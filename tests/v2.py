"""独立挂载、真实系统调用、崩溃与 SQLite 失败注入；不使用用户体验挂载点。"""
import errno
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time

root = Path(__file__).resolve().parents[1]
binary = Path(os.environ.get("CARGO_TARGET_DIR", str(Path.home() / ".cache/teamfs/target"))) / "debug/teamfs"
base = Path(tempfile.mkdtemp(prefix="teamfs-v2-check-", dir=Path.home()))
mount = base / "mnt"
other = base / "other"
store = base / "store"
mount.mkdir()
other.mkdir()
trace = (root / "artifacts/v2-trace.log").open("w")
process = None
count = 0


def passed(message):
    global count
    count += 1
    print(f"PASS {count:02d}: {message}", flush=True)


def mounted():
    return subprocess.run(["findmnt", "-rn", "--mountpoint", str(mount)], stdout=subprocess.DEVNULL).returncode == 0


def start(memory=False):
    global process
    args = [str(binary), "mount", str(mount), "--trace"]
    args += ["--memory"] if memory else ["--store", str(store)]
    process = subprocess.Popen(args, stdout=trace, stderr=trace)
    for _ in range(200):
        if process.poll() is not None:
            raise AssertionError("mount exited; see artifacts/v2-trace.log")
        if mounted():
            return
        time.sleep(.05)
    raise AssertionError("mount timeout")


def stop(kill=False):
    global process
    if kill:
        process.kill()
        process.wait(timeout=15)
    subprocess.run(["fusermount3", "-u", str(mount)], check=True)
    result = process.wait(timeout=15)
    if not kill:
        assert result == 0, result
    process = None


def cli(*args, success=True):
    result = subprocess.run([str(binary), *map(str, args)], capture_output=True, timeout=30)
    assert (result.returncode == 0) == success, (args, result.stdout, result.stderr)
    return result


def snapshot(name):
    cli("snapshot", "create", mount, name)


def status():
    return json.loads((mount / ".teamfs/status.json").read_bytes())


def fails(errors, operation):
    try:
        operation()
    except OSError as error:
        assert error.errno in errors, error
    else:
        raise AssertionError("operation unexpectedly succeeded")


try:
    print("TeamFS v0.2 实际验收", flush=True)
    print(time.strftime("%Y-%m-%dT%H:%M:%S%z"), flush=True)
    print("binary:", binary, flush=True)
    start()
    assert status()["mode"] == "persistent" and not status()["dirty"]
    folder = mount / "资料"
    folder.mkdir()
    original = bytes(range(256)) * 100 + "初稿\n".encode()
    note = folder / "报告.bin"
    note.write_bytes(original)
    note.chmod(0o640)
    os.utime(note, (1700000000, 1700000001))
    raw_name = os.fsencode(mount) + b"/raw-\xff"
    fd = os.open(raw_name, os.O_CREAT | os.O_WRONLY, 0o600)
    os.write(fd, b"raw-name")
    os.close(fd)
    (folder / "empty").touch()
    ino = note.stat().st_ino
    assert status()["dirty"]
    cli("sync", mount)
    assert not status()["dirty"]
    stop(kill=True)
    start()
    assert note.read_bytes() == original and note.stat().st_ino == ino
    assert note.stat().st_mode & 0o777 == 0o640 and int(note.stat().st_mtime) == 1700000001
    assert Path(os.fsdecode(raw_name)).read_bytes() == b"raw-name"
    passed("显式同步后 SIGKILL：中文/非 UTF-8 名称、二进制、权限、mtime、inode 保留")

    note.write_bytes(b"UNSYNCED")
    (mount / "unsynced.txt").write_text("未保存")
    stop(kill=True)
    start()
    assert note.read_bytes() == original and not (mount / "unsynced.txt").exists()
    passed("未同步版本 B 在异常退出后丢失，恢复到已同步版本 A")

    note.rename(folder / "改名.bin")
    note = folder / "改名.bin"
    (folder / "empty").unlink()
    (mount / "exit-saved").write_bytes(b"normal unmount")
    stop()
    start()
    assert note.read_bytes() == original and not (folder / "报告.bin").exists()
    assert not (folder / "empty").exists() and (mount / "exit-saved").read_bytes() == b"normal unmount"
    passed("正常卸载保存：创建、删除和改名跨重挂载保留")

    fd = os.open(note, os.O_WRONLY)
    os.pwrite(fd, b"file-fsync", 0)
    os.fsync(fd)
    os.close(fd)
    stop(kill=True)
    start()
    assert note.read_bytes().startswith(b"file-fsync")
    (mount / "dir-sync").mkdir()
    fd = os.open(mount, os.O_RDONLY | os.O_DIRECTORY)
    os.fsync(fd)
    os.close(fd)
    stop(kill=True)
    start()
    assert (mount / "dir-sync").is_dir()
    passed("普通文件 fsync 与目录 fsync 都提交业务文件树")

    listing = folder / "many"
    listing.mkdir()
    names = {f"long-{i:03}-" + "x" * 80 for i in range(180)}
    for name in names:
        (listing / name).touch()
    note.write_bytes(original)
    (folder / "empty").touch()
    os.utime(note, (1700000000, 1700000001))
    begin = time.perf_counter()
    snapshot("初稿")
    elapsed = time.perf_counter() - begin
    history = mount / ".teamfs/snapshots/初稿/资料/改名.bin"
    assert history.stat().st_ino != note.stat().st_ino
    assert set(os.listdir(history.parent / "many")) == names
    assert history.read_bytes() == original
    cli("snapshot", "create", mount, "初稿", success=False)
    cli("snapshot", "create", mount, "../bad", success=False)
    stop(kill=True)
    start()
    assert history.read_bytes() == original
    passed(f"快照创建即保存、重启可读、inode 隔离、180 项目录续读；创建耗时 {elapsed:.6f}s")

    old = os.open(history, os.O_RDONLY)
    note.write_bytes(b"changed")
    assert os.read(old, len(original) + 1) == original
    for operation in [lambda: history.write_bytes(b"bad"), history.unlink, lambda: history.chmod(0o600), lambda: os.rename(history, folder / "bad")]:
        fails({errno.EROFS, errno.EACCES}, operation)
    fails({errno.EROFS, errno.EACCES}, lambda: (history.parent / "bad").touch())
    note.unlink()
    assert history.read_bytes() == original
    passed("当前文件覆盖/删除不改变历史，快照全部写操作受保护")

    subprocess.run(["cp", "-n", str(history), str(folder / "copied.bin")], check=True)
    subprocess.run(["diff", str(history), str(folder / "copied.bin")], check=True)

    cli("restore", mount, "初稿", "资料/改名.bin", "资料/恢复.bin")
    restored = folder / "恢复.bin"
    assert restored.read_bytes() == original
    assert restored.stat().st_mode & 0o777 == 0o640 and int(restored.stat().st_mtime) == 1700000001
    cli("restore", mount, "初稿", "资料/empty", "资料/空文件")
    assert (folder / "空文件").read_bytes() == b""
    for source, destination in [("资料/改名.bin", "资料/恢复.bin"), ("资料/改名.bin", "missing/a"), ("资料/改名.bin", "../escape"), ("资料/改名.bin", ".teamfs/x"), ("资料/改名.bin", "/absolute"), ("资料", "folder-copy")]:
        cli("restore", mount, "初稿", source, destination, success=False)
    cli("restore", mount, "初稿", os.fsdecode(b"raw-\xff"), "资料/raw-restored")
    assert (folder / "raw-restored").read_bytes() == b"raw-name"
    assert status()["dirty"]
    print("恢复 SHA256:", hashlib.sha256(restored.read_bytes()).hexdigest(), flush=True)
    assert restored.read_bytes() == original
    passed("恢复中文/原始字节名称、二进制与空文件；拒绝覆盖、越界、目录源和缺失父目录")

    cli("snapshot", "delete", mount, "初稿")
    assert not history.exists()
    assert os.pread(old, len(original), 0) == original
    os.close(old)
    stop(kill=True)
    start()
    assert not history.exists() and restored.read_bytes() == original
    passed("删除快照即提交；已打开历史句柄继续读取；同一事务保存当前恢复文件")

    state_path = mount / ".teamfs/status.json"
    cli("sync", mount)
    initial = status()
    fd = os.open(state_path, os.O_RDONLY)
    part = os.read(fd, 19)
    (mount / "counted").write_bytes(b"counter")
    assert (mount / "counted").read_bytes() == b"counter"
    while True:
        chunk = os.read(fd, 13)
        if not chunk:
            break
        part += chunk
    os.close(fd)
    frozen = json.loads(part)
    current = status()
    assert frozen["files"] == initial["files"] and current["files"] == initial["files"] + 1
    assert current["write_bytes"] == initial["write_bytes"] + 7
    assert current["read_bytes"] == initial["read_bytes"] + 7
    assert current["dirty"]
    assert status()["read_calls"] == current["read_calls"]
    fails({errno.EROFS, errno.EACCES}, lambda: state_path.write_bytes(b"bad"))
    passed("状态 JSON 按打开时刻固定、分段一致、重新打开刷新；排除管理读写计数")

    control = mount / ".teamfs/control"
    fd = os.open(control, os.O_WRONLY)
    os.write(fd, b'{"operation":"snapshot_create",')
    os.write(fd, '"name":"control-once"}'.encode())
    assert not (mount / ".teamfs/snapshots/control-once").exists()
    os.fsync(fd)
    os.fsync(fd)
    os.close(fd)
    assert status()["snapshot_count"] == 1
    fd = os.open(control, os.O_WRONLY)
    os.write(fd, b'{"operation":')
    fails({errno.EINVAL}, lambda: os.fsync(fd))
    fails({errno.EINVAL}, lambda: os.fsync(fd))
    os.close(fd)
    fd = os.open(control, os.O_WRONLY)
    os.write(fd, b'{"operation":"snapshot_create","name":"not-executed"}')
    os.close(fd)
    assert not (mount / ".teamfs/snapshots/not-executed").exists()
    passed("控制请求只在 fsync 执行一次，分段写入可用，非法/未同步请求无副作用")

    cli("sync", mount)
    old_content = restored.read_bytes()
    restored.write_bytes(b"pending failure test")
    db = sqlite3.connect(store / "state.sqlite3")
    db.execute("CREATE TRIGGER fail_insert BEFORE INSERT ON nodes BEGIN SELECT RAISE(ABORT,'injected save failure'); END")
    db.commit()
    cli("sync", mount, success=False)
    state = status()
    assert state["dirty"] and "injected save failure" in state["last_sync_error"]
    unmount_failed = subprocess.run(["bash", str(root / "scripts/unmount.sh"), str(mount)], capture_output=True, timeout=20)
    assert unmount_failed.returncode != 0 and mounted() and process.poll() is None
    cli("snapshot", "create", mount, "failed-snapshot", success=False)
    assert not (mount / ".teamfs/snapshots/failed-snapshot").exists()
    cli("snapshot", "delete", mount, "control-once", success=False)
    assert (mount / ".teamfs/snapshots/control-once").exists()
    live_blob = db.execute("SELECT data FROM nodes WHERE scope='' AND name=?", (sqlite3.Binary("恢复.bin".encode()),)).fetchone()[0]
    assert live_blob == old_content
    db.execute("DROP TRIGGER fail_insert")
    db.commit()
    cli("sync", mount)
    assert status()["last_sync_error"] is None and not status()["dirty"]
    db.close()
    passed("事务失败回滚、保留旧提交和 dirty；失败快照不可见；修复后可再次同步")

    result = cli("mount", other, "--store", store, success=False)
    assert "另一个".encode() in result.stderr
    cli("mount", other, "--store", other / "inside", success=False)
    assert not (other / "inside").exists()
    cli("mount", other, "--store", base / "unused" / ".." / "other" / "inside", success=False)
    assert not (other / "inside").exists()
    passed("拒绝重复使用存储目录，以及在挂载点内建立后端存储")

    for n in range(9):
        snapshot(f"limit-{n}")
    cli("snapshot", "create", mount, "too-many", success=False)
    assert status()["snapshot_count"] == 10
    for snap in status()["snapshots"]:
        cli("snapshot", "delete", mount, snap["name"])
    passed("10 个快照上限，超限拒绝且不自动删除旧版本")

    stop()
    db = sqlite3.connect(store / "state.sqlite3")
    db.execute("UPDATE metadata SET version=999")
    db.commit()
    db.close()
    before = (store / "state.sqlite3").read_bytes()
    cli("mount", mount, "--store", store, success=False)
    assert (store / "state.sqlite3").read_bytes() == before
    corrupt = base / "corrupt"
    corrupt.mkdir()
    (corrupt / "state.sqlite3").write_bytes(b"this is not sqlite")
    cli("mount", mount, "--store", corrupt, success=False)
    assert (corrupt / "state.sqlite3").read_bytes() == b"this is not sqlite"
    passed("不支持的格式版本、损坏数据库明确拒绝且不覆盖")

    start(memory=True)
    (mount / "memory-only").write_text("temporary")
    snapshot("memory-snapshot")
    assert status()["mode"] == "memory"
    stop()
    start(memory=True)
    assert not (mount / "memory-only").exists() and status()["snapshot_count"] == 0
    # 约 48 MiB 文件树，两个快照约 96 MiB，第三个超过 128 MiB 总容量。
    for n in range(3):
        fd = os.open(mount / f"large-{n}", os.O_CREAT | os.O_WRONLY, 0o600)
        os.ftruncate(fd, 16 * 1024 * 1024)
        os.close(fd)
    snapshot("large-a")
    snapshot("large-b")
    cli("snapshot", "create", mount, "large-c", success=False)
    assert status()["snapshot_count"] == 2
    stop()
    passed("内存模式重启清空，128 MiB 快照内容上限独立生效")
    print(f"ALL PASS: {count} v2 mounted scenarios; 测试挂载与临时存储将清理。", flush=True)
finally:
    if process is not None and process.poll() is None:
        process.kill()
        process.wait(timeout=10)
    if mounted():
        subprocess.run(["fusermount3", "-uz", str(mount)], check=True)
    trace.close()
    assert base.parent == Path.home() and base.name.startswith("teamfs-v2-check-") and not mounted()
    shutil.rmtree(base)
