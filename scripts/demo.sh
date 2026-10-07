#!/usr/bin/env bash
# 在第二个 WSL 终端运行；只操作本脚本创建的唯一目录。
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
mountpoint="${1:-$HOME/teamfs-mount}"
teamfs_check_mount "$mountpoint"
folder=''
note=''
cleanup() {
    if [[ -n "${note:-}" && -f "$note" ]]; then rm -- "$note"; fi
    if [[ -n "${folder:-}" && -d "$folder" ]]; then rmdir -- "$folder"; fi
}
trap cleanup EXIT
step() {
    echo
    echo "$1"
    if [[ -t 0 && "${TEAMFS_AUTO:-0}" != 1 ]]; then read -r -p '按 Enter 执行，随后查看挂载终端的回调日志… ' _; fi
}
step '1. ls / cat：列出根目录，读取预置欢迎文本'
ls -li -- "$mountpoint"
cat -- "$mountpoint/welcome.txt"
step '2. mkdir / create / write：创建演示目录，写入会议笔记'
folder="$(mktemp -d "$mountpoint/demo-XXXXXX")"
note="$folder/meeting.txt"
echo "演示目录：$folder"
printf '第一次小组会议：先理解 FUSE。\n' > "$note"
cat -- "$note"
step '3. >>：追加内容；>：覆盖为更短的内容（触发截断）'
printf '分工：模型、目录、读写、测试、讲解。\n' >> "$note"
cat -- "$note"
printf '短笔记\n' > "$note"
cat -- "$note"
stat -c '大小=%s 字节；inode=%i；链接数=%h' -- "$note"
step '4. mv：名字改变，inode 保持不变'
mv -- "$note" "$folder/renamed.txt"
note="$folder/renamed.txt"
ls -li -- "$folder"
step '5. rm / rmdir：删除文件及空目录'
rm -- "$note"
note=''
rmdir -- "$folder"
folder=''
step '6. 留下一份笔记，观察当前运行模式的保存行为'
marker="$(mktemp "$mountpoint/session-XXXXXX.txt")"
printf 'persistent 正常卸载会保留这份笔记；memory 退出后清空。\n' > "$marker"
echo "临时文件：$marker"
if [[ "${TEAMFS_MANAGED:-0}" == 1 ]]; then
    echo '接下来由完整演示脚本自动卸载和重挂载，无需手动操作。'
else
    printf '现在运行 bash scripts/unmount.sh %q，再运行 bash scripts/start.sh %q。\n' "$mountpoint" "$mountpoint"
    echo 'persistent 模式重新挂载同一存储后笔记仍在；--memory 模式恢复预置内容。'
fi
