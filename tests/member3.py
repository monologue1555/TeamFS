"""Member 3: real FUSE checkpoint durability, interruption, transactions and migration."""
import hashlib
import json
import os
import platform
import shutil
import signal
import sqlite3
import subprocess
import time

from fs_harness import BINARY, ROOT, Mount, wait_for
from storage_helpers import stored_bytes

h = Mount("member3")
m = h.mount
checks = []
jobs = []
evidence = {}


def passed(message):
    checks.append(message)
    print(f"PASS {len(checks):02}: {message}", flush=True)


def manifest(out):
    return json.loads((out / "manifest.json").read_bytes())


def inputs(report):
    return {bytes(row["path_bytes"]): row for row in report["capture"]["inputs"]}


def launch(name, code, output_name=None):
    out = h.base / ("report-" + (output_name or name))
    job = subprocess.Popen([str(BINARY), "experiment", "run", str(m), name,
                            "--output", str(out), "--", "python3", "-c", code],
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    jobs.append(job)
    return job, out


def run(name, code, success=True):
    job, out = launch(name, code)
    stdout, stderr = job.communicate(timeout=20)
    assert (job.returncode == 0) == success, (stdout, stderr)
    return out, manifest(out)


def checkpoint_with(out, *paths):
    if not (out / "manifest.json").exists():
        return False
    report = manifest(out)
    return report.get("checkpoint_sequence", 0) >= 2 and set(paths) <= set(inputs(report))


WAIT = "import os,time; from pathlib import Path; out=Path(os.environ['TEAMFS_OUTPUT']); "

try:
    h.start("--no-audit-file")
    original = bytes(range(256)) * 769  # Crosses several 64 KiB block boundaries.
    expected = hashlib.sha256(original).hexdigest()
    (m / "input.bin").write_bytes(original)
    (m / "late.bin").write_bytes(b"later evidence")
    code = WAIT + "open('input.bin','rb').read(); (out/'ready').touch();\nwhile not (out/'next').exists(): time.sleep(.02)\nopen('late.bin','rb').read(); (out/'late-ready').touch();\nwhile True: time.sleep(.1)"
    job, out = launch("durable", code)
    wait_for(lambda: (out / "outputs/ready").exists())
    wait_for(lambda: checkpoint_with(out, b"input.bin"))
    first = manifest(out)
    assert first["phase"] == "running" and not first["capture_complete"]
    assert inputs(first)[b"input.bin"]["sha256"] == expected
    (out / "outputs/next").touch()
    wait_for(lambda: checkpoint_with(out, b"input.bin", b"late.bin"))
    saved = manifest(out)
    assert saved["checkpoint_sequence"] > first["checkpoint_sequence"]
    assert not next(s for s in h.status()["experiments"] if s["name"] == "durable")["finished"]
    passed("running summaries are durably published repeatedly without sealing the capture session")

    # Mutating/truncating/deleting the live file cannot change the pinned input.
    (m / "input.bin").write_bytes(b"changed")
    os.truncate(m / "input.bin", 0)
    (m / "input.bin").unlink()
    pinned = m / ".teamfs/snapshots/experiment.durable/input.bin"
    assert pinned.read_bytes() == original
    try:
        pinned.write_bytes(b"must fail")
    except OSError:
        pass
    else:
        raise AssertionError("experiment input accepted a write")
    h.cli("snapshot", "delete", m, "experiment.durable", success=False)
    h.cli("experiment", "delete", m, "durable", success=False)
    passed("submitted input remains byte-exact and read-only after live overwrite, truncate and deletion")

    # Kill both processes before a final report can be exported.
    h.process.kill()
    job.kill()
    job.communicate(timeout=10)
    h.stop(kill=True)
    saved = manifest(out)
    assert saved["capture_source"] == "checkpoint" and not saved["capture_complete"]
    saved_capture = saved["capture"]
    h.start("--no-audit-file")
    h.cli("experiment", "recover", m, "durable", out)
    recovered = manifest(out)
    assert recovered["outcome"] == "daemon_interrupted" and recovered["phase"] == "interrupted"
    assert recovered["capture"] == saved_capture and recovered["unknown_after_checkpoint"]
    assert not recovered["capture_complete"] and recovered["input_version_available"]
    assert "exit_code" not in recovered and "signal" not in recovered
    assert hashlib.sha256((m / ".teamfs/snapshots/experiment.durable/input.bin").read_bytes()).hexdigest() == expected
    h.cli("experiment", "verify", m, out, success=False)
    h.cli("experiment", "replay", m, out, "invalid-replay", "--output", h.base / "invalid-replay", success=False)
    corrupt_report = h.base / "corrupt-report"
    shutil.copytree(out, corrupt_report)
    corrupt_manifest = manifest(corrupt_report)
    corrupt_manifest["capture"]["inputs"] = None
    (corrupt_report / "manifest.json").write_text(json.dumps(corrupt_manifest))
    before = (corrupt_report / "manifest.json").read_bytes()
    h.cli("experiment", "recover", m, "durable", corrupt_report, success=False)
    assert (corrupt_report / "manifest.json").read_bytes() == before
    h.cli("experiment", "restore", m, "durable", "restored")
    assert (m / "restored/input.bin").read_bytes() == original
    evidence["last_checkpoint"] = saved
    evidence["recovered_report"] = recovered
    passed("daemon and runner SIGKILL retain the last on-disk summary and pinned input; recovery preserves unknown gaps and exit status")

    # A crash of the runner alone can still recover the live daemon's observations.
    job, live_out = launch("runner-lost", WAIT + "open('late.bin','rb').read(); (out/'ready').touch(); time.sleep(60)")
    wait_for(lambda: checkpoint_with(live_out, b"late.bin"))
    pgid = manifest(live_out)["process_group"]
    job.kill(); job.communicate(timeout=10)
    wait_for(lambda: not os.path.exists(f"/proc/{pgid}"))
    h.cli("experiment", "recover", m, "runner-lost", live_out)
    live = manifest(live_out)
    assert live["outcome"] == "runner_interrupted" and live["capture_source"] == "live_recovery"
    assert not live["capture_complete"] and "exit_code" not in live
    passed("runner-only interruption recovers live observations without inventing command termination results")

    success_out, success = run("success", "open('late.bin','rb').read()")
    assert success["outcome"] == "succeeded" and success["exit_code"] == 0 and success["capture_complete"]
    _, failed = run("program-failed", "open('late.bin','rb').read(); raise SystemExit(7)", False)
    assert failed["outcome"] == "program_failed" and failed["exit_code"] == 7 and failed["capture_complete"]
    job, cancel_out = launch("cancelled", WAIT + "open('late.bin','rb').read(); (out/'ready').touch(); time.sleep(60)")
    wait_for(lambda: (cancel_out / "outputs/ready").exists())
    job.send_signal(signal.SIGTERM); job.communicate(timeout=10)
    cancelled = manifest(cancel_out)
    assert cancelled["outcome"] == "cancelled" and cancelled["signal"] == 9
    assert cancelled["exit_code"] is None and cancelled["capture_complete"]
    # Deliver cancellation while SQLite blocks the initial input commit. The
    # user's command must never be released afterwards, even briefly.
    marker = h.base / "cancel-before-start-must-not-run"
    with sqlite3.connect(h.store / "state.sqlite3") as db:
        db.execute("BEGIN IMMEDIATE")
        job, early_out = launch("early-cancel", f"from pathlib import Path; Path({str(marker)!r}).touch()")
        def handler_ready():
            with open(f"/proc/{job.pid}/status") as status:
                caught = next(line.split()[1] for line in status if line.startswith("SigCgt:"))
            return bool(int(caught, 16) & (1 << (signal.SIGTERM - 1)))
        wait_for(handler_ready, seconds=1)
        job.send_signal(signal.SIGTERM)
        db.commit()
    job.communicate(timeout=10)
    assert not marker.exists()
    assert manifest(early_out)["outcome"] == "cancelled"
    passed("success, command failure and cancellation retain distinct observed outcomes; cancellation during input commit never releases the command")

    # Even if the runner survives a daemon loss, its failure report retains a checkpoint.
    job, loss_out = launch("daemon-only", WAIT + "open('late.bin','rb').read(); (out/'ready').touch(); time.sleep(60)")
    wait_for(lambda: checkpoint_with(loss_out, b"late.bin"))
    prior = manifest(loss_out)
    h.process.kill(); h.process.wait(timeout=10)
    job.communicate(timeout=15)
    assert job.returncode != 0
    loss = manifest(loss_out)
    assert loss["outcome"] == "daemon_unavailable" and not loss["capture_complete"]
    assert loss["capture"] == prior["capture"] and loss["unknown_after_checkpoint"]
    h.stop(kill=True); h.start("--no-audit-file")
    h.cli("experiment", "recover", m, "daemon-only", loss_out)
    passed("daemon-only crash makes the runner fail explicitly while preserving its last persisted evidence")

    # Isolated injection checks commit rollback and the exec gate for each table.
    # Remove finished experiment versions to stay within the pre-existing snapshot quota.
    for name in ("runner-lost", "program-failed", "cancelled", "early-cancel", "daemon-only"):
        h.cli("experiment", "delete", m, name)
    (m / "sentinel").write_bytes(b"old commit")
    h.sync()
    for table in ("blobs", "nodes", "metadata"):
        (m / "unsaved").write_bytes((table + " new unique payload").encode())
        with sqlite3.connect(h.store / "state.sqlite3") as db:
            action = "UPDATE" if table == "metadata" else "INSERT"
            db.execute(f"CREATE TRIGGER fail_member3 BEFORE {action} ON {table} BEGIN SELECT RAISE(ABORT,'member3 failure'); END")
        marker = h.base / (table + "-should-not-exist")
        _, bad = run("fail-" + table, f"from pathlib import Path; Path({str(marker)!r}).touch()", False)
        assert bad["outcome"] == "start_failed" and not marker.exists()
        assert not (m / (".teamfs/snapshots/experiment.fail-" + table)).exists()
        with sqlite3.connect(h.store / "state.sqlite3") as db:
            assert stored_bytes(db, "sentinel") == b"old commit"
            db.execute("DROP TRIGGER fail_member3")
        _, retry = run("retry-" + table, "open('sentinel').read()")
        assert retry["outcome"] == "succeeded"
        h.cli("experiment", "delete", m, "retry-" + table)
    passed("blobs/nodes/metadata transaction failures preserve the old commit, never release the exec gate, and permit explicit retry")

    # External reports are copied separately; backup imports retain the pinned tree.
    backup = h.base / "backup"
    imported = h.base / "imported"
    h.cli("backup", "create", m, backup)
    h.cli("backup", "verify", backup)
    h.cli("backup", "import", backup, "--store", imported)
    h.cli("backup", "import", backup, "--store", imported, success=False)
    corrupt = h.base / "corrupt-backup"
    shutil.copytree(backup, corrupt)
    with (corrupt / "state.sqlite3").open("ab") as stream:
        stream.write(b"corruption")
    refused = h.base / "refused-import"
    h.cli("backup", "import", corrupt, "--store", refused, success=False)
    assert not refused.exists()
    copied_report = h.base / "copied-report"
    shutil.copytree(out, copied_report)
    copied_success = h.base / "copied-success"
    shutil.copytree(success_out, copied_success)
    h.stop()
    h.store = imported
    h.start("--no-audit-file")
    assert hashlib.sha256((m / ".teamfs/snapshots/experiment.durable/input.bin").read_bytes()).hexdigest() == expected
    assert manifest(copied_report)["capture"] == saved_capture
    h.cli("experiment", "recover", m, "durable", copied_report)
    assert manifest(copied_report)["input_version_status"] == "matched"
    assert manifest(copied_report)["input_version_available"] is True
    h.cli("experiment", "verify", m, copied_success)
    h.cli("experiment", "verify", m, copied_report, success=False)
    passed("report-plus-backup migration preserves checkpoint input SHA256; corrupt backups and existing targets are refused")

    # A snapshot's reusable name must never substitute for its original identity.
    job, old_out = launch("reuse-version", WAIT + "open('late.bin','rb').read(); time.sleep(60)")
    wait_for(lambda: checkpoint_with(old_out, b"late.bin"))
    old_capture = manifest(old_out)["capture"]
    pgid = manifest(old_out)["process_group"]
    job.kill(); job.communicate(timeout=10)
    wait_for(lambda: not os.path.exists(f"/proc/{pgid}"))
    h.stop(kill=True); h.start("--no-audit-file")
    h.cli("experiment", "delete", m, "reuse-version")
    (m / "late.bin").write_bytes(b"replacement version")
    replacement, replacement_out = launch("reuse-version", WAIT + "open('late.bin','rb').read(); time.sleep(60)", "replacement")
    wait_for(lambda: checkpoint_with(replacement_out, b"late.bin"))
    h.cli("experiment", "recover", m, "reuse-version", old_out)
    old = manifest(old_out)
    assert old["capture"] == old_capture
    assert old["input_version_available"] is False and old["input_version_status"] == "different_version"
    assert replacement.poll() is None
    assert not next(s for s in h.status()["experiments"] if s["name"] == "reuse-version")["finished"]
    replacement.send_signal(signal.SIGTERM); replacement.communicate(timeout=10)
    # Also refuse a completed report if only an unread file in its tree changed.
    h.cli("experiment", "delete", m, "success")
    (m / "late.bin").write_bytes(b"later evidence")
    (m / "unread-new-file").write_bytes(b"different input tree")
    replacement_success, replacement_success_out = launch("success", "pass", "replacement-success")
    replacement_success.communicate(timeout=10)
    assert replacement_success.returncode == 0
    h.cli("experiment", "verify", m, copied_success, success=False)
    passed("reused names cannot impersonate original input versions or seal another session; verification also detects unread tree changes")

    h.stop()
    h.start("--memory", "--no-audit-file")
    _, memory = run("memory", "open('welcome.txt').read()")
    assert memory["mode"] == "memory" and not memory["input_version_persistent"]
    assert not memory["capture"]["input_version_persistent"]
    h.stop(); h.start("--memory", "--no-audit-file")
    assert not (m / ".teamfs/snapshots/experiment.memory").exists()
    passed("memory-mode reports explicitly declare non-persistent input and remount does not pretend to retain it")

    # A bind-mounted checkout may belong to the host uid. Trust only this
    # explicit test checkout for these read-only provenance queries.
    git = ["git", "-c", f"safe.directory={ROOT}"]
    evidence.update({"checks": checks, "environment": platform.platform(),
                     "commit": subprocess.check_output([*git, "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
                     "working_tree_modified": bool(subprocess.check_output([*git, "status", "--porcelain"], cwd=ROOT)),
                     "measured_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
                     "input_sha256": expected,
                     "scope": "Member 3 real mounted tests; on-disk periodic summaries do not reconstruct unobserved gaps."})
    source_files = sorted((ROOT / "src").glob("*.rs")) + [
        ROOT / "Cargo.toml", ROOT / "Cargo.lock", ROOT / "tests/member3.py",
        ROOT / "scripts/test-member3.sh", ROOT / "scripts/check.sh",
    ]
    evidence["source_files_sha256"] = {
        path.relative_to(ROOT).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in source_files
    }
    (ROOT / "artifacts/member3-acceptance.json").write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n")
finally:
    for job in jobs:
        if job.poll() is None:
            job.kill()
        job.communicate(timeout=10)
    h.close()
