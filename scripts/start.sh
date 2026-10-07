#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
if [[ "${1:-}" == --help || "${1:-}" == -h ]]; then
    echo '用法：bash scripts/start.sh [挂载点] [--store 目录 | --memory] [--auto-sync 秒数] [--capacity-mib MiB] [--max-file-mib MiB] [--cached-io] [--audit-log 路径 | --no-audit-file]'
    echo '默认：$HOME/teamfs-mount；显式同步或正常卸载保存；自动同步须显式开启。'
    echo '环境异常时运行 teamfs doctor；卸载后的存储可用 teamfs check 自检。'
    echo '示例：bash scripts/start.sh "$HOME/teamfs-auto" --auto-sync 30'
    exit 0
fi
mountpoint="${1:-$HOME/teamfs-mount}"
if [[ $# -gt 0 ]]; then shift; fi
mkdir -p -- "$mountpoint"
mountpoint="$(realpath -- "$mountpoint")"
if mountpoint -q -- "$mountpoint"; then echo "挂载点已经使用：$mountpoint" >&2; exit 1; fi
teamfs_build
exec "$TEAMFS_BIN" mount "$mountpoint" --trace "$@"
