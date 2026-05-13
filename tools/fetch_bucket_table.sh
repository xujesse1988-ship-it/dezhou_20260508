#!/usr/bin/env bash
# tools/fetch_bucket_table.sh
#
# Download + BLAKE3-verify a bucket table artifact from GitHub Releases。
# 与 stage 2 §G-batch1 §3.4-batch2 D-218-rev2 真等价类 artifact (v2 schema)
# 配套使用：artifact 体积 ~528 MB，不进 git history，按需从 Release 拉取并
# 校验 BLAKE3 后放入 `artifacts/`。
#
# 用法：
#   tools/fetch_bucket_table.sh
#       # 默认拉 stage2-v1.1 tag 下的 v2 artifact，cache 到
#       # artifacts/bucket_table_default_500_500_500_seed_cafebabe_v2.bin
#
#   tools/fetch_bucket_table.sh --tag stage2-v1.1 \
#                               --artifact bucket_table_default_500_500_500_seed_cafebabe_v2.bin \
#                               --expected-blake3 <whole-file b3sum>
#
#   tools/fetch_bucket_table.sh --force        # 重新下载（即使本地已存在）
#
# 退出码：
#   0  成功（artifact 在 artifacts/ 下且 BLAKE3 与 expected 匹配）
#   1  参数错误
#   2  下载失败（curl 非 0 / GitHub 404）
#   3  BLAKE3 hash mismatch（artifact 已被改动 / 上传错文件）
#
# 依赖：curl + b3sum（`sudo apt install b3sum` / `cargo install b3sum`）

set -euo pipefail

# ----------------------------------------------------------------------------
# 默认值（与 §G-batch1 §3.4-batch2 production retrain 字面对齐）
# ----------------------------------------------------------------------------
REPO_DEFAULT="xujesse1988-ship-it/dezhou_20260508"
TAG_DEFAULT="stage2-v1.1"
ARTIFACT_DEFAULT="bucket_table_default_500_500_500_seed_cafebabe_v2.bin"
EXPECTED_BLAKE3_DEFAULT="211319ff86686a5734eb6952d92ff664c9dc230cd28506a732b97012b44535db"  # §G-batch1 §3.4-batch2 production retrain 出口 whole-file b3sum (2026-05-13)

REPO="$REPO_DEFAULT"
TAG="$TAG_DEFAULT"
ARTIFACT="$ARTIFACT_DEFAULT"
EXPECTED_BLAKE3="$EXPECTED_BLAKE3_DEFAULT"
FORCE=0

usage() {
    sed -n '2,30p' "$0"
    exit "${1:-1}"
}

# ----------------------------------------------------------------------------
# CLI 参数解析
# ----------------------------------------------------------------------------
while [ "$#" -gt 0 ]; do
    case "$1" in
        --repo)             REPO="$2"; shift 2;;
        --tag)              TAG="$2"; shift 2;;
        --artifact)         ARTIFACT="$2"; shift 2;;
        --expected-blake3)  EXPECTED_BLAKE3="$2"; shift 2;;
        --force)            FORCE=1; shift;;
        -h|--help)          usage 0;;
        *) echo "[fetch_bucket_table] unknown arg: $1" >&2; usage 1;;
    esac
done

# ----------------------------------------------------------------------------
# 依赖检查
# ----------------------------------------------------------------------------
for cmd in curl b3sum; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "[fetch_bucket_table] missing dependency: $cmd" >&2
        echo "[fetch_bucket_table] install: sudo apt install $cmd OR cargo install $cmd" >&2
        exit 1
    fi
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ARTIFACTS_DIR="$REPO_ROOT/artifacts"
TARGET_PATH="$ARTIFACTS_DIR/$ARTIFACT"
URL="https://github.com/$REPO/releases/download/$TAG/$ARTIFACT"

mkdir -p "$ARTIFACTS_DIR"

# ----------------------------------------------------------------------------
# 本地 cache 检查
# ----------------------------------------------------------------------------
if [ "$FORCE" -eq 0 ] && [ -f "$TARGET_PATH" ]; then
    echo "[fetch_bucket_table] cache hit: $TARGET_PATH"
    actual=$(b3sum "$TARGET_PATH" | awk '{print $1}')
    if [ -n "$EXPECTED_BLAKE3" ]; then
        if [ "$actual" = "$EXPECTED_BLAKE3" ]; then
            echo "[fetch_bucket_table] BLAKE3 match: $actual"
            exit 0
        else
            echo "[fetch_bucket_table] cache BLAKE3 mismatch:" >&2
            echo "[fetch_bucket_table]   expected: $EXPECTED_BLAKE3" >&2
            echo "[fetch_bucket_table]   actual:   $actual" >&2
            echo "[fetch_bucket_table] re-downloading (use --force to bypass cache)" >&2
        fi
    else
        echo "[fetch_bucket_table] no --expected-blake3 supplied; skipping verify (hash=$actual)"
        exit 0
    fi
fi

# ----------------------------------------------------------------------------
# 下载（GitHub Release public asset，curl follow-redirects）
# ----------------------------------------------------------------------------
echo "[fetch_bucket_table] downloading $URL"
echo "[fetch_bucket_table]   -> $TARGET_PATH"
tmp_path="${TARGET_PATH}.partial"
if ! curl --fail --location --show-error --silent --output "$tmp_path" "$URL"; then
    echo "[fetch_bucket_table] download failed: $URL" >&2
    rm -f "$tmp_path"
    exit 2
fi

# ----------------------------------------------------------------------------
# BLAKE3 校验
# ----------------------------------------------------------------------------
actual=$(b3sum "$tmp_path" | awk '{print $1}')
size_bytes=$(stat -c '%s' "$tmp_path" 2>/dev/null || stat -f '%z' "$tmp_path")
echo "[fetch_bucket_table] downloaded $size_bytes bytes / BLAKE3=$actual"

if [ -n "$EXPECTED_BLAKE3" ] && [ "$actual" != "$EXPECTED_BLAKE3" ]; then
    echo "[fetch_bucket_table] BLAKE3 mismatch!" >&2
    echo "[fetch_bucket_table]   expected: $EXPECTED_BLAKE3" >&2
    echo "[fetch_bucket_table]   actual:   $actual" >&2
    echo "[fetch_bucket_table] keeping partial as $tmp_path for inspection" >&2
    exit 3
fi

# atomic move into place (与 BucketTable::write_to_path .tmp + rename 同形态)
mv "$tmp_path" "$TARGET_PATH"
echo "[fetch_bucket_table] success: $TARGET_PATH (BLAKE3=$actual)"
exit 0
