"""Fixed-block foundation: real FUSE, version isolation, rollback, migration and backup."""
import hashlib
import json
import os
import platform
import sqlite3
import time
from fs_harness import Mount, ROOT
from storage_helpers import legacy_nodes, stored_bytes

PAGE = 65536
h = Mount("blocks")
m = h.mount
results = []
samples = []

def passed(message):
    results.append(message)
    print(f"PASS {len(results):02}: {message}", flush=True)

def database():
    return sqlite3.connect(h.store / "state.sqlite3")

def logical_digest():
    # SQLite may checkpoint committed WAL pages when connections close. Compare
    # schema and application values rather than mistaking that for data repair.
    digest = hashlib.sha256()
    db = database()
    try:
        schema = db.execute("SELECT type,name,sql FROM sqlite_master ORDER BY type,name").fetchall()
        digest.update(repr(schema).encode())
        for name, in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").fetchall():
            for row in db.execute(f'SELECT * FROM "{name}" ORDER BY rowid'):
                for value in row:
                    data = value if isinstance(value, bytes) else repr(value).encode()
                    digest.update(str(len(data)).encode()+b":"+data)
        return digest.hexdigest()
    finally:
        db.close()

try:
    h.start()
    f = m / "局部修改.bin"
    original = bytes(range(256)) * (16 * 1024 * 1024 // 256)
    f.write_bytes(original)
    h.snapshot("before")
    expected = bytearray(original)
    for iteration in range(3):
        fd = os.open(f, os.O_RDWR)
        begin = time.perf_counter()
        patch = bytes([90 + iteration]) * 4096
        assert os.pwrite(fd, patch, PAGE * 12 + 123) == len(patch)
        os.close(fd)
        write_ms = (time.perf_counter() - begin) * 1000
        expected[PAGE * 12 + 123:PAGE * 12 + 123 + len(patch)] = patch
        before = h.status()
        assert before["resident_unsaved_content_bytes"] == PAGE
        begin = time.perf_counter(); h.sync()
        sync_ms = (time.perf_counter() - begin) * 1000
        state = h.status()
        assert state["storage_format"] == 4
        assert state["last_commit_content_bytes"] == PAGE
        assert state["last_commit_new_blocks"] == 1
        assert state["last_commit_shared_blocks"] == 255
        assert state["resident_unsaved_content_bytes"] == 0
        samples.append({"changed_bytes": len(patch), "new_payload_bytes": state["last_commit_content_bytes"],
                        "new_blocks": 1, "shared_blocks": 255, "write_ms": write_ms, "sync_cli_ms": sync_ms})
    assert f.read_bytes() == expected
    assert (m / ".teamfs/snapshots/before/局部修改.bin").read_bytes() == original
    passed("16 MiB file / 4 KiB overwrite saves 64 KiB, shares 255 blocks; snapshot bytes remain unchanged")

    fd = os.open(f, os.O_RDWR); os.pwrite(fd, patch, PAGE * 12 + 123); os.close(fd)
    h.sync()
    assert h.status()["last_commit_content_bytes"] == 0
    assert h.status()["last_commit_shared_blocks"] == 256
    passed("overwriting a block with identical bytes reuses its old payload")

    # A boundary-spanning overwrite touches precisely two blocks.
    fd = os.open(f, os.O_RDWR); os.pwrite(fd, b"boundary", PAGE - 3); os.close(fd)
    expected[PAGE - 3:PAGE + 5] = b"boundary"
    h.sync(); state = h.status()
    assert state["last_commit_new_blocks"] == 2 and state["last_commit_content_bytes"] == PAGE * 2
    assert f.read_bytes() == expected
    h.stop(); h.start()
    assert f.read_bytes() == expected
    passed("cross-block write and normal remount preserve byte-exact contents")

    # Truncate then extend must never expose bytes from the old version.
    small = m / "truncate.bin"; small.write_bytes(b"X" * (PAGE * 3 + 7)); h.sync()
    os.truncate(small, PAGE + 9); os.truncate(small, PAGE * 4 + 13)
    assert small.read_bytes() == b"X" * (PAGE + 9) + bytes(PAGE * 3 + 4)
    h.sync(); h.stop(); h.start()
    assert small.read_bytes() == b"X" * (PAGE + 9) + bytes(PAGE * 3 + 4)
    os.truncate(small, 0); h.sync(); h.stop(); h.start(); assert small.read_bytes() == b""
    passed("truncate/grow, partial last blocks and zero-length files survive remount without resurrecting bytes")

    # Both payload and manifest creation must roll back on failure.
    for table in ("blobs", "content_blocks", "contents"):
        with database() as db:
            saved = stored_bytes(db, "局部修改.bin")
            counts = [db.execute(f"SELECT count(*) FROM {t}").fetchone()[0] for t in ("blobs", "content_blocks", "contents")]
            db.execute(f"CREATE TRIGGER block_fail BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'injected block failure'); END")
        fd = os.open(f, os.O_RDWR); os.pwrite(fd, b"failure-test", PAGE * 20); os.close(fd)
        h.cli("sync", m, success=False)
        assert h.status()["dirty"] and h.status()["last_sync_error"]
        with database() as db:
            assert stored_bytes(db, "局部修改.bin") == saved
            assert counts == [db.execute(f"SELECT count(*) FROM {t}").fetchone()[0] for t in ("blobs", "content_blocks", "contents")]
            db.execute("DROP TRIGGER block_fail")
        h.sync(); expected[PAGE * 20:PAGE * 20 + 12] = b"failure-test"
        assert f.read_bytes() == expected
    passed("payload, manifest and block-map insertion failures roll back together; dirty content remains readable and retries")

    # Unsaved changes are discarded after SIGKILL; saved snapshot survives.
    fd = os.open(f, os.O_RDWR); os.pwrite(fd, b"not-synced", 42); os.close(fd)
    h.stop(kill=True); h.start()
    assert f.read_bytes() == expected
    assert (m / ".teamfs/snapshots/before/局部修改.bin").read_bytes() == original
    passed("SIGKILL restores the last complete commit and retains immutable history")

    # Deleted history remains readable by an already open file handle.
    held = os.open(m / ".teamfs/snapshots/before/局部修改.bin", os.O_RDONLY)
    h.cli("snapshot", "delete", m, "before")
    assert os.pread(held, 4096, PAGE * 200) == original[PAGE * 200:PAGE * 200 + 4096]
    os.close(held)
    h.snapshot("retained")
    f.unlink(); records = h.trash()
    record = next(r for r in records if r["path_display"] == "局部修改.bin")
    h.cli("trash", "restore", m, record["id"], "restored.bin"); h.sync()
    assert (m / "restored.bin").read_bytes() == expected
    backup = h.base / "backup"
    h.cli("backup", "create", m, backup)
    h.cli("backup", "verify", backup)
    imported = h.base / "imported"
    h.cli("backup", "import", backup, "--store", imported)
    h.stop(); old_store = h.store; h.store = imported; h.start()
    assert (m / "restored.bin").read_bytes() == expected
    assert (m / ".teamfs/snapshots/retained/局部修改.bin").read_bytes() == expected
    h.stop(); h.store = old_store
    passed("held deleted snapshot, trash restore and independent backup/import preserve shared blocks")

    # Build an authentic format-3 layout from the fixture, then migrate transactionally.
    with database() as db:
        old_nodes = legacy_nodes(db)
        payloads = [(cid, stored) for cid, in db.execute("SELECT id FROM contents").fetchall()
                    for stored in [b"".join(b[0] for b in db.execute("SELECT b.data FROM content_blocks c JOIN blobs b ON b.id=c.blob_id WHERE c.content_id=? ORDER BY c.page", (cid,)).fetchall())]]
        db.execute("DROP TABLE content_blocks"); db.execute("DROP TABLE contents"); db.execute("DELETE FROM blobs")
        db.executemany("INSERT INTO blobs VALUES(?,?)", payloads)
        db.execute("UPDATE metadata SET version=3")
        db.execute("CREATE TRIGGER migration_fail BEFORE UPDATE OF version ON metadata BEGIN SELECT RAISE(ABORT,'migration failure'); END")
    h.cli("mount", m, "--store", h.store, success=False)
    with database() as db:
        assert db.execute("SELECT version FROM metadata").fetchone()[0] == 3
        assert not db.execute("SELECT name FROM sqlite_master WHERE name='contents'").fetchall()
        assert legacy_nodes(db) == old_nodes
        db.execute("DROP TRIGGER migration_fail")
    h.start()
    assert (m / "restored.bin").read_bytes() == expected
    assert (m / ".teamfs/snapshots/retained/局部修改.bin").read_bytes() == expected
    with database() as db:
        assert db.execute("SELECT version FROM metadata").fetchone()[0] == 4
        assert legacy_nodes(db) == old_nodes
    h.stop()
    passed("format-3 migration preserves paths, attributes and bytes; injected failure restores original schema")

    # Corrupt mappings must be rejected before startup GC changes the store.
    with database() as db:
        db.execute("UPDATE content_blocks SET page=99999 WHERE rowid=(SELECT rowid FROM content_blocks LIMIT 1)")
    db.close()
    digest = logical_digest()
    h.cli("mount", m, "--store", h.store, success=False)
    assert logical_digest() == digest
    passed("invalid block mappings are rejected without automatic repair or erasure")

    evidence = {"measured_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "kernel": platform.release(),
                "build": "debug", "version": state["version"], "block_bytes": PAGE, "file_bytes": len(original),
                "samples": samples, "recovered_sha256": hashlib.sha256(expected).hexdigest(), "checks": results,
                "scope": "Foundation acceptance, not a native filesystem or cloud performance comparison. Bytes are inserted content payload, not physical disk writes."}
    (ROOT / "artifacts/block-foundation.json").write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n")
finally:
    h.close()
