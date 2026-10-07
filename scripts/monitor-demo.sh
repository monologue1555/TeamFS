#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
exec python3 "$TEAMFS_ROOT/scripts/monitor-demo.py" "$@"
