#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
mkdir -p -- "$TEAMFS_ROOT/artifacts"
timeout 240 python3 "$TEAMFS_ROOT/tests/v6.py" | tee "$TEAMFS_ROOT/artifacts/v6-acceptance.txt"
timeout 240 python3 "$TEAMFS_ROOT/tests/compatibility.py" | tee "$TEAMFS_ROOT/artifacts/compatibility-v6.txt"
timeout 300 python3 "$TEAMFS_ROOT/tests/randomized.py" | tee "$TEAMFS_ROOT/artifacts/randomized-v6.txt"
