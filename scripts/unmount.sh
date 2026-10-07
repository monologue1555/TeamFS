#!/usr/bin/env bash
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
mountpoint="${1:-$HOME/teamfs-mount}"
teamfs_check_mount "$mountpoint"
"$TEAMFS_BIN" sync "$mountpoint"
teamfs_unmount "$mountpoint"
echo '已同步并卸载 TeamFS。persistent 模式的数据保留；memory 模式数据随进程退出释放。'
