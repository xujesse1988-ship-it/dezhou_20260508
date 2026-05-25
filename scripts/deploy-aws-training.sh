#!/usr/bin/env bash
#
# scripts/deploy-aws-training.sh
#
# 在一台全新的 AWS box 上从零部署并启动 NLHE CFR / MCCFR 训练。
# **从本地机器运行**（本地需能 ssh 到 AWS 和 vultr）。一条命令把裸机变成
# 正在跑的训练机：装工具链 → 部署源码 → 拉 bucket → 编译 → 校验 → 后台启动。
#
# 为什么需要这个脚本（都是踩过的坑）：
#   1. 全新 AWS box 没有 cc/linker —— rustup 只 warn 不装，不装 build-essential
#      cargo build 直接失败。
#   2. toolchain pin 文件名是 rust-toolchain.toml（不是 rust-toolchain）。
#   3. v3 bucket table 不在 GitHub Release，只在 vultr，必须 relay 过来。
#   4. LCFR 不能 resume（period state 不存 checkpoint，resume 回退 vanilla）——
#      训练必须一个进程从头跑完，所以用 setsid nohup 完全脱离 ssh 会话。
#   5. --batch-per-worker CLI 默认 16，热路径优化要的是 128，必须显式传。
#
# 用法：
#   scripts/deploy-aws-training.sh --host 18.221.200.43 [options]
#
# 常用：
#   # 默认 = think 分支 + cafebabe bucket + 500M LCFR（复刻 100M baseline 参数）
#   scripts/deploy-aws-training.sh --host <ip>
#
#   # 只部署不启动（拿来手动跑别的实验）
#   scripts/deploy-aws-training.sh --host <ip> --no-launch
#
#   # 自定义 run
#   scripts/deploy-aws-training.sh --host <ip> --updates 200000000 \
#       --run-name run_lcfr_200m --lcfr-period 1000000
#
# 选项（默认值见下方）：
#   --host <ip>            AWS 公网 IP（必填）
#   --key <path>           AWS ssh 私钥（默认 ~/us-east-2.pem）
#   --user <name>          AWS 登录用户（默认 ubuntu）
#   --branch <name>        部署哪个分支的源码（默认 think = 优化分支）
#   --vultr <user@host>    bucket / 持久存储来源（默认 shaopeng@64.176.35.138）
#   --bucket-seed <seed>   bucket table seed（默认 cafebabe = canonical）
#   --updates <N>          训练总 update 数（默认 500000000）
#   --seed <0xHEX|N>       训练随机种子（默认 0x4e4c48455f48335f = baseline）
#   --lcfr-period <N>      LCFR period；0 = vanilla ES-MCCFR（默认 1000000）
#   --threads <N>          worker 线程；0 = 远端 nproc（默认 0）
#   --batch <N>            --batch-per-worker（默认 128 = 优化值）
#   --ckpt-every <N>       checkpoint 间隔 update（默认 100000000）
#   --report-every <N>     throughput 上报间隔（默认 10000000）
#   --keep-last <N>        保留最近几个 auto checkpoint（默认 6）
#   --run-name <name>      checkpoint 子目录名（默认 run_lcfr_<updates>）
#   --no-launch            只部署 + 编译，不启动训练
#
# 退出后用这些命令盯训练（脚本结束会打印）：
#   ssh -i <key> <user>@<host> 'tail -f ~/dezhou_20260508/artifacts/<run>/train.log'
#
set -euo pipefail

# ---------------------------------------------------------------------------
# 默认值
# ---------------------------------------------------------------------------
AWS_HOST=""
AWS_KEY="$HOME/us-east-2.pem"
AWS_USER="ubuntu"
BRANCH="think"
VULTR="shaopeng@64.176.35.138"
REMOTE_DIR="dezhou_20260508"
BUCKET_SEED="cafebabe"

UPDATES=500000000
TRAIN_SEED=0x4e4c48455f48335f
LCFR_PERIOD=1000000
THREADS=0
BATCH=128
CKPT_EVERY=100000000
REPORT_EVERY=10000000
KEEP_LAST=6
RUN_NAME=""
DO_LAUNCH=1

usage() { sed -n '2,60p' "$0"; exit "${1:-1}"; }

while [ "$#" -gt 0 ]; do
    case "$1" in
        --host)         AWS_HOST="$2"; shift 2;;
        --key)          AWS_KEY="$2"; shift 2;;
        --user)         AWS_USER="$2"; shift 2;;
        --branch)       BRANCH="$2"; shift 2;;
        --vultr)        VULTR="$2"; shift 2;;
        --bucket-seed)  BUCKET_SEED="$2"; shift 2;;
        --updates)      UPDATES="$2"; shift 2;;
        --seed)         TRAIN_SEED="$2"; shift 2;;
        --lcfr-period)  LCFR_PERIOD="$2"; shift 2;;
        --threads)      THREADS="$2"; shift 2;;
        --batch)        BATCH="$2"; shift 2;;
        --ckpt-every)   CKPT_EVERY="$2"; shift 2;;
        --report-every) REPORT_EVERY="$2"; shift 2;;
        --keep-last)    KEEP_LAST="$2"; shift 2;;
        --run-name)     RUN_NAME="$2"; shift 2;;
        --no-launch)    DO_LAUNCH=0; shift;;
        -h|--help)      usage 0;;
        *) echo "[deploy] unknown arg: $1" >&2; usage 1;;
    esac
done

[ -n "$AWS_HOST" ] || { echo "[deploy] --host 必填" >&2; usage 1; }
[ -f "$AWS_KEY" ]  || { echo "[deploy] ssh key 不存在: $AWS_KEY" >&2; exit 1; }
[ -z "$RUN_NAME" ] && RUN_NAME="run_lcfr_$(( UPDATES / 1000000 ))m"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUCKET_FILE="bucket_table_default_500_500_500_seed_${BUCKET_SEED}_schemav3.bin"
AWS="$AWS_USER@$AWS_HOST"
SSH=(ssh -i "$AWS_KEY" -o StrictHostKeyChecking=accept-new -o ConnectTimeout=20)
RD="$REMOTE_DIR"   # 远端 ~/dezhou_20260508

say() { echo "[deploy] $*"; }

# ---------------------------------------------------------------------------
# 0. 探测 box
# ---------------------------------------------------------------------------
say "目标 $AWS  分支 $BRANCH  bucket $BUCKET_SEED  run $RUN_NAME"
REMOTE_NPROC="$("${SSH[@]}" "$AWS" 'nproc')"
say "远端 nproc = $REMOTE_NPROC"
[ "$THREADS" -eq 0 ] && THREADS="$REMOTE_NPROC"

# ---------------------------------------------------------------------------
# 1. 系统依赖：b3sum + build-essential(cc) + rustup（toolchain 由 pin 自动装）
# ---------------------------------------------------------------------------
say "安装系统依赖 + rustup（已装则跳过）"
"${SSH[@]}" "$AWS" 'bash -se' <<'REMOTE'
set -euo pipefail
need_apt=0
command -v b3sum >/dev/null || need_apt=1
command -v cc    >/dev/null || need_apt=1
if [ "$need_apt" -eq 1 ]; then
    sudo apt-get update -qq
    sudo apt-get install -y -qq b3sum build-essential pkg-config >/dev/null
fi
if ! command -v rustup >/dev/null && [ ! -x "$HOME/.cargo/bin/rustup" ]; then
    # --default-toolchain none：别装 stable，仓库的 rust-toolchain.toml 会在
    # 首次 cargo 时自动拉 pin 的版本（1.95.0），省一次下载。
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain none --profile minimal
fi
. "$HOME/.cargo/env"
echo "[remote] b3sum=$(b3sum --version) cc=$(cc --version | head -1) rustup=$(rustup --version | head -1)"
REMOTE

# ---------------------------------------------------------------------------
# 2. 部署源码：git archive 当前分支 → 远端解包
#    （archive 只含 tracked 文件，artifacts/ 等 gitignore 内容不会被带过去，
#     正好——bucket 单独 relay。要求该分支已 commit 干净。）
# ---------------------------------------------------------------------------
say "部署 $BRANCH 源码到 $AWS:~/$RD"
git -C "$REPO_ROOT" rev-parse --verify "$BRANCH" >/dev/null \
    || { echo "[deploy] 本地无分支 $BRANCH" >&2; exit 1; }
DEPLOY_SHA="$(git -C "$REPO_ROOT" rev-parse --short "$BRANCH")"
git -C "$REPO_ROOT" archive --format=tar "$BRANCH" \
    | "${SSH[@]}" "$AWS" "mkdir -p ~/$RD && tar xf - -C ~/$RD"
say "部署完成 @ $BRANCH ($DEPLOY_SHA)"

# ---------------------------------------------------------------------------
# 3. 传 bucket table：vultr → AWS（本地中转一条 pipe，零跨机鉴权）。
#    幂等：远端已有且 b3sum 与 vultr sidecar 一致就跳过。
# ---------------------------------------------------------------------------
say "校验 / 传输 bucket: $BUCKET_FILE"
EXPECT_B3="$(ssh -o ConnectTimeout=20 "$VULTR" \
    "awk '{print \$1}' ~/$RD/artifacts/$BUCKET_FILE.b3sum")"
say "vultr sidecar whole-file b3sum = $EXPECT_B3"

REMOTE_B3="$("${SSH[@]}" "$AWS" \
    "test -f ~/$RD/artifacts/$BUCKET_FILE && b3sum ~/$RD/artifacts/$BUCKET_FILE | awk '{print \$1}' || true")"

if [ "$REMOTE_B3" = "$EXPECT_B3" ]; then
    say "bucket 已在远端且 b3sum 匹配，跳过传输"
else
    say "relay 中（~553MB，走本地）…"
    ssh -o ConnectTimeout=20 "$VULTR" "cat ~/$RD/artifacts/$BUCKET_FILE" \
        | "${SSH[@]}" "$AWS" "mkdir -p ~/$RD/artifacts && cat > ~/$RD/artifacts/$BUCKET_FILE"
    GOT_B3="$("${SSH[@]}" "$AWS" "b3sum ~/$RD/artifacts/$BUCKET_FILE | awk '{print \$1}'")"
    [ "$GOT_B3" = "$EXPECT_B3" ] \
        || { echo "[deploy] bucket b3sum mismatch! got=$GOT_B3 expect=$EXPECT_B3" >&2; exit 3; }
    say "bucket 传输完成，b3sum 校验通过"
fi

# ---------------------------------------------------------------------------
# 4. 编译 train_cfr（首次会自动拉 1.95.0 toolchain）
# ---------------------------------------------------------------------------
say "编译 train_cfr --release"
"${SSH[@]}" "$AWS" "cd ~/$RD && . \"\$HOME/.cargo/env\" && cargo build --release --bin train_cfr 2>&1 | tail -3"
"${SSH[@]}" "$AWS" "test -x ~/$RD/target/release/train_cfr" \
    || { echo "[deploy] train_cfr 编译产物缺失" >&2; exit 1; }
say "编译完成"

# ---------------------------------------------------------------------------
# 5. 启动训练（detached：setsid nohup，脱离 ssh 会话，断连不影响）
# ---------------------------------------------------------------------------
if [ "$DO_LAUNCH" -eq 0 ]; then
    say "--no-launch：部署完成，未启动训练。"
    echo
    echo "手动启动示例："
    echo "  ssh -i $AWS_KEY $AWS"
    echo "  cd ~/$RD && . ~/.cargo/env"
    echo "  ./target/release/train_cfr --game nlhe --trainer es-mccfr \\"
    echo "    --bucket-table artifacts/$BUCKET_FILE \\"
    echo "    --updates $UPDATES --seed $TRAIN_SEED --lcfr-period $LCFR_PERIOD \\"
    echo "    --threads $THREADS --batch-per-worker $BATCH \\"
    echo "    --checkpoint-dir artifacts/$RUN_NAME --checkpoint-every $CKPT_EVERY \\"
    echo "    --report-every $REPORT_EVERY --keep-last $KEEP_LAST"
    exit 0
fi

LCFR_ARG=""
[ "$LCFR_PERIOD" -gt 0 ] && LCFR_ARG="--lcfr-period $LCFR_PERIOD"

say "启动 $RUN_NAME：$UPDATES updates / threads=$THREADS / batch=$BATCH / lcfr=$LCFR_PERIOD"
"${SSH[@]}" "$AWS" "bash -se" <<REMOTE
set -euo pipefail
cd ~/$RD && . "\$HOME/.cargo/env"
mkdir -p artifacts/$RUN_NAME
if pgrep -f 'release/train_cfr' >/dev/null; then
    echo "[remote] 已有 train_cfr 在跑，拒绝重复启动" >&2; exit 1
fi
setsid nohup ./target/release/train_cfr --game nlhe --trainer es-mccfr \
    --bucket-table artifacts/$BUCKET_FILE \
    --updates $UPDATES --seed $TRAIN_SEED $LCFR_ARG \
    --threads $THREADS --batch-per-worker $BATCH \
    --checkpoint-dir artifacts/$RUN_NAME --checkpoint-every $CKPT_EVERY \
    --report-every $REPORT_EVERY --keep-last $KEEP_LAST \
    > artifacts/$RUN_NAME/train.log 2>&1 < /dev/null &
echo "[remote] launched PID \$!"
REMOTE

sleep 5
say "启动日志头部："
"${SSH[@]}" "$AWS" "head -20 ~/$RD/artifacts/$RUN_NAME/train.log"

echo
say "===== 监控命令 ====="
echo "  # 实时日志"
echo "  ssh -i $AWS_KEY $AWS 'tail -f ~/$RD/artifacts/$RUN_NAME/train.log'"
echo "  # 只看 throughput"
echo "  ssh -i $AWS_KEY $AWS 'grep throughput= ~/$RD/artifacts/$RUN_NAME/train.log'"
echo "  # 进程 + 内存"
echo "  ssh -i $AWS_KEY $AWS 'ps -o pid,rss,etime,cmd -C train_cfr'"
echo "  # checkpoint"
echo "  ssh -i $AWS_KEY $AWS 'ls -lh ~/$RD/artifacts/$RUN_NAME/'"
say "注意：LCFR 不能 resume，训练必须一个进程跑完；别 kill 后想 --resume 续 LCFR。"
