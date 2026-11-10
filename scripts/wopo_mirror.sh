#!/usr/bin/env bash
# 在本地（不是实盘机）同步 wopo 实盘机的牌局数据，并在本地起画像网页服务（opponent_analysis live），
# 不用 SSH 隧道。实盘机只读不写。
#
# 用法（在仓库根目录跑）：
#   scripts/wopo_mirror.sh <实盘机 IP> [--dest 目录] [--port 8780] [--interval 3]
#                          [--key ~/northeast-3.pem] [--db opponent_db_20261031] [--no-serve]
#   --dest      本地镜像目录（缺省 ~/wopo_mirror），里面是 artifacts/ 的同名子集。
#   --port      本地网页端口（缺省 8780），打开 http://127.0.0.1:<port>/。
#   --interval  同步间隔秒数（缺省 3）。
#   --db        画像库目录名（实盘机 artifacts/ 下，缺省 opponent_db_20261031），只在启动时同步一次。
#   --no-serve  只同步，不起网页服务。
# Ctrl-C 停止同步和网页服务。实盘机换 IP 后带新 IP 重跑即可（镜像目录接着用，只补差额）。
#
# 同步什么：每个 artifacts/wopo_ante200_*_live_*/ 的 wopo_hh.jsonl、wopo_play_events.jsonl、wopo_ranges.jsonl、
# driver.pid（rsync --inplace：文件编号不变，网页服务只读新增部分），加上画像库。每轮同时查实盘机上 driver 还在
# 不在跑，写进镜像目录的 driver.alive（网页「进程状态」用它；同步停了超过 30 秒显示「未知」）。
# 需要：本地 python3、rsync 3.x、ssh，能用 --key 登录 ubuntu@<IP>。
set -euo pipefail

usage() { sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }

IP=""
DEST="$HOME/wopo_mirror"
PORT=8780
INTERVAL=3
KEY="$HOME/northeast-3.pem"
DB=opponent_db_20261031
SERVE=1
while [ $# -gt 0 ]; do
  case "$1" in
    --dest) DEST=$2; shift 2 ;;
    --port) PORT=$2; shift 2 ;;
    --interval) INTERVAL=$2; shift 2 ;;
    --key) KEY=$2; shift 2 ;;
    --db) DB=$2; shift 2 ;;
    --no-serve) SERVE=0; shift ;;
    -h|--help) usage ;;
    -*) echo "不认识的参数：$1" >&2; usage ;;
    *) [ -z "$IP" ] || usage; IP=$1; shift ;;
  esac
done
[ -n "$IP" ] || usage
[[ "$PORT" =~ ^[0-9]+$ ]] || { echo "--port 要是数字：$PORT" >&2; exit 2; }
[[ "$INTERVAL" =~ ^[0-9]+$ ]] && [ "$INTERVAL" -ge 1 ] || { echo "--interval 要是正整数：$INTERVAL" >&2; exit 2; }
[[ "$DB" =~ ^[A-Za-z0-9_.-]+$ ]] || { echo "--db 只写目录名：$DB" >&2; exit 2; }
[ -f "$KEY" ] || { echo "找不到密钥：$KEY" >&2; exit 1; }
[ -d opponent_analysis ] || { echo "请在仓库根目录跑（要用 opponent_analysis/）" >&2; exit 1; }
command -v rsync >/dev/null || { echo "缺 rsync" >&2; exit 1; }
# macOS ships rsync 2.6.9 / openrsync; --inplace + filter rules here are only tested with 3.x.
RSYNC_VER=$(rsync --version 2>&1 | sed -n 1p)
[[ "$RSYNC_VER" =~ rsync\ +version\ +([3-9]|[1-9][0-9])\. ]] \
  || { echo "要 rsync 3.x（现在是：${RSYNC_VER}）；macOS 用 brew install rsync，并让它排在 PATH 前面" >&2; exit 1; }

REMOTE=ubuntu@$IP
SRC=dezhou_20260508/artifacts
A=$DEST/artifacts
mkdir -p "$A"
# Short fixed base: macOS $TMPDIR is long and a unix socket path must stay under 104 bytes.
CTL=$(mktemp -d /tmp/wm.XXXXXX)
# One shared SSH connection for every rsync / check (ControlMaster), closed on exit.
SSH=(ssh -i "$KEY" -o ControlMaster=auto -o "ControlPath=$CTL/%C" -o ControlPersist=120
     -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o ConnectTimeout=15 -o StrictHostKeyChecking=accept-new)
RSYNC=(rsync -rt --inplace -e "${SSH[*]}")

LIVE_PID=""
cleanup() {
  trap - EXIT INT TERM
  [ -n "$LIVE_PID" ] && kill "$LIVE_PID" 2>/dev/null && wait "$LIVE_PID" 2>/dev/null || true
  "${SSH[@]}" -O exit "$REMOTE" >/dev/null 2>&1 || true
  rm -rf "$CTL"
  echo "已停（镜像留在 ${DEST}）"
}
trap cleanup EXIT INT TERM

"${SSH[@]}" "$REMOTE" "test -d $SRC" || { echo "连不上 $REMOTE 或没有 ~/$SRC" >&2; exit 1; }

# Run-directory files only: the three logs the page reads plus driver.pid (start time of the session).
sync_runs() {
  "${RSYNC[@]}" --prune-empty-dirs \
    --include='/wopo_ante200_*_live_*/' \
    --include='/wopo_ante200_*_live_*/wopo_hh.jsonl' \
    --include='/wopo_ante200_*_live_*/wopo_play_events.jsonl' \
    --include='/wopo_ante200_*_live_*/wopo_ranges.jsonl' \
    --include='/wopo_ante200_*_live_*/driver.pid' \
    --exclude='*' "$REMOTE:$SRC/" "$A/"
}

# Remote driver liveness per run directory → driver.alive next to the mirrored driver.pid.
write_alive() {
  local out now dir pid alive
  out=$("${SSH[@]}" "$REMOTE" 'cd ~/'"$SRC"' && for d in wopo_ante200_*_live_*/; do
      p=$(cat "$d/driver.pid" 2>/dev/null) || continue
      a=false; grep -q wopo_play "/proc/$p/cmdline" 2>/dev/null && a=true
      echo "${d%/} $p $a"; done') || return 1
  now=$(date +%s)
  while read -r dir pid alive; do
    [ -d "$A/$dir" ] && [[ "$pid" =~ ^[0-9]+$ ]] || continue
    printf '{"pid": %s, "alive": %s, "checked": %s}\n' "$pid" "$alive" "$now" > "$A/$dir/.driver.alive.tmp"
    mv -f "$A/$dir/.driver.alive.tmp" "$A/$dir/driver.alive"
  done <<< "$out"
}

echo "== 同步画像库 $DB 和牌局目录（第一次是全量，之后只补差额）"
"${RSYNC[@]}" --exclude=write.lock "$REMOTE:$SRC/$DB/" "$A/$DB/"
sync_runs
write_alive
echo "   $(ls -d "$A"/wopo_ante200_*_live_*/ 2>/dev/null | wc -l) 个牌局目录，$(du -sh "$DEST" | cut -f1)"

if [ "$SERVE" = 1 ]; then
  python3 -m opponent_analysis live --db "$A/$DB" --platform wopo --port "$PORT" \
    --follow "$A/wopo_ante200_*_live_2026*/wopo_hh.jsonl" \
    --play-events "$A/wopo_ante200_*_live_*/wopo_play_events.jsonl" --hero hero \
    >> "$DEST/live.log" 2>&1 &
  LIVE_PID=$!
  echo "== 网页服务 pid ${LIVE_PID}（日志 ${DEST}/live.log），加载完打开 http://127.0.0.1:$PORT/"
fi

echo "== 每 ${INTERVAL} 秒同步一次，Ctrl-C 停"
fails=0
while true; do
  sleep "$INTERVAL"
  if [ -n "$LIVE_PID" ] && ! kill -0 "$LIVE_PID" 2>/dev/null; then
    echo "网页服务退出了，看 $DEST/live.log" >&2
    LIVE_PID=""
    exit 1
  fi
  if sync_runs >/dev/null 2>"$DEST/.sync_err" && write_alive 2>>"$DEST/.sync_err"; then
    [ "$fails" -gt 0 ] && echo "$(date '+%H:%M:%S') 同步恢复"
    fails=0
  else
    fails=$((fails + 1))
    [ "$fails" = 1 ] && echo "$(date '+%H:%M:%S') 同步失败（会一直重试）：$(tail -1 "$DEST/.sync_err")" >&2
  fi
done
