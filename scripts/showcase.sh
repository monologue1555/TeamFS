#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
mkdir -p -- "$TEAMFS_ROOT/artifacts"
timeout 180 python3 "$TEAMFS_ROOT/scripts/showcase-v3.py" | tee "$TEAMFS_ROOT/artifacts/showcase-output.txt"
timeout 180 python3 "$TEAMFS_ROOT/scripts/showcase-v4.py" | tee "$TEAMFS_ROOT/artifacts/showcase-v4-output.txt"
