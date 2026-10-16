"""Independent member-5 baseline. No changes to Rust or shared test harnesses."""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import traceback
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[1]
MODES = ("native", "teamfs", "experiment")
FIXTURES = {
    "small": (1, 4096, 32),
    "many": (256, 4096, 3),
    "large": (1, 16 * 1024 * 1024, 3),
}


def write_json(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def checked(command, **kwargs):
    return subprocess.run([str(x) for x in command], check=True, capture_output=True,
                          timeout=180, **kwargs)


def elapsed(action):
    start = time.perf_counter_ns()
    value = action()
    return (time.perf_counter_ns() - start) / 1e6, value


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def storage(path):
    return {name: (path / name).stat().st_size if (path / name).exists() else 0
            for name in ("state.sqlite3", "state.sqlite3-wal", "state.sqlite3-shm")}


def tree_bytes(path):
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file())


def rss(pid):
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1]) * 1024
    except FileNotFoundError:
        pass
    return None


class MemorySampler:
    def __init__(self, pid):
        self.pid = pid
        self.samples = []
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.poll, daemon=True)

    def poll(self):
        while not self.stop.is_set():
            value = rss(self.pid)
            if value is not None:
                self.samples.append(value)
            self.stop.wait(0.01)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.stop.set()
        self.thread.join()


class PrivateMount:
    def __init__(self, base, binary):
        self.mount = base / "input"
        self.store = base / "store"
        self.mount.mkdir()
        self.binary = binary
        self.log = (base / "daemon.log").open("wb")
        self.process = None

    def cli(self, *args):
        return checked([self.binary, *args])

    def start(self):
        self.process = subprocess.Popen([
            str(self.binary), "mount", str(self.mount), "--store", str(self.store),
            "--no-audit-file", "--capacity-mib", "128", "--max-file-mib", "32",
        ], stdout=self.log, stderr=self.log)
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError("daemon exited; see retained daemon.log")
            if os.path.ismount(self.mount):
                return
            time.sleep(0.02)
        raise TimeoutError("FUSE mount did not become ready")

    def close(self):
        try:
            if os.path.ismount(self.mount):
                checked([shutil.which("fusermount3") or "fusermount", "-u", self.mount])
            if self.process is not None:
                code = self.process.wait(timeout=15)
                if code:
                    raise RuntimeError(f"daemon exited with {code}")
        finally:
            self.log.close()


def make_fixture(directory, count, size):
    directory.mkdir()
    (directory / "data").mkdir()
    shutil.copyfile(ROOT / "tests/member5_workload.py", directory / "workload.py")
    (directory / "unused.bin").write_bytes(b"intentionally unread control\n")
    for index in range(count):
        # Synthetic deterministic bytes, not scientific data or entropy claims.
        block = hashlib.sha256(f"member5-fixture-{index}".encode()).digest() * 2048
        with (directory / "data" / f"{index:04d}.bin").open("wb") as stream:
            remaining = size
            while remaining:
                chunk = block[:min(remaining, len(block))]
                stream.write(chunk)
                remaining -= len(chunk)
    return {str(p.relative_to(directory)): sha(p)
            for p in sorted(directory.rglob("*")) if p.is_file()}


def persist_native(directory):
    # All copied files and both directories; no global sync or cache dropping.
    for p in sorted(directory.rglob("*")):
        if p.is_file():
            with p.open("rb") as stream:
                os.fsync(stream.fileno())
    for p in (directory / "data", directory):
        fd = os.open(p, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


def expected_output(fixture, count, rounds):
    digest = hashlib.sha256()
    total = 0
    for _ in range(rounds):
        for index in range(count):
            with (fixture / "data" / f"{index:04d}.bin").open("rb") as stream:
                while chunk := stream.read(65536):
                    digest.update(chunk)
                    total += len(chunk)
    return {"read_bytes": total, "sha256": digest.hexdigest()}


def run_sample(args, label, mode, repetition, fixture, hashes, oracle, destination, workroot):
    count, size, rounds = FIXTURES[label]
    evidence = destination / f"{label}-{repetition:02d}-{mode}"
    evidence.mkdir()
    base = Path(tempfile.mkdtemp(prefix="sample-", dir=workroot))
    mount = None
    row = {"workload": label, "mode": mode, "repetition": repetition,
           "file_count": count, "file_bytes": size, "rounds": rounds,
           "evidence": evidence.name, "status": "running"}
    try:
        if mode != "native":
            mount = PrivateMount(base, args.binary)
            row["mount_ms"], _ = elapsed(mount.start)
            root = mount.mount
        else:
            root = base / "input"
            root.mkdir()
            row["mount_ms"] = None
        row["input_copy_ms"], _ = elapsed(lambda: shutil.copytree(fixture, root, dirs_exist_ok=True))
        row["input_persist_ms"], _ = elapsed(
            (lambda: mount.cli("sync", root)) if mount else (lambda: persist_native(root)))
        row["storage_after_prepare"] = storage(mount.store) if mount else None
        report = base / "report"
        env = os.environ.copy()
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        command = [sys.executable, "-B", "workload.py", str(count), str(rounds)]
        if mode == "experiment":
            command = [str(args.binary), "experiment", "run", str(root), "bench",
                       "--output", str(report), "--", *command]
        else:
            (report / "outputs").mkdir(parents=True)
            env["TEAMFS_INPUT"] = str(root)
            env["TEAMFS_OUTPUT"] = str(report / "outputs")
        write_json(evidence / "command.json", {"argv": command, "cwd": str(root)})
        row["daemon_rss_before_run_bytes"] = rss(mount.process.pid) if mount else None
        if mount:
            with MemorySampler(mount.process.pid) as sampler:
                row["command_wall_ms"], completed = elapsed(lambda: checked(command, cwd=root, env=env))
            row["daemon_rss_sampled_peak_bytes"] = max(sampler.samples, default=None)
            row["daemon_rss_sample_count"] = len(sampler.samples)
        else:
            row["command_wall_ms"], completed = elapsed(lambda: checked(command, cwd=root, env=env))
            row["daemon_rss_sampled_peak_bytes"] = None
        (evidence / "command.stdout.txt").write_bytes(completed.stdout)
        (evidence / "command.stderr.txt").write_bytes(completed.stderr)
        measurement = json.loads((report / "outputs/workload-measurement.json").read_bytes())
        row["workload_body_ms"] = measurement["body_ms"]
        row["workload_self_maxrss_bytes"] = measurement["self_maxrss_bytes"]
        row["storage_after_run"] = storage(mount.store) if mount else None
        result = json.loads((report / "outputs/result.json").read_bytes())
        if result != oracle:
            raise AssertionError("workload output differs from independent fixture oracle")
        row["result_file_sha256"] = sha(report / "outputs/result.json")
        row["output_matches_oracle"] = True
        row["verify_cli_ms"] = None
        row["snapshot_ms"] = None
        row["capture_hash_ms"] = None
        row["report_export_ms"] = None
        if mode == "experiment":
            manifest = json.loads((report / "manifest.json").read_bytes())
            if not (manifest["capture_complete"] and manifest["phase"] == "finished"
                    and manifest["exit_code"] == 0 and not manifest.get("cancelled", False)):
                raise AssertionError("experiment did not finish successfully")
            capture = manifest["capture"]
            inputs = {bytes(x["path_bytes"]).decode(): x for x in capture["inputs"]}
            expected = {p: digest for p, digest in hashes.items() if p != "unused.bin"}
            if set(inputs) != set(expected):
                raise AssertionError(f"unexpected input list: {set(inputs) ^ set(expected)}")
            for name, digest in expected.items():
                if inputs[name]["sha256"] != digest or inputs[name]["access_class"] != "content_read":
                    raise AssertionError(f"input version/class mismatch: {name}")
                if name.startswith("data/") and inputs[name]["read_bytes"] != size * rounds:
                    raise AssertionError(f"capture byte count mismatch: {name}")
            if capture["unpinned_mount_accesses"] or capture["external_link_accesses"]:
                raise AssertionError("input capture escaped the pinned snapshot")
            row["captured_input_count"] = len(inputs)
            row["verify_cli_ms"], verification = elapsed(lambda: mount.cli("experiment", "verify", root, report))
            (evidence / "verify.stdout.txt").write_bytes(verification.stdout)
        row["report_metadata_bytes"] = sum(p.stat().st_size for p in report.iterdir() if p.is_file())
        row["workload_output_bytes"] = tree_bytes(report / "outputs")
        if mount:
            (evidence / "status-after.json").write_bytes((root / ".teamfs/status.json").read_bytes())
            (evidence / "metrics-after.json").write_bytes((root / ".teamfs/metrics.json").read_bytes())
        row["status"] = "pass"
    except Exception:
        row["status"] = "fail"
        row["error"] = traceback.format_exc()
        error = sys.exception()
        if isinstance(error, (subprocess.CalledProcessError, subprocess.TimeoutExpired)):
            (evidence / "failed-command.stdout.txt").write_bytes(error.stdout or b"")
            (evidence / "failed-command.stderr.txt").write_bytes(error.stderr or b"")
    finally:
        # Preserve evidence before disposing only this sample's private Linux directory.
        if (base / "report").exists():
            shutil.copytree(base / "report", evidence / "report", dirs_exist_ok=True)
        try:
            if mount:
                mount.close()
                row["storage_after_unmount"] = storage(mount.store)
            safe = not os.path.ismount(base / "input")
        except Exception:
            safe = False
            row["status"] = "fail"
            row["cleanup_error"] = traceback.format_exc()
        if (base / "daemon.log").exists():
            shutil.copyfile(base / "daemon.log", evidence / "daemon.log")
        if safe:
            assert base.parent == workroot and base.name.startswith("sample-")
            shutil.rmtree(base)
        else:
            row["retained_linux_directory"] = str(base)
        write_json(evidence / "sample.json", row)
    return row


def publish(destination, result):
    write_json(destination / "summary.json", result)
    fields = ["workload", "mode", "repetition", "status", "input_copy_ms", "input_persist_ms",
              "command_wall_ms", "workload_body_ms", "verify_cli_ms", "workload_self_maxrss_bytes",
              "daemon_rss_sampled_peak_bytes", "report_metadata_bytes", "result_file_sha256"]
    with (destination / "samples.csv").open("w", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=fields, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(result["samples"])
    lines = ["# Member 5 synthetic baseline", "", f"Status: {result['status']}. Profile: {result['profile']}.",
             f"Source commit: `{result['source']['commit']}`; local changes and source hashes are recorded in summary.json.", "",
             "Medians in milliseconds. Body is nested within command wall time; do not add them.",
             "Separate verify runs AFTER the experiment command and is not capture-time hashing.", "",
             "| Workload | Mode | n | Copy | Persist | Command wall | Read/hash body | Verify CLI |",
             "|---|---|---:|---:|---:|---:|---:|---:|---:|"]
    for label in result["workloads"]:
        for mode in result["modes"]:
            rows = [x for x in result["samples"] if x["status"] == "pass" and x["workload"] == label and x["mode"] == mode]
            if not rows:
                continue
            def median(key):
                values = [x[key] for x in rows if x.get(key) is not None]
                return f"{statistics.median(values):.3f}" if values else "N/A"
            lines.append(f"| {label} | {mode} | {len(rows)} | " + " | ".join(median(k) for k in
                         ("input_copy_ms", "input_persist_ms", "command_wall_ms", "workload_body_ms", "verify_cli_ms")) + " |")
    lines += ["", "Scope and limitations:", ""] + ["- " + note for note in result["measurement_notes"]]
    (destination / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--profile", choices=("debug", "release"), required=True)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--workloads", nargs="+", choices=tuple(FIXTURES), default=list(FIXTURES))
    parser.add_argument("--modes", nargs="+", choices=MODES, default=list(MODES))
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if not 1 <= args.samples <= 20:
        parser.error("samples must be 1..20; use at least 3 for a baseline, 1 only for smoke checks")
    args.binary = args.binary.resolve(strict=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    workroot = Path(tempfile.mkdtemp(prefix="teamfs-member5-", dir=Path.home()))
    filesystem = checked(["findmnt", "-n", "-o", "FSTYPE", "-T", workroot]).stdout.decode().strip()
    if filesystem in ("9p", "drvfs"):
        workroot.rmdir()
        parser.error("Linux HOME must reside on a Linux filesystem, not a Windows shared drive")
    destination = args.output or ROOT / "artifacts/member5" / (stamp + "-" + workroot.name.rsplit("-", 1)[-1])
    destination = destination.resolve()
    destination.mkdir(parents=True, exist_ok=False)
    source_paths = [ROOT / p for p in ("Cargo.toml", "Cargo.lock", "scripts/benchmark-member5.sh",
                                      "tests/member5_benchmark.py", "tests/member5_workload.py",
                                      "scripts/plot-member5.py")]
    source_paths += sorted((ROOT / "src").glob("*.rs"))
    for path in source_paths:
        target = destination / "source" / path.relative_to(ROOT)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
    result = {
        "schema_version": 1, "status": "running", "measured_at_utc": stamp,
        "profile": args.profile, "samples_per_group": args.samples,
        "workloads": args.workloads, "modes": args.modes, "samples": [],
        "source": {
            "commit": checked(["git", "rev-parse", "HEAD"], cwd=ROOT).stdout.decode().strip(),
            "status": checked(["git", "status", "--porcelain", "--", ".", ":(exclude)artifacts"], cwd=ROOT).stdout.decode(),
            "sha256": {str(p.relative_to(ROOT)): sha(p) for p in source_paths},
            "binary_sha256": sha(args.binary), "binary": str(args.binary),
        },
        "environment": {
            "platform": platform.platform(), "python": sys.version, "cpu_count": os.cpu_count(),
            "cpu_model": next((x.split(":", 1)[1].strip() for x in Path("/proc/cpuinfo").read_text().splitlines() if x.startswith("model name")), None),
            "meminfo": Path("/proc/meminfo").read_text(), "linux_work_filesystem": filesystem,
            "rustc": checked(["rustc", "--version"]).stdout.decode().strip(),
            "cargo": checked(["cargo", "--version"]).stdout.decode().strip(),
            "libfuse": checked(["pkg-config", "--modversion", "fuse"]).stdout.decode().strip(),
        },
        "measurement_notes": [
            "Synthetic sequential read plus SHA256 workloads, not a scientific case or universal filesystem ranking.",
            "Fresh mount/store per sample; rotating mode order. No OS cache drop. Prepared data and repeated reads include warm/uncontrolled caches.",
            "TeamFS uses default direct I/O, no file audit/trace/auto-sync; in-memory audit and runtime metrics remain enabled.",
            "Input copy and persistence are separate. Native persistence fsyncs copied files and directories; TeamFS uses sync CLI. Their durability mechanisms differ.",
            "Command wall includes subprocess startup. Experiment wall also includes snapshot commit, capture, complete-input hashing and report publication.",
            "Snapshot, capture hash and report export are NOT separately instrumented: null means unavailable, never zero. Verify CLI is a separate post-run recheck.",
            "Body timing covers read/hash loops only and is contained in command wall. Mount, evidence archiving and cleanup are excluded from those timings.",
            "Workload RSS is Linux ru_maxrss for the workload process only. Daemon peak is observed VmRSS sampled every 10 ms during command execution, may miss peaks; runner memory is not measured.",
            "Database/WAL/SHM values are logical file lengths, not physical device write traffic. Report bytes exclude workload outputs, which are recorded separately.",
            "Every successful output matches an independent deterministic oracle; experiment input hashes, classification, read bytes and unused-file exclusion are checked.",
            "No optimization claim or final integration acceptance: rerun after member 3/4 changes and with the final scientific case.",
        ],
    }
    publish(destination, result)
    print(f"Evidence: {destination}", flush=True)
    try:
        for label in args.workloads:
            count, size, rounds = FIXTURES[label]
            fixture = workroot / label
            hashes = make_fixture(fixture, count, size)
            oracle = expected_output(fixture, count, rounds)
            write_json(destination / f"fixture-{label}.json", {"file_sha256": hashes, "oracle": oracle,
                                                               "count": count, "file_bytes": size, "rounds": rounds})
            for repetition in range(1, args.samples + 1):
                offset = (repetition - 1) % len(args.modes)
                for mode in args.modes[offset:] + args.modes[:offset]:
                    row = run_sample(args, label, mode, repetition, fixture, hashes, oracle, destination, workroot)
                    result["samples"].append(row)
                    publish(destination, result)
                    print(f"{row['status'].upper()} {label} {mode} #{repetition}: {row.get('command_wall_ms', 0):.3f} ms", flush=True)
                    if row["status"] != "pass":
                        raise RuntimeError(row.get("error") or row.get("cleanup_error"))
            shutil.rmtree(fixture)
        result["status"] = "pass"
    except Exception:
        result["status"] = "fail"
        result["error"] = traceback.format_exc()
        print(result["error"], file=sys.stderr)
    finally:
        publish(destination, result)
        # Never recurse over this root if any mount or failed sample was retained.
        if not any(workroot.iterdir()):
            workroot.rmdir()
        else:
            print(f"Retained Linux workspace: {workroot}", flush=True)
    print(f"Result: {result['status']}; {destination / 'report.md'}", flush=True)
    return 0 if result["status"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
