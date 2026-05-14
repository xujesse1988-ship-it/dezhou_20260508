#!/usr/bin/env bash
# 跨 host checkpoint BLAKE3 baseline capture（stage 3 F1 / D-347 / D-362 /
# 继承 stage 1 cross_arch_hash + stage 2 bucket-table-arch-hashes 模式）。
#
# 用法：
#   ./scripts/capture-checkpoint-hashes.sh             # 跑当前 (os, arch) 写到 baseline
#   ./scripts/capture-checkpoint-hashes.sh --diff      # 跑当前 host，并与 linux-x86_64 baseline 比较
#
# 把 32 个固定 seed 的 5-iter Kuhn checkpoint 文件 BLAKE3 dump 到
# tests/data/checkpoint-hashes-<os>-<arch>.txt，commit 后 D-347 跨 host 验证由
# `cargo test --test cross_host_blake3 cross_host_baseline_byte_equal_for_current_arch`
# 自动执行。
#
# 在 macOS arm64 / Linux aarch64 上首次跑后请把生成的 baseline 文件提交进仓库。

set -euo pipefail

cd "$(dirname "$0")/.."

UNAME_S="$(uname -s)"
UNAME_M="$(uname -m)"

case "$UNAME_S" in
  Linux)  OS_TAG="linux" ;;
  Darwin) OS_TAG="darwin" ;;
  *) echo "unsupported OS: $UNAME_S" >&2; exit 2 ;;
esac

case "$UNAME_M" in
  x86_64)         ARCH_TAG="x86_64" ;;
  arm64|aarch64)  ARCH_TAG="aarch64" ;;
  *) echo "unsupported arch: $UNAME_M" >&2; exit 2 ;;
esac

BASELINE_DIR="tests/data"
BASELINE_FILE="${BASELINE_DIR}/checkpoint-hashes-${OS_TAG}-${ARCH_TAG}.txt"
mkdir -p "$BASELINE_DIR"

echo "[capture-checkpoint] OS=$OS_TAG ARCH=$ARCH_TAG → $BASELINE_FILE"

cargo test --test cross_host_blake3 --no-run >/dev/null

OUTPUT="$(cargo test --test cross_host_blake3 cross_host_capture_only -- \
  --ignored --nocapture 2>&1 | grep -oE 'seed=[0-9]+ hash=[0-9a-f]+')"

if [[ -z "$OUTPUT" ]]; then
  echo "[capture-checkpoint] ERROR: no seed lines in test output" >&2
  exit 1
fi

LINE_COUNT="$(printf '%s\n' "$OUTPUT" | wc -l | tr -d ' ')"
if [[ "$LINE_COUNT" != "32" ]]; then
  echo "[capture-checkpoint] ERROR: expected 32 seed lines, got $LINE_COUNT" >&2
  exit 1
fi

printf '%s\n' "$OUTPUT" > "$BASELINE_FILE"
echo "[capture-checkpoint] wrote $LINE_COUNT lines → $BASELINE_FILE"

if [[ "${1:-}" == "--diff" ]]; then
  REF="tests/data/checkpoint-hashes-linux-x86_64.txt"
  if [[ -f "$REF" && "$BASELINE_FILE" != "$REF" ]]; then
    echo "[capture-checkpoint] diff against $REF:"
    if diff -u "$REF" "$BASELINE_FILE"; then
      echo "[capture-checkpoint] D-347: ${OS_TAG}-${ARCH_TAG} 与 linux-x86_64 baseline byte-equal ✓"
    else
      echo "[capture-checkpoint] D-347 / D-052 carve-out: 跨架构 checkpoint 哈希不一致（aspirational）"
      echo "  这本身不是 stage-3 失败；F3 验收报告应显式记录该状态。"
      exit 0
    fi
  fi
fi
