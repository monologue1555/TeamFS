#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
mkdir -p -- "$TEAMFS_ROOT/artifacts"
timeout 180 python3 "$TEAMFS_ROOT/tests/member3.py" | tee "$TEAMFS_ROOT/artifacts/member3-acceptance.txt"
