#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
mkdir -p -- "$TEAMFS_ROOT/artifacts"
timeout 300 python3 "$TEAMFS_ROOT/tests/blocks.py" | tee "$TEAMFS_ROOT/artifacts/block-foundation.txt"
