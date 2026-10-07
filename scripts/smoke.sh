#!/usr/bin/env bash
# 自动挂载到本脚本独占的临时目录，不使用正在演示的挂载点。
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
mkdir -p -- "$TEAMFS_ROOT/artifacts"
testdir="$(mktemp -d "$HOME/teamfs-check-XXXXXX")"
mountpoint="$testdir/mnt"
mkdir -- "$mountpoint"
pid=''
cleanup() {
    if mountpoint -q -- "$mountpoint"; then teamfs_unmount "$mountpoint" || true; fi
    if [[ -n "$pid" ]]; then
        if kill -0 "$pid" 2>/dev/null; then kill "$pid" 2>/dev/null || true; fi
        wait "$pid" 2>/dev/null || true
    fi
    # 只删除明确创建的两个空目录；卸载失败时不做递归删除。
    if ! mountpoint -q -- "$mountpoint"; then rmdir -- "$mountpoint" "$testdir" 2>/dev/null || true; fi
}
trap cleanup EXIT
mount_fs() {
    "$TEAMFS_BIN" mount "$mountpoint" --memory --trace >> "$TEAMFS_ROOT/artifacts/fuse-trace.log" 2>&1 &
    pid=$!
    for _ in $(seq 1 100); do
        if mountpoint -q -- "$mountpoint"; then return; fi
        if ! kill -0 "$pid" 2>/dev/null; then cat "$TEAMFS_ROOT/artifacts/fuse-trace.log" >&2; return 1; fi
        sleep 0.1
    done
    echo '挂载超时' >&2
    return 1
}
unmount_fs() {
    teamfs_unmount "$mountpoint"
    # 正常卸载应让 fuse::mount 返回并使进程退出。
    for _ in $(seq 1 100); do
        if ! kill -0 "$pid" 2>/dev/null; then wait "$pid"; pid=''; return; fi
        sleep 0.1
    done
    echo '卸载后进程没有及时退出' >&2
    return 1
}
: > "$TEAMFS_ROOT/artifacts/fuse-trace.log"
{
    echo 'TeamFS 实际验收'
    date -Iseconds
    uname -r
    rustc --version
    cargo --version
    printf 'libfuse: '; pkg-config --modversion fuse
    id
    mount_fs
    findmnt -n -o TARGET,FSTYPE --mountpoint "$mountpoint"
    timeout 60 python3 "$TEAMFS_ROOT/tests/mounted.py" "$mountpoint"
    unmount_fs
    mount_fs
    timeout 15 python3 "$TEAMFS_ROOT/tests/mounted.py" "$mountpoint" --reset
    unmount_fs
    echo 'ALL PASS — 已完成真实挂载、读写、卸载和重挂载，测试挂载已清理。'
} > >(tee "$TEAMFS_ROOT/artifacts/acceptance.txt")
