"""Offline inspection, phase timings, and management/transaction failure combinations."""
import errno
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import time
import threading
from fs_harness import Mount, ROOT, fails
from reporting import Report

report=Report("v6-acceptance")
h=Mount("v6")
m=h.mount


def metrics():return json.loads(h.cli("metrics",m,"--json").stdout)


def timing_checks():
    h.start(trace=False)
    f=m/"timing";f.write_bytes(b"x"*8192)
    fd=os.open(f,os.O_RDWR)
    try:os.fsync(fd)
    finally:os.close(fd)
    f.read_bytes()
    before=h.status();a=metrics()
    assert any(e["operation"]=="fuse.write" for e in a["operations"])
    assert any(e["operation"]=="sync.commit" for e in a["operations"])
    fsync=next(e for e in a["operations"] if e["operation"]=="fuse.fsync")
    assert fsync["commit_total_us"]>0 and fsync["service_total_us"]>=fsync["commit_total_us"]
    for _ in range(8):metrics();h.status()
    assert metrics()==a and h.status()["dirty"]==before["dirty"]
    fd=os.open(m/".teamfs/metrics.json",os.O_RDONLY)
    try:
        saved=os.read(fd,13);f.write_bytes(b"new")
        while True:
            chunk=os.read(fd,73)
            if not chunk:break
            saved+=chunk
    finally:os.close(fd)
    assert json.loads(saved)==a and metrics()["recorded"]>a["recorded"]
    fails({errno.EACCES,errno.EROFS},lambda:(m/".teamfs/metrics.json").write_bytes(b"bad"))
    event=json.loads((m/".teamfs/events.json").read_bytes())["events"]
    w=next(e for e in event if e["operation"]=="write")
    assert set(w["arguments"]["timing_us"])=={"total_us","lock_wait_us","service_us","commit_us"}
    (ROOT/"artifacts/metrics-example.json").write_text(json.dumps(metrics(),ensure_ascii=False,indent=2)+"\n")


def busy_check_and_doctor():
    result=h.cli("check",h.store,"--json",success=False)
    value=json.loads(result.stdout);assert not value["ok"]
    assert "正在使用" in value["checks"][0]["detail"]
    value=json.loads(h.cli("doctor",m,"--store",h.store,"--json").stdout)
    assert value["ok"] and any(c["name"]=="store_lock" and c["status"]=="warning" for c in value["checks"])
    assert h.process.poll() is None
    h.cli("doctor",m,"--store",m/"invalid-store","--json",success=False)
    assert not (m/"invalid-store").exists()


def offline_checks():
    h.sync();h.stop()
    database=h.store/"state.sqlite3"
    baseline=hashlib.sha256(database.read_bytes()).hexdigest()
    v=json.loads(h.cli("check",h.store,"--json").stdout)
    assert v["ok"] and v["storage"]["format"]==3
    assert hashlib.sha256(database.read_bytes()).hexdigest()==baseline
    (ROOT/"artifacts/check-example.json").write_text(json.dumps(v,ensure_ascii=False,indent=2)+"\n")
    missing=h.base/"nonexistent"
    v=json.loads(h.cli("check",missing,"--json",success=False).stdout)
    assert not v["ok"] and not missing.exists()
    # Work exclusively on copies; no bad fixture is ever mounted.
    for label,sql in [
        ("unknown","UPDATE metadata SET version=999"),
        ("missing_blob","DELETE FROM blobs WHERE id=(SELECT content_id FROM nodes WHERE name=X'74696D696E67' AND scope='')"),
        ("orphan_scope","UPDATE nodes SET scope='missing-tree' WHERE scope='' AND name=X'74696D696E67'")]:
        dest=h.base/label;shutil.copytree(h.store,dest)
        db=sqlite3.connect(dest/"state.sqlite3");db.execute(sql);db.commit();db.close()
        digest=hashlib.sha256((dest/"state.sqlite3").read_bytes()).hexdigest()
        value=json.loads(h.cli("check",dest,"--json",success=False).stdout)
        assert not value["ok"] and hashlib.sha256((dest/"state.sqlite3").read_bytes()).hexdigest()==digest
    dest=h.base/"corrupt";shutil.copytree(h.store,dest)
    (dest/"state.sqlite3").write_bytes(b"invalid SQLite header")
    value=json.loads(h.cli("check",dest,"--json",success=False).stdout)
    assert not value["ok"] and (dest/"state.sqlite3").read_bytes()==b"invalid SQLite header"
    # Unused content is a warning, not falsely reported as corruption, and is not deleted.
    db=sqlite3.connect(database);db.execute("INSERT INTO blobs(data) VALUES(?)",(b"unused",));db.commit();db.close()
    value=json.loads(h.cli("check",h.store,"--json").stdout)
    assert value["storage"]["unreferenced_blob_bytes"]>=6
    db=sqlite3.connect(database);assert db.execute("SELECT count(*) FROM blobs WHERE data=?",(b"unused",)).fetchone()[0]==1;db.close()


def transaction_failure(table,operation):
    h.store=h.base/f"fault-{table}-{operation}"
    h.start(trace=False)
    f=m/"old";f.write_bytes(b"A");h.snapshot("before")
    (m/"trashme").write_bytes(b"T");(m/"trashme").unlink();h.sync()
    old_ids=[r["id"] for r in h.trash()]
    db=sqlite3.connect(h.store/"state.sqlite3")
    statement="UPDATE" if table=="metadata" else "INSERT"
    db.execute(f"CREATE TRIGGER fail_tx BEFORE {statement} ON {table} BEGIN SELECT RAISE(ABORT,'v6 injected {table} failure'); END")
    db.commit()
    f.write_bytes(b"B");(m/"new").write_bytes(b"pending")
    args={"sync":["sync",m],"snapshot":["snapshot","create",m,"failed"],"purge":["trash","purge",m,"--all"]}[operation]
    h.cli(*args,success=False)
    assert h.status()["dirty"] and "v6 injected" in h.status()["last_sync_error"]
    assert [r["id"] for r in h.trash()]==old_ids
    assert not (m/".teamfs/snapshots/failed").exists()
    assert db.execute("SELECT b.data FROM nodes n JOIN blobs b ON b.id=n.content_id WHERE n.scope='' AND n.name=?",(b"old",)).fetchone()[0]==b"A"
    # Remove the test-only trigger, then kill without retry: the old complete commit survives.
    db.execute("DROP TRIGGER fail_tx");db.commit();db.close()
    h.stop(kill=True);h.start(trace=False)
    assert f.read_bytes()==b"A" and not (m/"new").exists()
    assert [r["id"] for r in h.trash()]==old_ids
    f.write_bytes(b"C");h.cli(*args)
    h.stop(kill=True);h.start(trace=False)
    assert f.read_bytes()==b"C"
    if operation=="snapshot":assert (m/".teamfs/snapshots/failed/old").read_bytes()==b"C"
    if operation=="purge":assert not h.trash()
    stats=metrics();assert all(o["calls"]>=o["errors"] for o in stats["operations"])
    h.stop()
    return {"failure_point":table,"command":operation,"failed_commit":"rolled back","retry":"persisted across SIGKILL"}



def writer_contention():
    h.store=h.base/"contention";h.start(trace=False)
    f=m/"held";f.write_bytes(b"A");h.sync()
    outcomes=[]
    for seconds,success in [(.2,True),(2.5,False)]:
        f.write_bytes(b"B" if success else b"C")
        acquired=threading.Event()
        def hold():
            db=sqlite3.connect(h.store/"state.sqlite3");db.execute("BEGIN IMMEDIATE");acquired.set()
            time.sleep(seconds);db.rollback();db.close()
        worker=threading.Thread(target=hold);worker.start();assert acquired.wait(5)
        began=time.perf_counter()
        try:h.cli("sync",m,success=success)
        finally:worker.join(timeout=5)
        elapsed=time.perf_counter()-began
        if not success:
            assert h.status()["dirty"] and h.status()["last_sync_error"]
            db=sqlite3.connect(h.store/"state.sqlite3")
            assert db.execute("SELECT b.data FROM nodes n JOIN blobs b ON b.id=n.content_id WHERE n.scope='' AND n.name=?",(b"held",)).fetchone()[0]==b"B"
            db.close();h.sync()
        outcomes.append({"writer_lock_seconds":seconds,"sync_succeeded":success,"seconds_including_release_wait":elapsed})
    assert not h.status()["dirty"]
    h.stop(kill=True);h.start(trace=False);assert f.read_bytes()==b"C";h.stop()
    return outcomes

def lifecycle_soak():
    h.store=h.base/"soak";h.start(trace=False)
    samples=[]
    for i in range(120):
        f=m/"cycling";f.write_bytes(bytes([i])*4096)
        fd=os.open(f,os.O_RDWR);f.unlink();os.pwrite(fd,b"later",0)
        record=h.trash()[-1];held=os.open(m/".teamfs/trash"/str(record["id"]),os.O_RDONLY)
        h.purge()
        assert os.read(held,4096)==bytes([i])*4096
        os.close(held);os.close(fd)
        if i%20==0:
            h.snapshot("cycle");h.cli("snapshot","delete",m,"cycle")
            h.sync();s=h.status();assert s["runtime"]["open_handles"]==0,s["runtime"]
            assert s["resident_unsaved_content_bytes"] <= (i+1)*4096
            samples.append({"iteration":i,"runtime":s["runtime"],"resident_unsaved_content_bytes":s["resident_unsaved_content_bytes"]})
    h.sync();h.stop()
    before=json.loads(h.cli("check",h.store,"--json").stdout)["storage"]
    h.start(trace=False);assert h.status()["resident_unsaved_content_bytes"]==0;h.stop()
    after=json.loads(h.cli("check",h.store,"--json").stdout)["storage"]
    assert after["unreferenced_blobs"]==0
    return {"iterations":120,"samples":samples,"unreferenced_bytes_before_restart":before["unreferenced_blob_bytes"],"unreferenced_bytes_after_restart":after["unreferenced_blob_bytes"],"scope":"bounded lifecycle exercise, not a long-term reliability claim"}


try:
    report.case("phase metrics, frozen reads, read-only and no self-observation",timing_checks)
    report.case("busy store rejection and environment/mount diagnostics",busy_check_and_doctor)
    report.case("offline integrity, corrupt/unknown/missing-reference refusal, no data mutation",offline_checks)
    for table in ["blobs","nodes","metadata"]:
        for operation in ["sync","snapshot","purge"]:
            report.case(f"transaction fault {table} / {operation}",lambda:transaction_failure(table,operation))
    report.case("SQLite writer contention waits, timeout retains dirty and retry saves",writer_contention)
    report.case("120 lifecycle rounds: open/unlink/purge/close and retired-content reclamation",lifecycle_soak)
finally:h.close()
report.finish()
