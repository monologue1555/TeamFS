#!/usr/bin/env bash
# 访问记录与进程归属验收：真实挂载上把 --trace 回调与实验报告逐项对账。
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
mkdir -p -- "$TEAMFS_ROOT/artifacts"
timeout 300 python3 "$TEAMFS_ROOT/tests/attribution.py" | tee "$TEAMFS_ROOT/artifacts/attribution-acceptance.txt"
