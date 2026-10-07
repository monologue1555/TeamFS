#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
timeout 300 python3 "$TEAMFS_ROOT/tests/performance.py" | tee "$TEAMFS_ROOT/artifacts/performance-v4.txt"
