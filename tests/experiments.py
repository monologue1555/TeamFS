"""Real reproducible data inputs; no simulated FUSE callbacks or production claims."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import sqlite3
import subprocess
import time
from fs_harness import Mount, ROOT, BINARY, wait_for

h = Mount("experiments")
m = h.mount
checks = []
evidence = {}

def passed(message):
    checks.append(message)
    print(f"PASS {len(checks):02}: {message}", flush=True)

def run(name, *command, success=True):
    output = h.base / ("report-" + name)
    result = h.cli("experiment", "run", m, name, "--output", output, "--", *command, success=success)
    return output, json.loads((output / "manifest.json").read_bytes()), result

def inputs(report):
    return {bytes(row["path_bytes"]): row for row in report["capture"]["inputs"]}

def delete(name):
    h.cli("experiment", "delete", m, name)

try:
    h.start("--no-audit-file")
    for source in (ROOT / "examples/research").iterdir():
        if source.is_file():
            (m / source.name).write_bytes(source.read_bytes())
    first, report, result = run("baseline", "python3", "analyze.py")
    assert report["phase"] == "finished" and report["capture_complete"] and report["exit_code"] == 0
    paths = inputs(report)
    assert set(paths) == {b"analyze.py", b"measurements.csv", b"parameters.json", b"calibration.txt"}
    assert all(row["access_class"] == "content_read" for row in paths.values())
    for path, row in paths.items():
        assert row["sha256"] == hashlib.sha256((m / os.fsdecode(path)).read_bytes()).hexdigest()
    first_result = (first / "outputs/result.json").read_bytes()
    assert json.loads(first_result)["calibrated_mean"] == 41.5
    h.cli("experiment", "verify", m, first)
    assert json.loads(h.cli("experiment", "show", first).stdout) == report
    (ROOT / "artifacts/experiment-manifest-example.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    evidence["baseline"] = json.loads(first_result)
    passed("real analysis records its source and three consumed inputs, excludes unused data, stores SHA256 and command outcome")

    (m / "measurements.csv").write_text("sample,value\na,50\nb,60\nc,70\n")
    second, changed, _ = run("changed", "python3", "analyze.py")
    assert json.loads((second / "outputs/result.json").read_bytes())["calibrated_mean"] == 121.5
    replay = h.base / "report-replay"
    h.cli("experiment", "replay", m, first, "replay", "--output", replay)
    replay_report = json.loads((replay / "manifest.json").read_bytes())
    assert (replay / "outputs/result.json").read_bytes() == first_result
    assert inputs(replay_report)[b"measurements.csv"]["sha256"] == paths[b"measurements.csv"]["sha256"]
    assert changed["capture"]["inputs"] != report["capture"]["inputs"]
    evidence["changed"] = json.loads((second / "outputs/result.json").read_bytes())
    evidence["replayed"] = json.loads(first_result)
    passed("same-named current data changes the result; replaying pinned code/data returns the original byte-exact result")

    h.cli("snapshot", "delete", m, "experiment.baseline", success=False)
    h.stop(kill=True); h.start("--cached-io", "--no-audit-file")
    h.cli("experiment", "verify", m, first)
    h.cli("experiment", "restore", m, "baseline", "old-inputs")
    assert (m / "old-inputs/measurements.csv").read_text() == "sample,value\na,10\nb,20\nc,30\n"
    h.sync()
    assert h.status()["experiments"] == []  # Reports are durable outside the daemon, not reconstructed runtime counters.
    passed("successful input snapshots survive daemon SIGKILL, normal snapshot delete is refused, and old inputs restore to a new directory")
    delete("changed"); delete("replay")

    raw = b"raw-\xff\n.bin"
    binary = bytes(range(256)) * 16
    (m / os.fsdecode(raw)).write_bytes(binary)
    (m / "empty").touch(); (m / "opened-only").write_bytes(b"not-read")
    # Publish cached writes before snapshot creation, as required by the existing cached-I/O contract.
    for name in (os.fsdecode(raw), "empty", "opened-only"):
        fd = os.open(m / name, os.O_RDONLY); os.fsync(fd); os.close(fd)
    code = "import os; f=os.open(b'raw-\\xff\\n.bin',os.O_RDONLY); assert os.pread(f,2,5)==bytes([5,6]); os.close(f); f=os.open('empty',os.O_RDONLY); assert os.read(f,1)==b''; os.close(f); f=os.open('opened-only',os.O_RDONLY); os.close(f); os.stat('unused.csv')"
    output, binary_report, _ = run("binary", "python3", "-c", code)
    rows = inputs(binary_report)
    assert rows[raw]["sha256"] == hashlib.sha256(binary).hexdigest()
    assert rows[raw]["read_bytes"] == 2 and rows[raw]["read_calls"] == 1
    assert rows[b"empty"]["sha256"] == hashlib.sha256(b"").hexdigest()
    assert rows[b"opened-only"]["access_class"] == "opened_only"
    assert b"unused.csv" not in rows
    assert any(bytes(row["path_bytes"]) == b"unused.csv" for row in binary_report["capture"]["metadata_accesses"])
    h.cli("experiment", "verify", m, output)
    passed("partial/binary/non-UTF8/empty inputs are exact; opened-only and metadata-only accesses remain distinct in cached mount mode")
    delete("binary")

    output, failed, failed_result = run("failed", "python3", "-c", "open('calibration.txt').read(); raise SystemExit(7)", success=False)
    assert failed_result.returncode == 7 and failed["capture_complete"] and failed["exit_code"] == 7
    assert b"calibration.txt" in inputs(failed)
    assert (output / "stderr.txt").exists()
    passed("command failure preserves its input evidence and original exit code instead of pretending success")
    delete("failed")

    # More reads than the audit ring retains, plus another process reading this snapshot.
    out = h.base / "report-many"
    code = "import os,time; from pathlib import Path; Path(os.environ['TEAMFS_OUTPUT'],'ready').touch(); f=os.open('calibration.txt',os.O_RDONLY);\nfor i in range(2499): assert os.pread(f,1,0)==b'1'\ntime.sleep(.4); assert os.pread(f,1,0)==b'1'; os.close(f)"
    job = subprocess.Popen([str(BINARY), "experiment", "run", str(m), "many", "--output", str(out), "--", "python3", "-c", code], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    wait_for(lambda: (out / "outputs/ready").exists())
    (m / "calibration.txt").write_text("9.9\n")
    fd = os.open(m / "calibration.txt", os.O_RDONLY); os.fsync(fd); os.close(fd)
    for _ in range(10):
        (m / ".teamfs/snapshots/experiment.many/unused.csv").read_bytes()
    stdout, stderr = job.communicate(timeout=30)
    assert job.returncode == 0, (stdout, stderr)
    many = json.loads((out / "manifest.json").read_bytes())
    assert set(inputs(many)) == {b"calibration.txt"}
    assert inputs(many)[b"calibration.txt"]["read_calls"] == 2500
    assert inputs(many)[b"calibration.txt"]["read_bytes"] == 2500
    assert inputs(many)[b"calibration.txt"]["sha256"] == paths[b"calibration.txt"]["sha256"]
    passed("2500 actual reads survive audit-ring eviction; another process's reads of the same snapshot are not attributed")
    delete("many")

    code = "import subprocess,sys; subprocess.run([sys.executable,'-c',\"open('parameters.json').read()\"],check=True); open('missing-file').read()"
    output, children, _ = run("children", "python3", "-c", code, success=False)
    assert b"parameters.json" in inputs(children)
    assert any(row["path_display"] == "missing-file" and row["errno"] == 2 for row in children["capture"]["errors"])
    passed("ordinary subprocess reads are captured and missing input requests are reported")
    delete("children")

    output, outside, _ = run("outside", "python3", "-c", f"open({str(m / 'calibration.txt')!r}).read()")
    assert outside["capture"]["unpinned_mount_accesses"] > 0
    h.cli("experiment", "verify", m, output, success=False)
    passed("reads from the mutable business tree are flagged rather than silently claimed as fixed inputs")
    delete("outside")

    external = h.base / "external-data"; external.write_text("outside")
    os.symlink(str(external), m / "outside-link")
    os.symlink("parameters.json", m / "inside-link")
    output, links, _ = run("links", "python3", "-c", "open('inside-link').read(); open('outside-link').read()")
    assert b"parameters.json" in inputs(links)
    assert links["capture"]["external_link_accesses"] > 0
    assert any(row.get("link_target_display") == str(external) and row["link_may_escape_input"] for row in links["capture"]["metadata_accesses"])
    h.cli("experiment", "verify", m, output, success=False)
    delete("links")
    passed("relative internal links retain target evidence; absolute external links are flagged and cannot be certified as fixed inputs")

    # Cancellation is finalised; abrupt runner loss is explicitly incomplete.
    for label, sig in (("cancelled", signal.SIGTERM), ("interrupted", signal.SIGKILL)):
        out = h.base / ("report-" + label)
        code = "import os,time; from pathlib import Path; open('calibration.txt').read(); Path(os.environ['TEAMFS_OUTPUT'],'ready').touch(); time.sleep(20)"
        job = subprocess.Popen([str(BINARY), "experiment", "run", str(m), label, "--output", str(out), "--", "python3", "-c", code], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for(lambda: (out / "outputs/ready").exists())
        job.send_signal(sig); job.communicate(timeout=10)
        if sig == signal.SIGKILL:
            wait_for(lambda: not Path(f"/proc/{json.loads((out/'manifest.json').read_bytes())['process_group']}").exists())
            h.cli("experiment", "recover", m, label, out)
            recovered = json.loads((out / "manifest.json").read_bytes())
            assert recovered["phase"] == "interrupted" and not recovered["capture_complete"]
            h.cli("experiment", "verify", m, out, success=False)
        else:
            cancelled = json.loads((out / "manifest.json").read_bytes())
            assert cancelled["cancelled"] and cancelled["signal"] == 9 and cancelled["capture_complete"]
        delete(label)
    passed("SIGTERM stops and records the job; runner SIGKILL preserves an incomplete report and allows explicit recovery of observations")

    out = h.base / "report-daemon-lost"
    code = "import os,time; from pathlib import Path; open('parameters.json').read(); Path(os.environ['TEAMFS_OUTPUT'],'ready').touch(); time.sleep(1)"
    job = subprocess.Popen([str(BINARY), "experiment", "run", str(m), "daemon-lost", "--output", str(out), "--", "python3", "-c", code], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    wait_for(lambda: (out / "outputs/ready").exists())
    # The command holds its cwd in the snapshot. Let it exit before normal unmount.
    h.process.kill(); h.process.wait(timeout=10); job.communicate(timeout=10); h.stop(kill=True)
    lost = json.loads((out / "manifest.json").read_bytes())
    assert job.returncode != 0 and lost["phase"] == "failed" and not lost["capture_complete"]
    h.start("--no-audit-file")
    assert (m / ".teamfs/snapshots/experiment.daemon-lost/parameters.json").exists()
    h.cli("experiment", "verify", m, out, success=False)
    delete("daemon-lost")
    passed("daemon crash preserves submitted inputs while explicitly rejecting lost runtime capture as incomplete")

    existing = h.base / "existing"; existing.mkdir(); (existing / "sentinel").write_text("keep")
    h.cli("experiment", "run", m, "bad-output", "--output", existing, "--", "true", success=False)
    assert (existing / "sentinel").read_text() == "keep"
    h.cli("experiment", "run", m, "../invalid", "--output", h.base / "invalid", "--", "true", success=False)
    h.cli("experiment", "run", m, "inside", "--output", m / "report", "--", "true", success=False)
    db = sqlite3.connect(h.store / "state.sqlite3")
    db.execute("CREATE TRIGGER fail_exp BEFORE INSERT ON nodes BEGIN SELECT RAISE(ABORT,'snapshot failure'); END"); db.commit()
    output, begin_failed, _ = run("begin-failed", "python3", "-c", "raise RuntimeError('must not launch')", success=False)
    assert begin_failed["phase"] == "failed" and not begin_failed["capture_complete"]
    assert not (m / ".teamfs/snapshots/experiment.begin-failed").exists()
    assert not any(not s["finished"] for s in h.status()["experiments"])
    db.execute("DROP TRIGGER fail_exp"); db.commit(); db.close()
    passed("invalid names/output targets are rejected; snapshot transaction failure never releases the command exec gate")

    evidence.update({"version": "0.8.0", "measured_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "checks": checks,
                     "baseline_result_sha256": hashlib.sha256(first_result).hexdigest(),
                     "replay_result_sha256": hashlib.sha256((replay / "outputs/result.json").read_bytes()).hexdigest(),
                     "scope": "Local deterministic file-input experiment only; not full environment or arbitrary ML reproducibility."})
    (ROOT / "artifacts/experiment-acceptance.json").write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n")
finally:
    h.close()
