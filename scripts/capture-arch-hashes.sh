#!/usr/bin/env bash
# 跨架构 hash baseline capture（D1 / D-052 / validation §6）。
#
# 用法：
#   ./scripts/capture-arch-hashes.sh              # 跑当前 (os, arch) 并写到 baseline 文件
#   ./scripts/capture-arch-hashes.sh --diff       # 跑当前 host，并与 Linux x86_64 baseline 比较
#
# 脚本会把 32 个固定 seed 的 hand history content_hash dump 到
# tests/data/arch-hashes-<os>-<arch>.txt，commit 后 D-052 跨架构验证就能由
# `cargo test --release --test cross_arch_hash cross_arch_hash_matches_baseline`
# 自动执行。
#
# 在 macOS arm64 / Linux aarch64 上首次跑后，请把生成的 baseline 文件提交进仓库。

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
BASELINE_FILE="${BASELINE_DIR}/arch-hashes-${OS_TAG}-${ARCH_TAG}.txt"
mkdir -p "$BASELINE_DIR"

echo "[capture-arch] OS=$OS_TAG ARCH=$ARCH_TAG → $BASELINE_FILE"

# 编译 release 一次，避免 cargo test 二次 spawn
cargo test --release --test cross_arch_hash --no-run >/dev/null

OUTPUT="$(cargo test --release --test cross_arch_hash cross_arch_hash_capture_only -- \
  --ignored --nocapture 2>&1 | grep -oE 'seed=[0-9]+ hash=[0-9a-f]+')"

if [[ -z "$OUTPUT" ]]; then
  echo "[capture-arch] ERROR: no seed lines in test output" >&2
  exit 1
fi

LINE_COUNT="$(printf '%s\n' "$OUTPUT" | wc -l | tr -d ' ')"
if [[ "$LINE_COUNT" != "32" ]]; then
  echo "[capture-arch] ERROR: expected 32 seed lines, got $LINE_COUNT" >&2
  exit 1
fi

printf '%s\n' "$OUTPUT" > "$BASELINE_FILE"
echo "[capture-arch] wrote $LINE_COUNT lines → $BASELINE_FILE"

if [[ "${1:-}" == "--diff" ]]; then
  REF="tests/data/arch-hashes-linux-x86_64.txt"
  if [[ -f "$REF" && "$BASELINE_FILE" != "$REF" ]]; then
    echo "[capture-arch] diff against $REF:"
    if diff -u "$REF" "$BASELINE_FILE"; then
      echo "[capture-arch] D-052: ${OS_TAG}-${ARCH_TAG} 与 linux-x86_64 baseline byte-equal ✓"
    else
      echo "[capture-arch] D-052: 跨架构哈希不一致（D-052 期望目标未达成）。"
      echo "  这本身不是 stage-1 失败（D-051 才是必过门槛）；F3 验收报告应显式记录该状态。"
      exit 0  # 退出 0：D-052 不达成不算 hard fail
    fi
  fi
fi
