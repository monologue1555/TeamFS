"""通过真实 Linux 文件 API 验收 FUSE；不是模型的替身。"""
import errno
import os
from pathlib import Path
import stat
import sys

mount = Path(sys.argv[1])
checks = 0


def passed(message):
    global checks
    checks += 1
    print(f"PASS {checks:02d}: {message}", flush=True)


def fails(expected, operation):
    try:
        operation()
    except OSError as exc:
        assert exc.errno == expected, (exc.errno, expected, str(exc))
    else:
        raise AssertionError(f"expected errno {expected}")


if len(sys.argv) > 2 and sys.argv[2] == "--reset":
    assert sorted(os.listdir(mount)) == [".teamfs", "notes", "welcome.txt"]
    assert (mount / "welcome.txt").read_text().startswith("欢迎来到 TeamFS")
    assert not (mount / "session-proof.txt").exists()
    assert sorted(os.listdir(mount / "notes")) == ["example.txt"]
    passed("卸载 / 重挂载：临时文件消失，预置内容恢复")
    sys.exit(0)

assert sorted(os.listdir(mount)) == [".teamfs", "notes", "welcome.txt"]
assert (mount / "welcome.txt").read_text().startswith("欢迎来到 TeamFS")
assert "小组笔记示例" in (mount / "notes/example.txt").read_text()
passed("真实挂载与预置中文文件")

folder = mount / "验收目录"
folder.mkdir()
note = folder / "会议笔记.txt"
note.write_text("第一次会议\n", encoding="utf-8")
assert note.read_text() == "第一次会议\n"
assert note.stat().st_size == len("第一次会议\n".encode())
passed("mkdir / create / write / read：中文名称、内容与字节大小")

with note.open("a", encoding="utf-8") as f:
    f.write("第二条\n")
assert note.read_text() == "第一次会议\n第二条\n"
note.write_bytes(b"xy")
assert note.read_bytes() == b"xy" and note.stat().st_size == 2
passed("追加与覆盖截断：更短的内容没有残留尾部")

fd = os.open(note, os.O_RDWR)
try:
    assert os.pwrite(fd, b"!", 5) == 1
    assert os.pread(fd, 99, 0) == b"xy\0\0\0!"
    assert os.pread(fd, 1, 1) == b"y"
    assert os.pread(fd, 99, 999) == b""
    os.ftruncate(fd, 3)
    assert os.pread(fd, 99, 0) == b"xy\0"
    os.ftruncate(fd, 6)
    assert os.pread(fd, 99, 0) == b"xy\0\0\0\0"
    os.fsync(fd)
finally:
    os.close(fd)
passed("pread / pwrite / ftruncate：偏移、EOF、补零及内存同步")

target = folder / "target.txt"
target.write_bytes(b"old target")
old = os.open(target, os.O_RDONLY)
ino = note.stat().st_ino
try:
    os.rename(note, target)
    assert not note.exists() and target.stat().st_ino == ino
    assert os.pread(old, 99, 0) == b"old target"
finally:
    os.close(old)
passed("rename 覆盖目标：源 inode 保留，旧目标的已打开句柄仍可读")

fd = os.open(target, os.O_RDWR)
try:
    target.unlink()
    assert not target.exists() and os.fstat(fd).st_nlink == 0
    assert os.pread(fd, 99, 0) == b"xy\0\0\0\0"
    assert os.pwrite(fd, b"Z", 0) == 1
    assert os.pread(fd, 1, 0) == b"Z"
finally:
    os.close(fd)
passed("unlink 后继续读写：数据直到句柄关闭才可回收")

sub = folder / "sub"
sub.mkdir()
child = sub / "child"
child.mkdir()
fails(errno.ENOTEMPTY, sub.rmdir)
fails(errno.EISDIR, sub.unlink)
fails(errno.EEXIST, sub.mkdir)
fails(errno.ENOENT, lambda: (folder / "missing").read_bytes())
fails(errno.EINVAL, lambda: os.rename(sub, child / "cycle"))
plain = folder / "plain"
plain.write_bytes(b"a")
fails(errno.ENOTDIR, plain.rmdir)
fails(errno.ENOTDIR, lambda: (plain / "nested").mkdir())
fails(errno.EISDIR, lambda: os.rename(plain, sub))
fails(errno.ENOTDIR, lambda: os.rename(sub, plain))
passed("错误码：缺失、重复、非空目录、类型冲突及目录循环")

left = folder / "left"
right = folder / "right"
left.mkdir()
right.mkdir()
moving = left / "moving"
moving.mkdir()
assert left.stat().st_nlink == 3 and right.stat().st_nlink == 2
os.rename(moving, right / "moved")
assert left.stat().st_nlink == 2 and right.stat().st_nlink == 3
assert os.stat(right / "moved/..").st_ino == right.stat().st_ino
(right / "moved").rmdir()
left.rmdir()
right.rmdir()
passed("跨目录移动：父目录、.. 与目录链接数正确更新")

listing = folder / "many"
listing.mkdir()
names = {f"item-{i:03d}-" + "x" * 80 for i in range(180)}
for name in names:
    (listing / name).touch()
assert set(os.listdir(listing)) == names
for name in names:
    (listing / name).unlink()
listing.rmdir()
passed("180 个长目录项：超过单次 readdir 缓冲区，offset 续读无遗漏")

plain.chmod(0o600)
assert stat.S_IMODE(plain.stat().st_mode) == 0o600
os.utime(plain, (1700000000, 1700000001))
assert int(plain.stat().st_mtime) == 1700000001
fails(errno.EFBIG, lambda: os.truncate(plain, 17 * 1024 * 1024))
assert os.statvfs(mount).f_bsize == 512
passed("属性修改、时间更新、单文件容量限制和 statfs")

plain.unlink()
child.rmdir()
sub.rmdir()
folder.rmdir()
assert not folder.exists()
passed("删除文件及空目录，完整清理演示操作")

(mount / "session-proof.txt").write_text("本次挂载专有数据")
(mount / "welcome.txt").write_text("已修改的欢迎文本")
passed("为重挂载检查准备临时文件和修改后的预置文件")
print(f"Mounted syscall scenarios: {checks} passed", flush=True)
