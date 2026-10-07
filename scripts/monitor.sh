#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
mountpoint="${1:-$HOME/teamfs-mount}"
if [[ $# -gt 0 ]]; then shift; fi
exec python3 "$TEAMFS_ROOT/scripts/monitor.py" --mount "$mountpoint" "$@"
