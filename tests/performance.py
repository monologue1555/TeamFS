"""Same fixture and operations before/after; report samples, not a universal benchmark."""
import json
import os
from pathlib import Path
import sqlite3
import statistics
import time
from fs_harness import Mount, ROOT, BINARY

def measure(binary,label):
    h=Mount("perf-"+label)
    m=h.mount
    try:
        h.start(binary=binary)
        for i in range(4):
            (m/f"large-{i}").touch()
            os.truncate(m/f"large-{i}",8*1024*1024)
        small=m/"small"
        small.write_bytes(b"a"*4096)
        h.snapshot("one");h.snapshot("two");h.stop()
        h.start(binary=binary)
        proc=Path(f"/proc/{h.process.pid}/status").read_text()
        rss=int(next(line.split()[1] for line in proc.splitlines() if line.startswith("VmRSS:")))*1024
        state=h.status()
        db=sqlite3.connect(h.store/"state.sqlite3")
        version=db.execute("SELECT version FROM metadata").fetchone()[0]
        db.execute("CREATE TABLE measurement_payload(n INTEGER)")
        table="blobs" if version==3 else "nodes"
        db.execute(f"CREATE TRIGGER measurement_insert AFTER INSERT ON {table} BEGIN INSERT INTO measurement_payload VALUES(length(NEW.data)); END")
        db.commit()
        samples=[]
        for i in range(3):
            small.write_bytes(bytes([i])*4096)
            db.execute("DELETE FROM measurement_payload");db.commit()
            begin=time.perf_counter();h.sync();seconds=time.perf_counter()-begin
            inserted=db.execute("SELECT coalesce(sum(n),0) FROM measurement_payload").fetchone()[0]
            samples.append({"sync_command_seconds":seconds,"sql_payload_insert_bytes":inserted})
        assert (m/".teamfs/snapshots/one/small").read_bytes()==b"a"*4096
        assert small.read_bytes()==bytes([2])*4096
        db.close();h.stop()
        return {"version":state.get("version",label),"format":version,"startup_rss_bytes":rss,
            "startup_content_fetched_bytes":state.get("content_fetched_bytes"),
            "startup_cache_bytes":state.get("cache_bytes"),"samples":samples,
            "median_sync_seconds":statistics.median(s["sync_command_seconds"] for s in samples),
            "backend_bytes_after_unmount":sum(p.stat().st_size for p in h.store.iterdir() if p.is_file())}
    finally:h.close()

baseline=os.environ.get("TEAMFS_V03_BIN")
if not baseline:raise SystemExit("Set TEAMFS_V03_BIN to an existing v0.3 binary for the comparison.")
result={"measured_at":time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    "fixture":"4 x 8MiB files, one 4KiB file, seed files, two complete snapshots; modify only the 4KiB file",
    "notes":"Three local samples including CLI overhead. SQL payload bytes are not physical device-write bytes. RSS includes process and SQLite overhead.",
    "before":measure(Path(baseline),"0.3"),"after":measure(BINARY,"0.4")}
assert all(s["sql_payload_insert_bytes"]==4096 for s in result["after"]["samples"])
(ROOT/"artifacts/performance-v4.json").write_text(json.dumps(result,ensure_ascii=False,indent=2)+"\n")
print(json.dumps(result,ensure_ascii=False,indent=2),flush=True)
