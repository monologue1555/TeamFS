"""Measure the unchanged member-4 public Iris case in three modes on Linux."""
import argparse
from collections import defaultdict
import csv
from decimal import Decimal
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import traceback

import member5_benchmark as bench

ROOT = bench.ROOT
CASE = ROOT / "examples/research/iris"
INPUTS = {"analyze_iris.py", "iris.data", "analysis-config.json", "iris-baseline.json"}
DATA_HASH = "6f608b71a7317216319b4d27b4d9bc84e6abd734eda7872b71a458569e2656c0"


def oracle(case):
    """Independent decimal arithmetic, without importing the analyzed program."""
    if bench.sha(case / "iris.data") != DATA_HASH:
        raise ValueError("public Iris data differs from the documented source")
    config = json.loads((case / "analysis-config.json").read_bytes())
    grouped = defaultdict(list)
    for row in csv.reader((case / "iris.data").read_text().splitlines()):
        if row:
            grouped[row[4]].append([Decimal(v) for v in row[:4]])
    counts = {name: len(rows) for name, rows in grouped.items()}
    if counts != config["expected_classes"] or sum(counts.values()) != 150:
        raise ValueError("unexpected public dataset dimensions")
    def means(rows):
        return {feature: float(round(sum(row[i] for row in rows) / len(rows), 6))
                for i, feature in enumerate(config["features"])}
    return {"dataset": {"rows": 150, "features": config["features"], "class_counts": counts},
            "class_means": {name: means(rows) for name, rows in grouped.items()},
            "overall_means": means([row for rows in grouped.values() for row in rows]),
            "inputs": {kind: {"path": name, "sha256": bench.sha(case / name), "bytes": (case / name).stat().st_size}
                       for kind, name in (("data", "iris.data"), ("config", "analysis-config.json"), ("baseline", "iris-baseline.json"))}}


def validate_result(report, expected):
    result = json.loads((report / "outputs/result.json").read_bytes())
    if result.get("status") != "succeeded" or result.get("baseline_check", {}).get("matched") is not True:
        raise ValueError("Iris calculation or baseline check failed")
    for key, value in expected.items():
        if result.get(key) != value:
            raise ValueError("independent Iris oracle mismatch: " + key)
    return bench.sha(report / "outputs/result.json")


def validate_manifest(report, hashes):
    manifest = json.loads((report / "manifest.json").read_bytes())
    if not (manifest.get("capture_complete") is True and manifest.get("phase") == "finished"
            and manifest.get("exit_code") == 0 and manifest.get("outcome") == "succeeded"
            and not manifest.get("cancelled")):
        raise ValueError("experiment did not finish successfully")
    capture = manifest["capture"]
    rows = capture["inputs"]
    inputs = {bytes(row["path_bytes"]).decode(): row for row in rows}
    if len(inputs) != len(rows) or set(inputs) != INPUTS:
        raise ValueError("actual input set includes unused files or omits a dependency")
    for name, row in inputs.items():
        if row["sha256"] != hashes[name] or row["access_class"] != "content_read":
            raise ValueError("input content/version mismatch: " + name)
    if capture["unpinned_mount_accesses"] or capture["external_link_accesses"]:
        raise ValueError("input capture escaped snapshot")
    outputs = manifest["outputs"]
    if not (outputs["captured"] and outputs["complete"] and not outputs["errors"]):
        raise ValueError("incomplete output capture")
    entries = {bytes(row["path_bytes"]).decode(): row for row in outputs["files"]}
    if len(entries) != len(outputs["files"]) or set(entries) != {"outputs/result.json", "stdout.txt", "stderr.txt"}:
        raise ValueError("unexpected output set")
    for name, row in entries.items():
        if row["sha256"] != bench.sha(report / name) or row["bytes"] != (report / name).stat().st_size:
            raise ValueError("output digest/size mismatch: " + name)
    if outputs["count"] != len(entries) or outputs["total_bytes"] != sum(row["bytes"] for row in entries.values()):
        raise ValueError("output totals disagree")
    if not manifest.get("environment", {}).get("python3"):
        raise ValueError("missing runtime environment evidence")
    return manifest


def persist(directory):
    for path in directory.iterdir():
        with path.open("rb") as stream:
            os.fsync(stream.fileno())
    fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def sample(args, mode, repetition, destination, workroot, expected, hashes):
    evidence = destination / f"iris-{repetition:02d}-{mode}"
    evidence.mkdir()
    base = Path(tempfile.mkdtemp(prefix="sample-", dir=workroot))
    report, replay = base / "report", base / "replay"
    mount = None
    row = {"workload": "uci_iris", "mode": mode, "repetition": repetition, "status": "running",
           "evidence": evidence.name, "workload_body_ms": None, "workload_self_maxrss_bytes": None}
    try:
        if mode == "native":
            root = base / "input"
            root.mkdir()
        else:
            mount = bench.PrivateMount(base, args.binary)
            row["mount_ms"], _ = bench.elapsed(mount.start)
            root = mount.mount
        row["input_copy_ms"], _ = bench.elapsed(lambda: shutil.copytree(CASE, root, dirs_exist_ok=True))
        row["input_persist_ms"], _ = bench.elapsed(lambda: mount.cli("sync", root) if mount else persist(root))
        row["storage_after_prepare"] = bench.storage(mount.store) if mount else None
        command = [sys.executable, "-I", "analyze_iris.py", "iris.data", "analysis-config.json", "iris-baseline.json"]
        env = {**os.environ, "TEAMFS_INPUT": str(root), "TEAMFS_OUTPUT": str(report / "outputs")}
        if mode == "experiment":
            command = [args.binary, "experiment", "run", root, "iris-bench", "--output", report, "--", *command]
        else:
            (report / "outputs").mkdir(parents=True)
        bench.write_json(evidence / "command.json", {"argv": list(map(str, command)), "cwd": str(root)})
        row["daemon_rss_before_run_bytes"] = bench.rss(mount.process.pid) if mount else None
        if mount:
            with bench.MemorySampler(mount.process.pid) as sampler:
                row["command_wall_ms"], completed = bench.elapsed(lambda: bench.checked(command, cwd=root, env=env))
            row["daemon_rss_sampled_peak_bytes"] = max(sampler.samples, default=None)
        else:
            row["command_wall_ms"], completed = bench.elapsed(lambda: bench.checked(command, cwd=root, env=env))
            row["daemon_rss_sampled_peak_bytes"] = None
        (evidence / "command.stdout.txt").write_bytes(completed.stdout)
        (evidence / "command.stderr.txt").write_bytes(completed.stderr)
        if mode != "experiment":
            (report / "stdout.txt").write_bytes(completed.stdout)
            (report / "stderr.txt").write_bytes(completed.stderr)
        row["result_file_sha256"] = validate_result(report, expected)
        row["output_matches_oracle"] = True
        row["report_metadata_bytes"] = sum(p.stat().st_size for p in report.iterdir() if p.is_file())
        row["workload_output_bytes"] = bench.tree_bytes(report / "outputs")
        row["storage_after_run"] = bench.storage(mount.store) if mount else None
        if mount:
            (evidence / "metrics-after.json").write_bytes((root / ".teamfs/metrics.json").read_bytes())
        if mode == "experiment":
            manifest = validate_manifest(report, hashes)
            row.update(checkpoint_sequence=manifest["checkpoint_sequence"], output_digest_count=manifest["outputs"]["count"],
                       output_digests_verified=True, captured_input_count=len(manifest["capture"]["inputs"]))
            row["verify_cli_ms"], verified = bench.elapsed(lambda: mount.cli("experiment", "verify", root, report))
            (evidence / "verify.stdout.txt").write_bytes(verified.stdout)
            # Change current live data before replay; original pinned input must still be used.
            original = (root / "iris.data").read_bytes()
            changed = original.replace(b"5.1,3.5,1.4,0.2,Iris-setosa", b"5.2,3.5,1.4,0.2,Iris-setosa", 1)
            if changed == original:
                raise ValueError("expected live-data mutation did not apply")
            (root / "iris.data").write_bytes(changed)
            mount.cli("sync", root)
            row["live_data_changed_sha256"] = bench.sha(root / "iris.data")
            row["replay_cli_ms"], replayed = bench.elapsed(lambda: mount.cli("experiment", "replay", root, report, "iris-replay", "--output", replay))
            (evidence / "replay.stdout.txt").write_bytes(replayed.stdout)
            validate_manifest(replay, hashes)
            if validate_result(replay, expected) != row["result_file_sha256"]:
                raise ValueError("replay result bytes differ")
            row["replay_verify_cli_ms"], verified = bench.elapsed(lambda: mount.cli("experiment", "verify", root, replay))
            (evidence / "replay-verify.stdout.txt").write_bytes(verified.stdout)
            row["compare_cli_ms"], compared = bench.elapsed(lambda: mount.cli("experiment", "compare", report, replay, "--json"))
            comparison = json.loads(compared.stdout)
            (evidence / "compare.json").write_bytes(compared.stdout)
            if not all(comparison["verdict"][key] is True for key in ("both_capture_complete", "inputs_content_identical", "outputs_identical")):
                raise ValueError("comparison did not confirm replay equality")
            row["replay_byte_identical"] = True
            row["compare_confirmed"] = True
        row["status"] = "pass"
    except Exception:
        row["status"], row["error"] = "fail", traceback.format_exc()
        error = sys.exception()
        if isinstance(error, (subprocess.CalledProcessError, subprocess.TimeoutExpired)):
            (evidence / "failed.stdout.txt").write_bytes(error.stdout or b"")
            (evidence / "failed.stderr.txt").write_bytes(error.stderr or b"")
    finally:
        for path in (report, replay):
            if path.exists():
                shutil.copytree(path, evidence / path.name)
        try:
            if mount:
                mount.close()
            safe = not os.path.ismount(base / "input")
        except Exception:
            safe = False
            row["status"], row["cleanup_error"] = "fail", traceback.format_exc()
        if (base / "daemon.log").exists():
            shutil.copyfile(base / "daemon.log", evidence / "daemon.log")
        if safe:
            assert base.parent == workroot and base.name.startswith("sample-")
            shutil.rmtree(base)
        else:
            row["retained_linux_directory"] = str(base)
        bench.write_json(evidence / "sample.json", row)
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--target-ref", required=True)
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not 3 <= args.samples <= 20:
        parser.error("formal case requires 3..20 samples per mode")
    args.binary = args.binary.resolve(strict=True)
    target = bench.checked(["git", "rev-parse", args.target_ref + "^{commit}"], cwd=ROOT).stdout.decode().strip()
    if bench.checked(["git", "diff", target, "--", "src", "Cargo.toml", "Cargo.lock"], cwd=ROOT).stdout:
        parser.error("current core source differs from target ref")
    expected = oracle(CASE)
    workroot = Path(tempfile.mkdtemp(prefix="teamfs-member5-iris-", dir=Path.home()))
    filesystem = bench.checked(["findmnt", "-n", "-o", "FSTYPE", "-T", workroot]).stdout.decode().strip()
    if filesystem in ("9p", "drvfs"):
        workroot.rmdir()
        parser.error("use a Linux HOME filesystem")
    destination = args.output.resolve()
    destination.mkdir(parents=True, exist_ok=False)
    paths = [ROOT / name for name in ("Cargo.toml", "Cargo.lock", "tests/member5_iris.py", "tests/member5_benchmark.py")]
    paths += sorted((ROOT / "src").glob("*.rs")) + sorted(p for p in CASE.iterdir() if p.is_file())
    for path in paths:
        saved = destination / "source" / path.relative_to(ROOT)
        saved.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, saved)
    hashes = {p.name: bench.sha(p) for p in CASE.iterdir() if p.is_file()}
    bench.write_json(destination / "fixture-iris.json", {"file_sha256": hashes, "oracle": expected})
    result = {"schema_version": 1, "kind": "member5_public_iris", "status": "running", "profile": "release",
              "measured_at_utc": datetime.now(timezone.utc).isoformat(),
              "samples_per_group": args.samples, "samples": [], "modes": list(bench.MODES),
              "source": {"commit": target, "checkout_commit": bench.checked(["git", "rev-parse", "HEAD"], cwd=ROOT).stdout.decode().strip(),
                         "sha256": {p.relative_to(ROOT).as_posix(): bench.sha(p) for p in paths}, "binary_sha256": bench.sha(args.binary),
                         "status": bench.checked(["git", "status", "--porcelain", "--", ".", ":(exclude)artifacts"], cwd=ROOT).stdout.decode()},
              "environment": {"platform": platform.platform(), "python": sys.version, "cpu_count": os.cpu_count(),
                              "cpu_model": next(x.split(":", 1)[1].strip() for x in Path('/proc/cpuinfo').read_text().splitlines() if x.startswith('model name')),
                              "linux_work_filesystem": filesystem, "rustc": bench.checked(["rustc", "--version"]).stdout.decode().strip(),
                              "cargo": bench.checked(["cargo", "--version"]).stdout.decode().strip(),
                              "libfuse": bench.checked(["pkg-config", "--modversion", "fuse"]).stdout.decode().strip()},
              "notes": ["Unchanged public UCI Iris analysis; 150 rows, 4 features, 3 classes. DOI 10.24432/C56C76; CC BY 4.0.",
                        "Independent Decimal means oracle; byte-identical results required across all modes and replays.",
                        "Fresh stores/mounts, rotating modes, no cache drop. Command includes process startup; experiment includes snapshot, input/output digests, environment capture and report publication.",
                        "Replay follows a live-data mutation; verify/replay/compare are separate commands after the timed original run, not additive stages.",
                        "No body or workload RSS instrumentation was added to the member-4 program; these remain null. Report bytes are final lengths, not device write traffic.",
                        "Small public case, not large-data throughput or final project acceptance. Source and raw reports are retained."]}
    try:
        for repetition in range(1, args.samples + 1):
            offset = (repetition - 1) % 3
            for mode in bench.MODES[offset:] + bench.MODES[:offset]:
                row = sample(args, mode, repetition, destination, workroot, expected, hashes)
                result["samples"].append(row)
                bench.write_json(destination / "summary.json", result)
                print(f"{row['status']} {mode} #{repetition}: {row.get('command_wall_ms', 0):.3f} ms", flush=True)
                if row["status"] != "pass":
                    raise RuntimeError(row.get("error") or row.get("cleanup_error"))
        if len({row["result_file_sha256"] for row in result["samples"]}) != 1:
            raise ValueError("results differ between modes")
        result["status"] = "pass"
    except Exception:
        result["status"], result["error"] = "fail", traceback.format_exc()
        print(result["error"], file=sys.stderr)
    finally:
        bench.write_json(destination / "summary.json", result)
        if not any(workroot.iterdir()):
            workroot.rmdir()
    return 0 if result["status"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
