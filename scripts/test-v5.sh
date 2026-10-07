#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
timeout 180 python3 "$TEAMFS_ROOT/tests/v5.py" | tee "$TEAMFS_ROOT/artifacts/v5-acceptance.txt"
