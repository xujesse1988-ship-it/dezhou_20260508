#!/usr/bin/env bash
#
# Rsync the canonical 1000/1000/1000 bucket table to the Vultr user home.
#
# Usage:
#   scripts/rsync-bucket-to-vultr.sh [options]
#
# Options:
#   --source <path>       Local bucket file path.
#   --remote <user@host>  Remote SSH target.
#   --dest <path>         Remote destination directory.
#   --dry-run            Show what would be transferred.
#   -h, --help           Show this help.

set -euo pipefail

SOURCE="artifacts/bucket_table_default_1000_1000_1000_seed_cafebabe_schemav4.bin"
REMOTE="shaopeng@64.176.35.138"
DEST="~/"
DRY_RUN=0

usage() { sed -n '2,14p' "$0"; exit "${1:-1}"; }

while [ "$#" -gt 0 ]; do
    case "$1" in
        --source)
            SOURCE="$2"
            shift 2
            ;;
        --remote)
            REMOTE="$2"
            shift 2
            ;;
        --dest)
            DEST="$2"
            shift 2
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        -h|--help)
            usage 0
            ;;
        *)
            echo "[rsync-bucket] unknown arg: $1" >&2
            usage 1
            ;;
    esac
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SOURCE_PATH="$SOURCE"
if [[ "$SOURCE_PATH" != /* ]]; then
    SOURCE_PATH="$REPO_ROOT/$SOURCE_PATH"
fi

[ -f "$SOURCE_PATH" ] || {
    echo "[rsync-bucket] source file not found: $SOURCE_PATH" >&2
    exit 1
}

RSYNC_ARGS=(-avh --progress --partial)
[ "$DRY_RUN" -eq 1 ] && RSYNC_ARGS+=(--dry-run)

echo "[rsync-bucket] source: $SOURCE_PATH"
echo "[rsync-bucket] target: $REMOTE:$DEST"
rsync "${RSYNC_ARGS[@]}" "$SOURCE_PATH" "$REMOTE:$DEST"
