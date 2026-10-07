#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
mkdir -p -- "$TEAMFS_ROOT/artifacts"
timeout 600 python3 "$TEAMFS_ROOT/tests/baseline.py" "$@" | tee "$TEAMFS_ROOT/artifacts/baseline-v6.txt"
