"""Repeatable, bounded workloads. No cache dropping or production paths are used."""
import argparse
import json
import os
from pathlib import Path
import random
import shutil
import statistics
import subprocess
import time
from fs_harness import Mount, ROOT
from reporting import Report, EnvironmentSkip

parser=argparse.ArgumentParser()
parser.add_argument("--samples",type=int,default=3)
args=parser.parse_args()
if not 1<=args.samples<=20:parser.error("samples must be 1..20")
report=Report("baseline-v6")
fio=os.environ.get("TEAMFS_FIO") or shutil.which("fio")
cached=Path.home()/".cache/teamfs-tools/fio-3.39/fio"
if not fio and cached.is_file():fio=str(cached)
report.context={"samples_per_workload":args.samples,"build":"debug", "io_engine":"psync, queue depth 1 per job",
    "cache_policy":"no host cache drop; includes warm/uncontrolled OS caches; fio invalidate disabled consistently",
    "audit":"file logger and trace disabled; runtime metrics still enabled on TeamFS",
    "meaning":"fio completion latency and end-to-end CLI wall time are different metrics; no general performance ranking inferred"}
if fio:report.context["fio_version"]=subprocess.check_output([fio,"--version"],text=True).strip()


def measured(action):
    start=time.perf_counter_ns();action();return (time.perf_counter_ns()-start)/1e6


def fio_case(root,label,rw,bs,jobs):
    if not fio:raise EnvironmentSkip("fio not installed; set TEAMFS_FIO to the actual fio executable")
    samples=[]
    # Precreate identical per-job fixtures; no writes to any path outside the private root.
    for i in range(jobs):(root/f"fio-{i}").write_bytes(bytes([i+31])*4*1024*1024)
    for sample in range(args.samples):
        output=ROOT/"artifacts"/f"fio-{label}-{rw}-{jobs}-{sample}.json"
        command=[fio,"--name=teamfs-baseline","--directory="+str(root),"--filename_format=fio-$jobnum",
            "--rw="+rw,"--bs="+bs,"--size=4m","--io_size=4m","--numjobs="+str(jobs),"--ioengine=psync",
            "--iodepth=1","--direct=0","--invalidate=0","--fallocate=none","--randrepeat=1","--randseed=1555",
            "--group_reporting=1","--output-format=json","--output="+str(output)]
        if rw=="write":command += ["--verify=crc32c","--do_verify=1","--end_fsync=1"]
        result=subprocess.run(command,capture_output=True,timeout=45,cwd=root.parent)
        assert result.returncode==0,(command,result.stdout,result.stderr,output)
        value=json.loads(output.read_text());assert all(j["error"]==0 for j in value["jobs"])
        job=value["jobs"][0];direction=job["write"] if rw=="write" else job["read"]
        samples.append({"iops":direction["iops"],"bandwidth_bytes_per_sec":direction["bw_bytes"],
            "completion_latency_ns_mean":direction["clat_ns"]["mean"],
            "completion_latency_ns_p95":direction["clat_ns"].get("percentile",{}).get("95.000000"),
            "total_io_bytes":direction["io_bytes"],"raw_result":output.name})
    return {"fixture":"4MiB per job", "jobs":jobs,"operation":rw,"block_size":bs,"samples":samples,
        "median_iops":statistics.median(x["iops"] for x in samples)}


def metadata_case(root):
    samples=[]
    for sample in range(args.samples):
        directory=root/f"small-{sample}";directory.mkdir()
        def create():
            for i in range(150):(directory/f"file-{i:04}").write_bytes(b"x"*64)
        create_ms=measured(create)
        def scan():
            entries=list(directory.iterdir());assert len(entries)==150
            assert sum(p.stat().st_size for p in entries)==150*64
        scan_ms=measured(scan)
        def remove():
            for p in directory.iterdir():p.unlink()
            directory.rmdir()
        delete_ms=measured(remove)
        samples.append(dict(create_ms=create_ms,scan_stat_ms=scan_ms,delete_ms=delete_ms))
    return {"fixture":"150 files x 64B per repetition; TeamFS deletions include trash protection", "samples":samples}


def persistence_case(h):
    root=h.mount
    big=root/"patch-large";big.write_bytes(b"A"*(16*1024*1024));h.sync()
    small=root/"patch-small";small.write_bytes(b"A"*4096);h.sync()
    rows=[]
    for file in [small,big]:
        for i in range(args.samples):
            fd=os.open(file,os.O_WRONLY)
            start=time.perf_counter_ns()
            try:os.pwrite(fd,bytes([66+i])*4096,0)
            finally:os.close(fd)
            write_ms=(time.perf_counter_ns()-start)/1e6
            sync_ms=measured(h.sync);s=h.status()
            rows.append({"file_bytes":file.stat().st_size,"changed_bytes":4096,"write_ms":write_ms,
                "sync_cli_ms":sync_ms,"inserted_payload_bytes":s["last_commit_content_bytes"],"changed_rows":s["last_commit_rows"]})
    # Waiting under an actual SQLite writer lock must be visible as commit time.
    import sqlite3,threading
    acquired=threading.Event()
    def lock_database():
        db=sqlite3.connect(h.store/"state.sqlite3");db.execute("BEGIN IMMEDIATE");acquired.set();time.sleep(.15);db.rollback();db.close()
    small.write_bytes(b"locked")
    thread=threading.Thread(target=lock_database);thread.start();assert acquired.wait(5)
    try:waited=measured(h.sync)
    finally:thread.join()
    metrics=json.loads(h.cli("metrics",h.mount,"--json").stdout)
    assert next(x for x in metrics["operations"] if x["operation"]=="sync.commit")["max_us"]>=100_000
    return {"samples":rows,"controlled_store_lock_sync_cli_ms":waited,
        "note":"SQL payload is not physical device I/O. Controlled 150ms writer lock is an injected slow-path example, excluded from throughput samples.","metrics":metrics}


for label in ["native","direct","cached"]:
    h=Mount("baseline-"+label)
    try:
        if label=="native":root=h.base/"native";root.mkdir()
        else:
            h.start("--no-audit-file",*(["--cached-io"] if label=="cached" else []),trace=False)
            root=h.mount
            report.context[label+"_version"]=h.status()["version"]
        for rw,bs,jobs in [("write","64k",1),("randread","4k",1),("randread","4k",4)]:
            report.case(f"{label} fio {rw} {bs} jobs={jobs}",lambda:fio_case(root,label,rw,bs,jobs))
        report.case(label+" small-file lifecycle",lambda:metadata_case(root))
        if label!="native":
            report.case(label+" persistence and controlled slow commit",lambda:persistence_case(h))
            h.sync();report.context[label+"_status"]=h.status();h.stop()
            report.context[label+"_storage"]=json.loads(h.cli("check",h.store,"--json").stdout)["storage"]
    finally:h.close()
report.finish()
