#!/usr/bin/env bash
set -euo pipefail
TEAMFS_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# 源码和交付文件在 Teamwork；编译缓存放在 WSL Linux 磁盘，提高构建速度。
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/teamfs/target}"
TEAMFS_BIN="$CARGO_TARGET_DIR/debug/teamfs"
teamfs_build() { (cd -- "$TEAMFS_ROOT" && cargo build --locked); }
teamfs_unmount() {
    if command -v fusermount3 >/dev/null; then fusermount3 -u -- "$1";
    else fusermount -u -- "$1"; fi
}
teamfs_check_mount() {
    local found
    found="$(findmnt -n -o FSTYPE --mountpoint "$1" || true)"
    [[ "$found" == fuse.teamfs ]] || { echo "该目录不是 TeamFS 挂载点：$1" >&2; return 1; }
}
