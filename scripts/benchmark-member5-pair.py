"""Build two immutable refs and run identical current measurement tools, sequentially."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
TOOLS = ("scripts/benchmark-member5.sh", "scripts/plot-member5.py", "scripts/compare-member5.py",
         "scripts/package-member5.py", "tests/member5_benchmark.py", "tests/member5_workload.py")
WORKLOADS = ("small", "many", "large", "paced_many", "paced_large", "unused_large")


def run(command, cwd=ROOT, env=None, log=None):
    print("RUN " + " ".join(map(str, command)), flush=True)
    if log:
        with log.open("wb") as stream:
            subprocess.run(list(map(str, command)), cwd=cwd, env=env, stdout=stream,
                           stderr=subprocess.STDOUT, check=True, timeout=1200)
    else:
        subprocess.run(list(map(str, command)), cwd=cwd, env=env, check=True, timeout=1200)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", required=True, help="old Git ref, e.g. 276e68e")
    parser.add_argument("--after", default="HEAD", help="new Git ref")
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--workloads", nargs="+", choices=WORKLOADS, default=list(WORKLOADS))
    parser.add_argument("--order", choices=("before-first", "after-first"), default="before-first")
    parser.add_argument("--before-checkpoints", choices=("auto", "required", "absent"), default="auto")
    parser.add_argument("--after-checkpoints", choices=("auto", "required", "absent"), default="required")
    parser.add_argument("--name", help="new directory name under artifacts/member5-studies")
    args = parser.parse_args()
    if not 3 <= args.samples <= 20:
        parser.error("formal pairs need 3..20 samples per group")
    if len(set(args.workloads)) != len(args.workloads):
        parser.error("duplicate workloads")
    name = args.name or datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    if not name or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-" for c in name):
        parser.error("name must contain only ASCII letters, digits, underscores or hyphens")
    refs = {key: subprocess.check_output(["git", "rev-parse", value + "^{commit}"], cwd=ROOT,
                                         text=True).strip() for key, value in (("before", args.before), ("after", args.after))}
    if refs["before"] == refs["after"]:
        parser.error("pair must use different target refs")
    study = ROOT / "artifacts/member5-studies" / name
    study.mkdir(parents=True, exist_ok=False)
    source_root = Path(tempfile.mkdtemp(prefix="teamfs-member5-pair-", dir=Path.home()))
    target = Path.home() / ".cache/teamfs-member5/target"
    env = {**os.environ, "CARGO_TARGET_DIR": str(target)}
    order = ["before", "after"] if args.order == "before-first" else ["after", "before"]
    state = {"schema_version": 1, "status": "running", "refs": refs, "order": order,
             "source_checkout_root": str(source_root), "samples_per_group": args.samples,
             "workloads": args.workloads, "runs": {},
             "expected_checkpoint_capability": {"before": args.before_checkpoints, "after": args.after_checkpoints},
             "method": "Both builds and unit suites finish before measurement; same current tools copied to each checkout; target versions run sequentially, not interleaved; modes rotate per repetition.",
             "scope": "Whole-version effect, not checkpoint on/off isolation; no core Rust modifications."}
    def save():
        (study / "study.json").write_text(json.dumps(state, indent=2) + "\n")
    save()
    try:
        # Keep both source checkouts and binaries available for audit. No worktree links
        # across Windows/Linux and no branch changes in the user's active checkout.
        for label, ref in refs.items():
            checkout = source_root / label
            run(["git", "clone", "--no-hardlinks", "--no-checkout", ROOT, checkout], log=study / f"{label}-clone.txt")
            run(["git", "checkout", "--detach", ref], cwd=checkout, log=study / f"{label}-checkout.txt")
            for relative in TOOLS:
                destination = checkout / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / relative, destination)
            core_diff = subprocess.check_output(["git", "status", "--porcelain", "--", "src", "Cargo.toml", "Cargo.lock"], cwd=checkout)
            if core_diff:
                raise RuntimeError("target core differs from recorded ref")
            run(["cargo", "build", "--locked", "--release"], cwd=checkout, env=env, log=study / f"{label}-build.txt")
            binary = source_root / (label + "-teamfs")
            shutil.copy2(target / "release/teamfs", binary)
            run(["cargo", "test", "--locked", "--release"], cwd=checkout, env=env, log=study / f"{label}-unit-tests.txt")
            output = ROOT / "artifacts/member5" / (name + "-" + label)
            state["runs"][label] = {"directory": output.relative_to(ROOT).as_posix(),
                                    "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
            save()
        for label in order:
            checkout = source_root / label
            output = ROOT / state["runs"][label]["directory"]
            run([sys.executable, checkout / "tests/member5_benchmark.py", "--binary", source_root / (label + "-teamfs"),
                 "--profile", "release", "--samples", args.samples, "--workloads", *args.workloads,
                 "--checkpoints", state["expected_checkpoint_capability"][label],
                 "--output", output], cwd=checkout, env=env)
        run([sys.executable, ROOT / "scripts/compare-member5.py",
             ROOT / state["runs"]["before"]["directory"], ROOT / state["runs"]["after"]["directory"],
             "--output", study / "comparison"])
        for label in order:
            output = ROOT / state["runs"][label]["directory"]
            run([sys.executable, ROOT / "scripts/package-member5.py", output])
        state["status"] = "pass"
    except Exception as error:
        state["status"] = "fail"
        state["error"] = str(error)
        raise
    finally:
        save()
        print(f"Study: {study}; source checkouts retained: {source_root}", flush=True)


if __name__ == "__main__":
    main()
