"""Deletion protection, idle timer, consistent diff, migration and failure recovery."""
import errno
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import time
from fs_harness import Mount, BINARY, fails, wait_for

h = Mount("v3")
m = h.mount
count = 0


def passed(message):
    global count
    count += 1
    print(f"PASS {count:02}: {message}", flush=True)


def restore(id, path, success=True):
    return h.cli("trash", "restore", m, id, path, success=success)


def read_parts(fd):
    data = b""
    while True:
        part = os.read(fd, 17)
        if not part:
            return json.loads(data)
        data += part


try:
    print("TeamFS v0.3 actual mounted acceptance", time.strftime("%Y-%m-%dT%H:%M:%S%z"), flush=True)
    print("binary:", BINARY, flush=True)
    h.start()
    note = m / "报告.bin"
    original = bytes(range(256)) * 20
    note.write_bytes(original)
    note.chmod(0o640)
    os.utime(note, (1700000000, 1700000001))
    original_ino = note.stat().st_ino
    old = os.open(note, os.O_RDWR)
    note.unlink()
    record = h.trash()[-1]
    archived = m / ".teamfs/trash" / str(record["id"])
    assert record["path_bytes"] == list("报告.bin".encode()) and record["reason"] == "unlink"
    assert archived.read_bytes() == original and archived.stat().st_ino != original_ino
    assert os.pwrite(old, b"changed", 0) == 7 and os.fstat(old).st_nlink == 0
    assert archived.read_bytes() == original
    os.close(old)
    restore(record["id"], "恢复.bin")
    recovered = m / "恢复.bin"
    assert recovered.read_bytes() == original and recovered.stat().st_ino != original_ino
    assert recovered.stat().st_mode & 0o777 == 0o640 and int(recovered.stat().st_mtime) == 1700000001
    assert recovered.stat().st_uid == record["attributes"]["uid"]
    assert h.status()["dirty"] and len(h.trash()) == 1
    for path in ["恢复.bin", "../outside", "/absolute", ".teamfs/new", "missing/new", "a//b"]:
        restore(record["id"], path, False)
    for operation in [lambda: archived.write_bytes(b"x"), archived.unlink, lambda: archived.chmod(0o600)]:
        fails({errno.EACCES, errno.EROFS}, operation)
    passed("unlink keeps immutable bytes, raw attributes and separate inode; restore is non-overwriting")

    raw = m / os.fsdecode(b"raw-\xff\n\\")
    raw.write_bytes(b"")
    raw.unlink()
    raw_record = h.trash()[-1]
    assert raw_record["path_bytes"] == list(b"raw-\xff\n\\")
    assert "\n" not in raw_record["path_display"]
    restore(raw_record["id"], os.fsdecode(b"restored-\xff"))
    assert (m / os.fsdecode(b"restored-\xff")).read_bytes() == b""
    for _ in range(2):
        note.write_bytes(b"repeat")
        note.unlink()
    assert len({r["id"] for r in h.trash()}) == 4
    passed("Chinese/non-UTF8/control names, empty file and repeated deletions retain distinct IDs")

    src, dst = m / "source", m / "target"
    src.write_bytes(b"new")
    dst.write_bytes(b"old")
    fd = os.open(dst, os.O_RDWR)
    before = len(h.trash())
    src.rename(dst)
    replacement = h.trash()[-1]
    assert replacement["reason"] == "rename_replace" and replacement["path_bytes"] == list(b"target")
    assert dst.read_bytes() == b"new" and os.pread(fd, 9, 0) == b"old"
    os.pwrite(fd, b"later", 0)
    assert (m / ".teamfs/trash" / str(replacement["id"])).read_bytes() == b"old"
    os.close(fd)
    dst.rename(dst)
    folder = m / "folder"
    folder.mkdir()
    fails({errno.EISDIR}, lambda: dst.rename(folder))
    fails({errno.ENOTDIR}, lambda: folder.rename(dst))
    fails({errno.ENOENT}, lambda: (m / "missing").rename(dst))
    dst.write_bytes(b"truncate does not archive")
    assert len(h.trash()) == before + 1
    passed("rename overwrite is protected; no-op/failed rename and direct truncate do not add records")

    h.sync()
    committed = h.trash()
    pending = m / "pending"
    pending.write_bytes(b"durable")
    h.sync()
    pending.unlink()
    h.stop(kill=True)
    h.start()
    assert pending.read_bytes() == b"durable" and h.trash() == committed
    pending.unlink()
    h.sync()
    h.stop(kill=True)
    h.start()
    assert not pending.exists() and h.trash()[-1]["path_bytes"] == list(b"pending")
    (m / "normal").write_bytes(b"exit")
    (m / "normal").unlink()
    h.stop()
    h.start()
    assert h.trash()[-1]["path_bytes"] == list(b"normal")
    passed("current tree and trash commit together across sync, crash and normal unmount")

    fd = os.open(archived, os.O_RDONLY)
    high_id = max(r["id"] for r in h.trash())
    h.purge(record["id"])
    assert not archived.exists() and os.pread(fd, len(original), 0) == original
    os.close(fd)
    h.purge()
    h.stop(kill=True)
    h.start()
    assert not h.trash()
    (m / "id-test").touch()
    (m / "id-test").unlink()
    assert h.trash()[0]["id"] > high_id
    passed("purge persists, held historical descriptor survives, committed IDs are not reused")

    db = sqlite3.connect(h.store / "state.sqlite3")
    h.sync()
    db.execute("CREATE TRIGGER fail_trash BEFORE INSERT ON trash BEGIN SELECT RAISE(ABORT,'trash failure'); END")
    db.commit()
    (m / "rollback").write_bytes(b"keep")
    (m / "rollback").unlink()
    current = h.trash()
    h.cli("sync", m, success=False)
    assert h.status()["dirty"] and "trash failure" in h.status()["last_sync_error"]
    # Purging one of two records still attempts the other INSERT and must rollback.
    h.cli("trash", "purge", m, current[0]["id"], success=False)
    assert h.trash() == current
    assert db.execute("SELECT count(*) FROM trash").fetchone()[0] == 1
    db.execute("DROP TRIGGER fail_trash")
    db.commit()
    h.sync()
    assert not h.status()["dirty"]
    db.execute("CREATE TRIGGER fail_purge BEFORE DELETE ON trash BEGIN SELECT RAISE(ABORT,'purge failure'); END")
    db.commit()
    h.cli("trash", "purge", m, "--all", success=False)
    assert h.trash() == current
    db.execute("DROP TRIGGER fail_purge")
    db.commit()
    db.close()
    h.purge()
    passed("trash INSERT/DELETE failure rolls back all tables and visible purge; retry succeeds")

    # Stable read-only listings retain a complete JSON even when entries change.
    (m / "frozen").write_bytes(b"frozen")
    (m / "frozen").unlink()
    index = m / ".teamfs/trash/index.json"
    fd = os.open(index, os.O_RDONLY)
    frozen_records = h.trash()
    h.purge()
    assert read_parts(fd)["entries"] == frozen_records
    os.close(fd)
    assert not h.trash()
    passed("trash index is frozen per open and refreshed on reopen")

    for i in range(1024):
        item = m / "empty-limit"
        item.touch()
        item.unlink()
    assert len(os.listdir(m / ".teamfs/trash")) == 1025
    victim = m / "victim"
    victim.write_bytes(b"keep")
    replacement_src = m / "replacement"
    replacement_src.write_bytes(b"source")
    fails({errno.ENOSPC}, victim.unlink)
    fails({errno.ENOSPC}, lambda: replacement_src.rename(victim))
    assert victim.read_bytes() == b"keep" and replacement_src.read_bytes() == b"source"
    assert h.status()["trash_count"] == 1024
    h.purge()
    for i in range(4):
        item = m / "large-trash"
        item.touch()
        os.truncate(item, 16 * 1024 * 1024)
        item.unlink()
    assert h.status()["trash_bytes"] == 64 * 1024 * 1024
    fails({errno.ENOSPC}, victim.unlink)
    fails({errno.ENOSPC}, lambda: replacement_src.rename(victim))
    assert victim.read_bytes() == b"keep" and replacement_src.read_bytes() == b"source"
    h.purge()
    victim.unlink()
    h.purge()
    passed("1024-record/64MiB limits refuse unlink and replacement without changing either file")

    h.snapshot("比较")
    h.sync()
    before = h.status()
    assert not h.diff("比较")["changes"]
    after = h.status()
    for field in ["dirty", "read_calls", "write_calls", "last_sync_unix"]:
        assert after[field] == before[field]
    dst.read_bytes()
    assert not h.diff("比较")["changes"]
    previous_time = dst.stat().st_mtime_ns
    # Same-length mutation with old mtime must still be detected by actual byte comparison.
    data = dst.read_bytes()
    dst.write_bytes(bytes([data[0] ^ 1]) + data[1:])
    os.utime(dst, ns=(previous_time, previous_time))
    recovered.chmod(0o600)
    os.utime(recovered, (1700000000, 1700000002))
    (m / "added-dir").mkdir()
    (m / "added-dir/new").write_bytes(b"new")
    (m / "replacement").rename(m / "renamed")
    (m / os.fsdecode(b"restored-\xff")).unlink()
    folder.rmdir()
    folder.write_bytes(b"now file")
    value = h.diff("比较")
    changes = {bytes(c["path_bytes"]): c for c in value["changes"]}
    assert changes[b"target"]["content_changed"] and changes[b"target"]["change"] == "modified"
    assert changes["恢复.bin".encode()]["metadata_changed"] == ["mode", "mtime"]
    assert changes[b"folder"]["change"] == "type_changed"
    assert changes[b"replacement"]["change"] == "removed" and changes[b"renamed"]["change"] == "added"
    assert changes[b"added-dir"]["change"] == "added" and changes[b"added-dir/new"]["change"] == "added"
    assert list(changes) == sorted(changes)
    h.cli("snapshot", "diff", m, "missing", success=False)
    for invalid in ["../x", "/x"]:
        h.cli("snapshot", "diff", m, invalid, success=False)
    passed("diff reports byte/content, metadata, type, paths and recursive additions; ignores atime")

    diff_path = m / ".teamfs/diffs/比较"
    fd = os.open(diff_path, os.O_RDONLY)
    (m / "after-diff-open").touch()
    old_diff = read_parts(fd)
    os.close(fd)
    assert all(c["path_display"] != "after-diff-open" for c in old_diff["changes"])
    fd = os.open(diff_path, os.O_RDONLY)
    h.cli("snapshot", "delete", m, "比较")
    assert not diff_path.exists()
    assert any(c["path_display"] == "after-diff-open" for c in read_parts(fd)["changes"])
    os.close(fd)
    passed("diff result is frozen per descriptor and survives deletion of its snapshot")

    h.stop()
    h.start("--auto-sync", 1)
    (m / "timer").write_bytes(b"idle save")
    # No further FUSE requests until deadline: inspect the backing commit through a read-only SQL connection.
    def timer_saved():
        with sqlite3.connect(f"file:{h.store / 'state.sqlite3'}?mode=ro", uri=True) as conn:
            row = conn.execute("SELECT b.data FROM nodes n JOIN blobs b ON n.content_id=b.id WHERE n.scope='' AND n.name=?", (b"timer",)).fetchone()
            return row is not None and row[0] == b"idle save"
    wait_for(timer_saved)
    assert h.status()["auto_sync_successes"] >= 1
    h.stop(kill=True)
    h.start("--auto-sync", 1)
    assert (m / "timer").read_bytes() == b"idle save"
    h.sync()
    baseline = h.status()["auto_sync_attempts"]
    time.sleep(1.2)
    assert h.status()["auto_sync_attempts"] == baseline
    db = sqlite3.connect(h.store / "state.sqlite3")
    db.execute("CREATE TRIGGER fail_auto BEFORE INSERT ON nodes BEGIN SELECT RAISE(ABORT,'auto failure'); END")
    db.commit()
    (m / "timer").write_bytes(b"retry")
    wait_for(lambda: "auto failure" in (h.status()["last_sync_error"] or ""))
    assert h.status()["dirty"] and h.process.poll() is None
    db.execute("DROP TRIGGER fail_auto")
    db.commit()
    db.close()
    wait_for(lambda: not h.status()["dirty"] and h.status()["last_sync_error"] is None)
    h.stop(kill=True)
    h.start("--auto-sync", 86400)
    assert (m / "timer").read_bytes() == b"retry"
    begin = time.monotonic()
    h.stop()
    assert time.monotonic() - begin < 5
    passed("idle timer commits; clean ticks skip; failures retry; stop wakes without waiting interval")

    for options in [("--auto-sync", "0"), ("--auto-sync", "86401"), ("--auto-sync", "1.5"),
                    ("--auto-sync", "-1"), ("--auto-sync", "1", "--auto-sync", "2"),
                    ("--memory", "--auto-sync", "1")]:
        h.cli("mount", m, *options, success=False)
    h.start()
    assert h.status()["auto_sync_interval_seconds"] is None
    (m / "no-auto").write_bytes(b"unsaved")
    time.sleep(1.2)
    assert h.status()["auto_sync_attempts"] == 0
    h.stop(kill=True)
    h.start()
    assert not (m / "no-auto").exists()
    h.stop()
    passed("optional timer arguments validated; default still loses unsynced state on crash")

    # Produce a v1 database with the original schema and validated live/snapshot data.
    db = sqlite3.connect(h.store / "state.sqlite3")
    db.execute("UPDATE nodes SET data=(SELECT data FROM blobs WHERE id=nodes.content_id)")
    db.execute("ALTER TABLE nodes DROP COLUMN content_id")
    db.execute("DROP TABLE blobs")
    db.execute("ALTER TABLE metadata DROP COLUMN capacity")
    db.execute("ALTER TABLE metadata DROP COLUMN max_file")
    db.execute("DROP TABLE trash")
    db.execute("ALTER TABLE metadata DROP COLUMN next_trash_id")
    db.execute("UPDATE metadata SET version=1")
    db.commit()
    original_nodes = db.execute("SELECT * FROM nodes ORDER BY scope,ino").fetchall()
    db.execute("CREATE TRIGGER fail_migration BEFORE UPDATE OF version ON metadata BEGIN SELECT RAISE(ABORT,'migration failure'); END")
    db.commit()
    db.close()
    h.cli("mount", m, "--store", h.store, success=False)
    db = sqlite3.connect(h.store / "state.sqlite3")
    assert db.execute("SELECT version FROM metadata").fetchone()[0] == 1
    assert not db.execute("SELECT name FROM sqlite_master WHERE name='trash'").fetchall()
    assert db.execute("SELECT * FROM nodes ORDER BY scope,ino").fetchall() == original_nodes
    db.execute("DROP TRIGGER fail_migration")
    db.commit()
    db.close()
    h.start()
    assert (m / "timer").read_bytes() == b"retry" and not h.trash()
    with sqlite3.connect(h.store / "state.sqlite3") as db:
        assert db.execute("SELECT version FROM metadata").fetchone()[0] == 3
        assert db.execute("SELECT n.scope,n.ino,n.parent,n.name,n.attr,b.data FROM nodes n JOIN blobs b ON n.content_id=b.id ORDER BY n.scope,n.ino").fetchall() == original_nodes
    h.stop()
    passed("v1 migration preserves all stored rows; migration failure rolls back schema and version")

    # If supplied, also verify an authentic store produced by the old executable.
    old_binary = os.environ.get("TEAMFS_V02_BIN")
    if old_binary:
        h.store = h.base / "authentic-v1"
        h.start(binary=Path(old_binary))
        (m / os.fsdecode(b"v1-\xff")).write_bytes(b"\x00\xfflegacy")
        (m / "资料").mkdir()
        (m / "资料/报告").write_bytes(b"old report")
        (m / "资料/报告").chmod(0o640)
        subprocess.run([old_binary, "snapshot", "create", str(m), "旧版本"], check=True, stdout=subprocess.DEVNULL)
        h.stop()
        with sqlite3.connect(h.store / "state.sqlite3") as db:
            assert db.execute("SELECT version FROM metadata").fetchone()[0] == 1
        h.start()
        assert (m / os.fsdecode(b"v1-\xff")).read_bytes() == b"\x00\xfflegacy"
        assert (m / ".teamfs/snapshots/旧版本/资料/报告").read_bytes() == b"old report"
        assert (m / "资料/报告").stat().st_mode & 0o777 == 0o640
        h.stop()
        passed("authentic 0.2 executable's v1 store migrates with raw names, attributes and snapshots")

    h.store = h.base / "combined"
    h.start("--auto-sync", 1)
    for i in range(4):
        (m / "sequence").write_bytes(f"version-{i}".encode())
        h.snapshot(f"s{i}")
        (m / "sequence").unlink()
        record = h.trash()[-1]
        restore(record["id"], "sequence")
        assert not h.diff(f"s{i}")["changes"]
        h.purge(record["id"])
        h.cli("snapshot", "delete", m, f"s{i}")
    wait_for(lambda: not h.status()["dirty"])
    h.stop(kill=True)
    h.start()
    assert (m / "sequence").read_bytes() == b"version-3" and not h.trash()
    h.stop()
    h.start("--memory")
    (m / "temporary").touch()
    (m / "temporary").unlink()
    h.snapshot("temporary")
    assert h.trash()
    h.stop()
    h.start("--memory")
    assert not h.trash() and h.status()["snapshot_count"] == 0
    h.stop()
    passed("combined auto-sync/snapshot/trash/diff operations serialize correctly; memory resets")
    print(f"ALL PASS: {count} v3 mounted scenarios", flush=True)
finally:
    h.close()
