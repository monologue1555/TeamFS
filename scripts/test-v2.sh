#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
mkdir -p -- "$TEAMFS_ROOT/artifacts"
timeout 240 python3 "$TEAMFS_ROOT/tests/v2.py" | tee "$TEAMFS_ROOT/artifacts/v2-acceptance.txt"
