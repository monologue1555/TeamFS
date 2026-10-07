#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
mkdir -p -- "$TEAMFS_ROOT/artifacts"
timeout 120 python3 "$TEAMFS_ROOT/scripts/showcase.py" | tee "$TEAMFS_ROOT/artifacts/showcase-output.txt"
