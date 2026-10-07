#!/usr/bin/env bash
# 在 Ubuntu / WSL 内执行；不替换已经安装的 FUSE3。
set -euo pipefail
sudo apt-get update
sudo apt-get install -y rustc cargo libfuse-dev libfuse2t64 pkg-config build-essential python3
pkg-config --modversion fuse
test -c /dev/fuse || { echo '没有 /dev/fuse，请使用支持 FUSE 的 Linux / WSL2。' >&2; exit 1; }
echo '环境就绪。运行 bash scripts/start.sh'
