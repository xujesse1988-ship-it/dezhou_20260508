#!/bin/bash
# §G-batch1 §3.9 v3 retrain bark watcher（远端跑在 AWS 32-core c6a.8xlarge）。
# tail train_v3.log，关键节点推送 iOS bark；训练完成或异常终止时也推。
#
# 用法（AWS 上）：
#   nohup bash ~/dezhou_20260508/tools/v3_bark_watcher.sh > ~/dezhou_20260508/artifacts/bark_watcher.log 2>&1 &
set -u

LOG="${LOG:-$HOME/dezhou_20260508/artifacts/train_v3.log}"
TRAIN_PID="${TRAIN_PID:-4849}"
BARK_BASE="${BARK_BASE:-https://api.day.app/VUV6EH7CfzgvWLVvYUYamU}"

notify() {
    local title="$1"
    local body="$2"
    curl -fsS "$BARK_BASE" \
        --data-urlencode "title=$title" \
        --data-urlencode "body=$body" \
        --data-urlencode "group=v3retrain" \
        >/dev/null 2>&1 \
        || echo "[bark] curl failed for: $title" >&2
    echo "[$(date '+%F %T')] notified: $title | $body"
}

notify "v3 retrain watcher started" "PID=$TRAIN_PID host=$(hostname)"

# Tail log in subshell；逐行 match 关键 milestone。
# 用 -n +1 让从头跑（已经写出来的行也算）；-F 让 tail 跟随 file rotation。
(
    tail -n +1 -F "$LOG" 2>/dev/null | while IFS= read -r line; do
        case "$line" in
            *"street=Flop sampled"*|*"street=Flop"*"phase=1 features done"*) ;;
            *"street=Flop features done"*)
                notify "v3 Flop features done" "$line" ;;
            *"street=Flop kmeans done"*)
                notify "v3 Flop kmeans done" "$line" ;;
            *"street=Flop mode=Production total wall"*)
                notify "v3 Flop ✓ STREET DONE" "$line" ;;
            *"street=Turn features done"*)
                notify "v3 Turn features done" "$line" ;;
            *"street=Turn kmeans done"*)
                notify "v3 Turn kmeans done" "$line" ;;
            *"street=Turn mode=Production total wall"*)
                notify "v3 Turn ✓ STREET DONE" "$line" ;;
            *"street=River features done"*)
                notify "v3 River features done" "$line" ;;
            *"street=River kmeans done"*)
                notify "v3 River kmeans done" "$line" ;;
            *"street=River mode=Production total wall"*)
                notify "v3 River ✓ STREET DONE" "$line" ;;
            *"training complete in"*)
                notify "v3 TRAINING COMPLETE" "$line" ;;
            *"BLAKE3="*)
                notify "v3 ✅ ARTIFACT WROTE" "$line" ;;
        esac
    done
) &
TAILER_PID=$!

# Foreground 监控 train pid。
while kill -0 "$TRAIN_PID" 2>/dev/null; do
    sleep 60
done

# Train 进程结束 → 等 tail 漏掉的最后几行 drain，再退出。
sleep 15
kill "$TAILER_PID" 2>/dev/null

# 如果 log 末尾没 "training complete" → 异常退出。
if tail -n 30 "$LOG" 2>/dev/null | grep -q "training complete in"; then
    notify "v3 watcher exiting cleanly" "training complete already notified"
else
    notify "v3 ❌ PROCESS DIED" "PID $TRAIN_PID exited without 'training complete in'; tail log"
fi
