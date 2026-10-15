#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
mkdir -p -- "$TEAMFS_ROOT/artifacts"
timeout 240 python3 "$TEAMFS_ROOT/tests/experiments.py" | tee "$TEAMFS_ROOT/artifacts/experiment-acceptance.txt"
