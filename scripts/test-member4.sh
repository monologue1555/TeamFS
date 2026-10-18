#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
mkdir -p -- "$TEAMFS_ROOT/artifacts"
timeout 300 python3 "$TEAMFS_ROOT/tests/member4.py" | tee "$TEAMFS_ROOT/artifacts/member4-acceptance.txt"
