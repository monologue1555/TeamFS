#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
mountpoint="${1:-$HOME/teamfs-mount}"
if [[ $# -gt 0 ]]; then shift; fi
mkdir -p -- "$mountpoint"
mountpoint="$(realpath -- "$mountpoint")"
if mountpoint -q -- "$mountpoint"; then echo "挂载点已经使用：$mountpoint" >&2; exit 1; fi
teamfs_build
exec "$TEAMFS_BIN" mount "$mountpoint" --trace "$@"
