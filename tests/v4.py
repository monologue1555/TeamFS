"""Real v0.4 workloads: atomic directory recovery, independent backups and application compatibility."""
import errno
import fcntl
import json
import mmap
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import time
from fs_harness import Mount, BINARY, fails

h=Mount("v4")
m=h.mount
count=0
def passed(message):
    global count
    count+=1
    print(f"PASS {count:02}: {message}",flush=True)
def tree(source,destination,dry=False,success=True):
    result=h.cli("restore-tree",m,"baseline",source,destination,*(["--dry-run"] if dry else []),"--json",success=success)
    return json.loads(result.stdout) if success else result
def bytes_in_database(conn,name):
    return conn.execute("SELECT b.data FROM nodes n JOIN blobs b ON n.content_id=b.id WHERE n.scope='' AND n.name=?",(os.fsencode(name),)).fetchone()[0]

try:
    print("TeamFS v0.4 real mounted acceptance",time.strftime("%Y-%m-%dT%H:%M:%S%z"),flush=True)
    h.start()
    folder=m/"课程报告"
    (folder/"资料").mkdir(parents=True)
    note=folder/"资料/正文.bin"
    data=bytes(range(256))*200
    note.write_bytes(data)
    note.chmod(0o640)
    os.utime(note,(1700000000,1700000001))
    (folder/"empty").touch()
    raw=folder/os.fsdecode(b"raw-\xff\n")
    raw.write_bytes(b"raw")
    os.symlink("资料/正文.bin",folder/"relative")
    os.symlink("not-yet-created",folder/"dangling")
    assert (folder/"relative").read_bytes()==data and os.readlink(folder/"dangling")=="not-yet-created"
    h.snapshot("baseline")
    old=h.status()
    preview=tree("课程报告","restored",True)
    assert preview["can_restore"] and preview["files"]==3 and preview["directories"]==2 and preview["symlinks"]==2
    assert not (m/"restored").exists()
    assert h.status()["dirty"]==old["dirty"] and h.status()["read_calls"]==old["read_calls"]
    result=tree("课程报告","restored")
    assert result["can_restore"] and (m/"restored/资料/正文.bin").read_bytes()==data
    assert (m/"restored/资料/正文.bin").stat().st_mode&0o777==0o640
    assert int((m/"restored/资料/正文.bin").stat().st_mtime)==1700000001
    assert os.readlink(m/"restored/relative")=="资料/正文.bin"
    assert (m/"restored"/os.fsdecode(b"raw-\xff\n")).read_bytes()==b"raw"
    assert (m/"restored/资料/正文.bin").stat().st_ino!=note.stat().st_ino
    passed("directory preview and whole-tree restore preserve hierarchy, raw names, bytes, permissions and links")

    before=h.status()["files"]
    conflict=tree("课程报告","restored",True)
    assert not conflict["can_restore"] and conflict["error_errno"]==errno.EEXIST
    tree("课程报告","restored",success=False)
    for destination in ["../escape",".teamfs/escape","missing/child","/absolute"]:
        tree("课程报告",destination,success=False)
    assert h.status()["files"]==before
    h.sync()
    tree(".","entire-root")
    assert (m/"entire-root/课程报告/资料/正文.bin").read_bytes()==data
    assert not (m/"entire-root/.teamfs").exists()
    h.stop(kill=True)
    h.start()
    assert not (m/"entire-root").exists() and (m/"restored/资料/正文.bin").read_bytes()==data
    passed("conflicts and bad paths leave no partial tree; root excludes management data; unsynced restore rolls back on crash")

    # Link metadata and payload survive persistence, snapshots and the trash.
    assert os.readlink(folder/"relative")=="资料/正文.bin"
    assert os.readlink(m/".teamfs/snapshots/baseline/课程报告/relative")=="资料/正文.bin"
    (folder/"relative").unlink()
    link_record=h.trash()[-1]
    h.cli("trash","restore",m,link_record["id"],"课程报告/link-restored")
    assert os.readlink(folder/"link-restored")=="资料/正文.bin"
    h.sync()
    passed("relative/dangling symlinks survive restart, snapshot and trash recovery without dereferencing targets")

    # A fresh backup is independent of the store and includes all committed history.
    backup=h.base/"backup"
    h.cli("backup","create",m,backup)
    h.cli("backup","verify",backup)
    assert sorted(p.name for p in backup.iterdir())==["manifest.json","state.sqlite3"]
    manifest=json.loads((backup/"manifest.json").read_text())
    assert manifest["database_bytes"]>0
    h.cli("backup","create",m,backup,success=False)
    h.cli("backup","create",m,m/"bad-backup",success=False)
    imported=h.base/"imported"
    h.cli("backup","import",backup,"--store",imported)
    h.cli("backup","import",backup,"--store",imported,success=False)
    h.stop()
    original_store=h.store
    h.store=imported
    h.start()
    assert note.read_bytes()==data and len(h.trash())==1
    assert (m/".teamfs/snapshots/baseline/课程报告/资料/正文.bin").read_bytes()==data
    h.stop()
    broken=h.base/"broken-backup"
    shutil.copytree(backup,broken)
    with (broken/"state.sqlite3").open("r+b") as f:
        f.seek(-1,2);f.write(b"\xaa")
    h.cli("backup","verify",broken,success=False)
    failed=h.base/"failed-import"
    h.cli("backup","import",broken,"--store",failed,success=False)
    assert not failed.exists() and not list(h.base.glob(".teamfs-stage-*"))
    passed("standalone checksummed backups restore files/history into new stores; corrupt or existing targets are refused atomically")

    h.store=original_store
    h.start()
    db=sqlite3.connect(h.store/"state.sqlite3")
    db.execute("CREATE TRIGGER reject_save BEFORE INSERT ON nodes BEGIN SELECT RAISE(ABORT,'backup sync failure'); END")
    db.commit()
    (m/"pending-backup").write_bytes(b"pending")
    blocked=h.base/"blocked-backup"
    h.cli("backup","create",m,blocked,success=False)
    assert not blocked.exists() and h.status()["dirty"]
    db.execute("DROP TRIGGER reject_save");db.commit();db.close()
    h.sync()
    passed("backup requires a successful initial sync and never publishes a backup on sync failure")

    # Actual tools, not synthetic replacements of their file operations.
    editor=m/"editor.txt"
    editor.write_text("draft\nsecond line\n")
    subprocess.run(["vim","-Nu","NONE","-n","-es",str(editor),"-c","%s/draft/final/g","-c","wq"],check=True,timeout=15)
    assert editor.read_text()=="final\nsecond line\n"
    subprocess.run(["cp","-a",str(folder),str(m/"copied")],check=True,timeout=20)
    assert (m/"copied/资料/正文.bin").read_bytes()==data
    assert os.readlink(m/"copied/link-restored")=="资料/正文.bin"
    archive=h.base/"files.tar"
    subprocess.run(["tar","-cf",str(archive),"-C",str(m),"课程报告"],check=True,timeout=20)
    (m/"unpacked").mkdir()
    subprocess.run(["tar","-xf",str(archive),"-C",str(m/"unpacked")],check=True,timeout=20)
    assert (m/"unpacked/课程报告/资料/正文.bin").read_bytes()==data
    assert os.readlink(m/"unpacked/课程报告/dangling")=="not-yet-created"
    passed("Vim actually edits/saves; cp -a and tar round-trip nested files and symbolic links")

    editor.chmod(0o000)
    fails({errno.EACCES},lambda:editor.read_bytes())
    editor.chmod(0o600)
    held=os.open(editor,os.O_RDWR)
    fcntl.flock(held,fcntl.LOCK_EX|fcntl.LOCK_NB)
    child=subprocess.run(["python3","-c","import os,fcntl,sys; f=os.open(sys.argv[1],os.O_RDWR)\ntry: fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)\nexcept BlockingIOError: sys.exit(0)\nsys.exit(1)",str(editor)])
    assert child.returncode==0
    fcntl.flock(held,fcntl.LOCK_UN);os.close(held)
    log=m/"append.txt";log.touch()
    code="import os,sys; f=os.open(sys.argv[1],os.O_WRONLY|os.O_APPEND)\nfor i in range(100): os.write(f,(sys.argv[2]+':'+str(i)+'\\n').encode())\nos.close(f)"
    children=[subprocess.Popen(["python3","-c",code,str(log),str(i)]) for i in range(4)]
    assert all(p.wait(timeout=20)==0 for p in children)
    lines=log.read_text().splitlines()
    assert len(lines)==400 and set(lines)=={f"{i}:{j}" for i in range(4) for j in range(100)}
    h.sync();h.stop(kill=True);h.start()
    assert editor.read_text()=="final\nsecond line\n" and len(log.read_text().splitlines())==400
    h.stop()
    passed("owner permissions, local advisory flock and four-process append workloads behave correctly and persist")

    h.start("--cached-io")
    mapped=m/"mapped.bin";mapped.write_bytes(b"A"*8192)
    fd=os.open(mapped,os.O_RDWR)
    mm=mmap.mmap(fd,8192)
    mm[4096:4100]=b"MMAP"
    mm.flush();os.fsync(fd);mm.close();os.close(fd)
    assert mapped.read_bytes()[4096:4100]==b"MMAP"
    h.stop(kill=True);h.start("--cached-io")
    assert mapped.read_bytes()[4096:4100]==b"MMAP"
    h.stop()
    passed("optional cached I/O supports shared mmap, msync/fsync and persistence; default direct mode remains available")

    # Capacity preflight must fail before publishing any new directory.
    h.store=h.base/"limits"
    h.start("--capacity-mib",2,"--max-file-mib",1)
    (m/"source-dir").mkdir()
    (m/"source-dir/a").write_bytes(b"a"*(700*1024))
    h.snapshot("baseline")
    (m/"filler").write_bytes(b"x"*(900*1024))
    before=h.status()
    preview=tree("source-dir","oversized",True)
    assert not preview["can_restore"] and preview["error_errno"]==errno.ENOSPC
    tree("source-dir","oversized",success=False)
    assert not (m/"oversized").exists() and h.status()["files"]==before["files"]
    h.stop()
    h.start()
    assert h.status()["capacity_bytes"]==2*1024*1024 and h.status()["max_file_bytes"]==1024*1024
    h.stop()
    passed("whole-directory capacity preflight is atomic; configured limits persist across remount")

    # Data set exceeds the old 64MiB cap, without loading all contents during startup.
    h.store=h.base/"large"
    h.start("--capacity-mib",256,"--max-file-mib",32)
    for i in range(6):
        with (m/f"large-{i}").open("wb") as f:
            for _ in range(16):f.write(bytes([i])*1024*1024)
    (m/"small").write_bytes(b"a"*4096)
    h.snapshot("baseline")
    h.sync();h.stop();h.start()
    start=h.status()
    assert start["used_bytes"]>64*1024*1024 and start["cache_bytes"]==0 and start["content_fetched_bytes"]==0
    assert start["resident_unsaved_content_bytes"]==0
    fd=os.open(m/"large-4",os.O_RDONLY)
    assert os.pread(fd,4096,123456)==bytes([4])*4096
    os.close(fd)
    assert h.status()["content_fetched_bytes"]<=128*1024
    with (m/"large-0").open("rb") as f:
        while f.read(1024*1024):pass
    assert h.status()["cache_bytes"]<=8*1024*1024
    (m/"small").write_bytes(b"b"*4096)
    h.sync()
    state=h.status()
    assert state["last_commit_content_bytes"]==4096
    assert (m/".teamfs/snapshots/baseline/small").read_bytes()==b"a"*4096
    h.sync()
    assert h.status()["last_commit_content_bytes"]==0
    db=sqlite3.connect(h.store/"state.sqlite3")
    db.execute("CREATE TRIGGER content_fail BEFORE INSERT ON blobs BEGIN SELECT RAISE(ABORT,'content failure'); END");db.commit()
    (m/"small").write_bytes(b"c"*4096)
    h.cli("sync",m,success=False)
    assert (m/"small").read_bytes()==b"c"*4096 and bytes_in_database(db,"small")==b"b"*4096
    assert h.status()["dirty"]
    db.execute("DROP TRIGGER content_fail");db.commit();db.close()
    h.sync();h.stop(kill=True);h.start()
    assert (m/"small").read_bytes()==b"c"*4096
    h.stop()
    passed("96MiB data starts without content reads, 8MiB cache stays bounded, 4KiB change saves 4KiB; failed content commits rollback and retry")

    # Verify an actual v2 store produced by the v0.3 binary.
    old=os.environ.get("TEAMFS_V03_BIN")
    if old:
        h.store=h.base/"v2-real"
        h.start(binary=Path(old))
        (m/"old").write_bytes(b"v2-data")
        h.snapshot("baseline")
        (m/"old").unlink()
        h.sync();h.stop()
        h.start()
        assert (m/".teamfs/snapshots/baseline/old").read_bytes()==b"v2-data"
        assert (m/".teamfs/trash"/str(h.trash()[-1]["id"])).read_bytes()==b"v2-data"
        assert h.status()["content_fetched_bytes"]>0
        h.stop()
        passed("authentic v0.3 format-2 database migrates atomically with snapshot and trash contents")
    h.start("--memory")
    h.cli("backup","create",m,h.base/"memory-backup",success=False)
    h.stop()
    print(f"ALL PASS: {count} v4 mounted scenarios",flush=True)
finally:
    h.close()
