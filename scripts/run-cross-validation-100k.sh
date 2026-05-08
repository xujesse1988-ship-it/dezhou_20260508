#!/usr/bin/env bash
# 规则与 PokerKit 100,000 手 cross-validation runner（D-085 / validation §7 / C-rev1
# carve-out 闭合）。
#
# 单进程串行 100k 手 ≈ 10–11h（python3 子进程 spawn ~0.4s/手，IO 主导）。本脚本
# 按 chunk 并行 N 个 cargo 进程，每进程跑 ceil(100000/N) 手 disjoint seed range，
# 把墙上时钟降到 ~ceil(11h/N)。N=8 ≈ 1.5 小时；N=16 ≈ 45 分钟。
#
# 用法：
#   .venv-pokerkit/bin/python -c "import pokerkit"   # 先确认 pokerkit 装好
#   ./scripts/run-cross-validation-100k.sh                 # 默认 N=8, total=100000
#   N=16 TOTAL=100000 ./scripts/run-cross-validation-100k.sh
#
# 输出：每个 chunk 的进度日志写到 target/xvalidate-100k/chunk-<i>.log；汇总到
# target/xvalidate-100k/summary.txt。任何 chunk 出现 diverged 或 panic → exit 1。
#
# PATH 必须含 PokerKit 0.4.14 的 python3：
#   export PATH="$PWD/.venv-pokerkit/bin:$PATH"
#
# C-rev1 carve-out 闭合后该脚本的实跑数据应当 commit 到 docs/ 或 closure note。

set -euo pipefail

cd "$(dirname "$0")/.."

N="${N:-8}"
TOTAL="${TOTAL:-100000}"
OUT_DIR="target/xvalidate-100k"
mkdir -p "$OUT_DIR"

if ! command -v python3 >/dev/null 2>&1; then
  echo "[xvalidate-100k] ERROR: python3 not in PATH" >&2
  exit 2
fi
if ! python3 -c "import pokerkit" 2>/dev/null; then
  echo "[xvalidate-100k] ERROR: pokerkit not importable via python3 in PATH." >&2
  echo "  Run:  export PATH=\"\$PWD/.venv-pokerkit/bin:\$PATH\"" >&2
  exit 2
fi

CHUNK="$(( (TOTAL + N - 1) / N ))"
echo "[xvalidate-100k] N=$N TOTAL=$TOTAL CHUNK=$CHUNK"

# 编译一次 release
cargo test --release --test cross_validation cross_validation_pokerkit_100k_random_hands \
  -- --ignored --list >/dev/null 2>&1 || true
cargo test --release --test cross_validation --no-run >/dev/null

PIDS=()
for i in $(seq 0 $((N - 1))); do
  OFFSET=$((i * CHUNK))
  THIS_TOTAL=$CHUNK
  REMAINING=$((TOTAL - OFFSET))
  if (( REMAINING <= 0 )); then break; fi
  if (( REMAINING < CHUNK )); then THIS_TOTAL=$REMAINING; fi

  LOG="$OUT_DIR/chunk-${i}.log"
  echo "[xvalidate-100k] chunk $i: seeds [$OFFSET, $((OFFSET + THIS_TOTAL))) → $LOG"
  XV_TOTAL=$THIS_TOTAL XV_OFFSET=$OFFSET cargo test --release --test cross_validation \
    cross_validation_pokerkit_100k_random_hands \
    -- --ignored --nocapture > "$LOG" 2>&1 &
  PIDS+=($!)
done

FAIL=0
for i in "${!PIDS[@]}"; do
  PID="${PIDS[$i]}"
  if wait "$PID"; then
    echo "[xvalidate-100k] chunk $i: ok"
  else
    echo "[xvalidate-100k] chunk $i: FAILED" >&2
    FAIL=1
  fi
done

SUMMARY="$OUT_DIR/summary.txt"
{
  echo "==== xvalidate-100k summary ===="
  echo "N=$N TOTAL=$TOTAL CHUNK=$CHUNK"
  echo "----"
  for i in $(seq 0 $((N - 1))); do
    LOG="$OUT_DIR/chunk-${i}.log"
    [[ -f "$LOG" ]] || continue
    LAST="$(grep -E "^\[xvalidate-100k\] final " "$LOG" | tail -1)"
    echo "chunk $i: $LAST"
  done
} > "$SUMMARY"

cat "$SUMMARY"

if (( FAIL )); then
  echo "[xvalidate-100k] one or more chunks failed; see $OUT_DIR/chunk-*.log" >&2
  exit 1
fi

echo "[xvalidate-100k] all $TOTAL hands matched; D-085 规则引擎侧 C2 通过门槛达成。"
