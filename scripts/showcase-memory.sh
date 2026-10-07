#!/usr/bin/env bash
# 一条命令走完真实演示；复用 demo.sh，独占临时挂载点，退出时卸载。
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
teamfs_build
mkdir -p -- "$TEAMFS_ROOT/artifacts"
trace="$TEAMFS_ROOT/artifacts/memory-showcase-trace.log"
output="$TEAMFS_ROOT/artifacts/memory-showcase-output.txt"
testdir="$(mktemp -d "$HOME/teamfs-showcase-XXXXXX")"
mountpoint="$testdir/mnt"
pid=''
mkdir -- "$mountpoint"

cleanup() {
    local status=$?
    trap - EXIT
    if mountpoint -q -- "$mountpoint"; then
        teamfs_unmount "$mountpoint" || status=1
    fi
    if [[ -n "$pid" ]]; then
        if kill -0 "$pid" 2>/dev/null; then kill "$pid" 2>/dev/null || true; fi
        wait "$pid" 2>/dev/null || true
    fi
    # 只移除本脚本创建的空目录；不递归删除挂载点。
    if ! mountpoint -q -- "$mountpoint"; then
        rmdir -- "$mountpoint" "$testdir" || status=1
    fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mount_fs() {
    "$TEAMFS_BIN" mount "$mountpoint" --memory --trace >> "$trace" 2>&1 &
    pid=$!
    for _ in $(seq 1 100); do
        if mountpoint -q -- "$mountpoint"; then teamfs_check_mount "$mountpoint"; return; fi
        if ! kill -0 "$pid" 2>/dev/null; then cat "$trace" >&2; return 1; fi
        sleep 0.1
    done
    echo '挂载超时，请查看 showcase-trace.log。' >&2
    return 1
}

unmount_fs() {
    teamfs_unmount "$mountpoint"
    for _ in $(seq 1 100); do
        if ! kill -0 "$pid" 2>/dev/null; then
            wait "$pid"
            pid=''
            return
        fi
        sleep 0.1
    done
    echo '卸载后进程没有及时退出。' >&2
    return 1
}

: > "$trace"
exec > >(tee "$output") 2>&1
echo 'TeamFS：从 Rust 程序到真实文件系统'
date -Iseconds
printf '运行用户：'; id -un
echo '链路：普通命令 → Linux 内核 → FUSE → Rust 回调 → 内存模型 → 返回结果'
echo
echo '【第一幕】把 Rust 程序挂载成一个目录'
mount_fs
findmnt -n -o TARGET,FSTYPE --mountpoint "$mountpoint"
echo '下面的 ls、cat、写入、改名和删除，都操作这个真实挂载点。'
timeout 60 env TEAMFS_AUTO=1 TEAMFS_MANAGED=1 bash "$TEAMFS_ROOT/scripts/demo.sh" "$mountpoint"

echo
echo '【第二幕】看看 Rust 真正收到了什么请求'
echo '以下按发生顺序，每种关键回调摘录一条；完整顺序和全部请求见日志。'
awk '$2 ~ /^(readdir|lookup|open|read|mkdir|create|write|setattr|rename|unlink|rmdir)$/ && !seen[$2]++ {print}' "$trace"
echo 'inode 是节点身份；fh 是打开句柄；offset 与 bytes 按字节计数。'
echo 'setattr 的 size=Some(0) 表示覆盖写入前清空原内容。'

echo
echo '【第三幕】卸载后重新启动，验证数据只在内存里'
echo '卸载前，根目录中存在刚刚创建的 session-*.txt：'
ls -li -- "$mountpoint"
unmount_fs
echo '旧进程已经退出。现在重新挂载一个全新的 MemFs。'
mount_fs
timeout 15 python3 "$TEAMFS_ROOT/tests/mounted.py" "$mountpoint" --reset
ls -li -- "$mountpoint"
unmount_fs
echo
echo '演示通过：普通工具读写成功，真实回调已记录，重挂载恢复初始内容。'
echo '演示挂载已卸载；临时空目录将在脚本退出时清理。'
printf '演示记录：%s\n完整回调：%s\n' "$output" "$trace"
