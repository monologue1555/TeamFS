"""Compare preserved member-5 runs, rejecting incomplete or incompatible baselines."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import statistics


METRICS = (
    "input_copy_ms", "input_persist_ms", "command_wall_ms", "workload_body_ms",
    "verify_cli_ms", "snapshot_ms", "capture_hash_ms", "report_export_ms",
    "workload_self_maxrss_bytes", "daemon_rss_before_run_bytes", "daemon_rss_sampled_peak_bytes",
    "report_metadata_bytes", "workload_output_bytes",
) + tuple(stage + "/" + name for stage in ("storage_after_prepare", "storage_after_run", "storage_after_unmount")
          for name in ("state.sqlite3", "state.sqlite3-wal", "state.sqlite3-shm"))
REQUIRED = ("input_copy_ms", "input_persist_ms", "command_wall_ms", "workload_body_ms",
            "workload_self_maxrss_bytes", "report_metadata_bytes")
ENVIRONMENT = ("platform", "python", "cpu_model", "cpu_count", "linux_work_filesystem",
               "rustc", "cargo", "libfuse")
MEASUREMENT_SOURCES = ("tests/member5_workload.py", "tests/member5_benchmark.py",
                       "scripts/benchmark-member5.sh")


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def metric_value(row, metric):
    if "/" in metric:
        stage, name = metric.split("/", 1)
        return (row.get(stage) or {}).get(name)
    return row.get(metric)


def load_run(directory):
    data = read_json(directory / "summary.json")
    if data.get("schema_version") != 1 or data.get("status") != "pass":
        raise ValueError(f"{directory}: expected a completed schema-1 passing run")
    n = data["samples_per_group"]
    if type(n) is not int or n < 3:
        raise ValueError("At least 3 samples per group are required; smoke runs are not baselines")
    for key in ("workloads", "modes"):
        if not data[key] or len(set(data[key])) != len(data[key]):
            raise ValueError(f"Invalid/duplicate {key}")
    groups = {(workload, mode): [] for workload in data["workloads"] for mode in data["modes"]}
    fixtures = {name: read_json(directory / f"fixture-{name}.json") for name in data["workloads"]}
    for row in data["samples"]:
        key = row["workload"], row["mode"]
        if row.get("status") != "pass" or key not in groups or row.get("output_matches_oracle") is not True:
            raise ValueError("Unexpected, failed or unverified sample")
        fixture = fixtures[key[0]]
        if (row["file_count"], row["file_bytes"], row["rounds"]) != (fixture["count"], fixture["file_bytes"], fixture["rounds"]):
            raise ValueError("Sample dimensions do not match fixture metadata")
        required = REQUIRED + (("verify_cli_ms",) if row["mode"] == "experiment" else ())
        for metric in METRICS:
            value = metric_value(row, metric)
            if value is None:
                if metric in required:
                    raise ValueError(f"Missing required metric: {metric}")
            elif type(value) not in (int, float) or not math.isfinite(value) or value < 0:
                raise ValueError(f"Invalid metric: {metric}")
        if not isinstance(row.get("result_file_sha256"), str) or len(row["result_file_sha256"]) != 64:
            raise ValueError("Missing output file SHA256")
        groups[key].append(row)
    for key, rows in groups.items():
        if len(rows) != n or sorted(r["repetition"] for r in rows) != list(range(1, n + 1)):
            raise ValueError(f"Missing or duplicate repetitions in {key}")
        # A partially populated optional metric must not silently use fewer samples.
        for metric in METRICS:
            present = sum(metric_value(row, metric) is not None for row in rows)
            if present not in (0, n):
                raise ValueError(f"Partially populated metric in {key}: {metric}")
    for workload in data["workloads"]:
        hashes = {r["result_file_sha256"] for r in data["samples"] if r["workload"] == workload}
        if len(hashes) != 1:
            raise ValueError(f"Output hashes differ within {workload}")
    return data, groups, fixtures


def compare(before_dir, after_dir, allow_mismatch=False):
    before, left, before_fixtures = load_run(before_dir)
    after, right, after_fixtures = load_run(after_dir)
    if set(left) != set(right):
        raise ValueError("Workload/mode groups differ; compare complete matching groups")
    if before_fixtures != after_fixtures:
        raise ValueError("Input bytes, workload dimensions or expected outputs differ")
    for key in left:
        if left[key][0]["result_file_sha256"] != right[key][0]["result_file_sha256"]:
            raise ValueError(f"Output hashes differ between runs: {key}")
    mismatches = []
    for field in ("profile", "measurement_notes", "workloads", "modes"):
        if field not in before or field not in after or before[field] != after[field]:
            mismatches.append(field)
    for field in ENVIRONMENT:
        if before["environment"].get(field) is None or before["environment"].get(field) != after["environment"].get(field):
            mismatches.append("environment." + field)
    for path in MEASUREMENT_SOURCES:
        value = before["source"]["sha256"].get(path)
        if not value or value != after["source"]["sha256"].get(path):
            mismatches.append("measurement_source." + path)
    if mismatches and not allow_mismatch:
        raise ValueError("Incompatible measurement conditions: " + ", ".join(mismatches) +
                         ". Use --allow-mismatch only for an explicitly exploratory report.")
    rows = []
    for key in left:
        for metric in METRICS:
            values = [[metric_value(r, metric) for r in group if metric_value(r, metric) is not None]
                      for group in (left[key], right[key])]
            medians = [statistics.median(v) if v else None for v in values]
            old, new = medians
            comparable = old is not None and new is not None
            rows.append({"workload": key[0], "mode": key[1], "metric": metric,
                         "before_n": len(values[0]), "after_n": len(values[1]),
                         "before_median": old, "after_median": new,
                         "delta": new - old if comparable else None,
                         "change_percent": (new / old - 1) * 100 if comparable and old != 0 else None})
    return {
        "schema_version": 1,
        "assessment": "exploratory_conditions_differ" if mismatches else "matched_recorded_conditions",
        "condition_mismatches": mismatches,
        "before": {"directory": str(before_dir), "commit": before["source"]["commit"],
                   "binary_sha256": before["source"].get("binary_sha256"),
                   "summary_sha256": hashlib.sha256((before_dir / "summary.json").read_bytes()).hexdigest()},
        "after": {"directory": str(after_dir), "commit": after["source"]["commit"],
                  "binary_sha256": after["source"].get("binary_sha256"),
                  "summary_sha256": hashlib.sha256((after_dir / "summary.json").read_bytes()).hexdigest()},
        "rows": rows,
        "notes": ["Positive percent means the later median is larger; no automatic pass/fail performance threshold.",
                  "Missing stages stay null/N/A. Body is included in command wall; never add them.",
                  "Matching recorded conditions does not control power, background activity or cache state and is not proof of causation/statistical significance.",
                  "Comparing a run with itself is an integrity check only, never optimization evidence."],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("--output", type=Path, required=True, help="new output directory; existing paths refused")
    parser.add_argument("--allow-mismatch", action="store_true")
    args = parser.parse_args()
    try:
        report = compare(args.before, args.after, args.allow_mismatch)
    except (ValueError, KeyError, TypeError, OSError) as error:
        parser.exit(2, f"Comparison refused: {error}\n")
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / "comparison.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    lines = ["# Member 5 version comparison", "", "Assessment: " + report["assessment"], "",
             f"Before: `{report['before']['commit']}`; after: `{report['after']['commit']}`.", "",
             "Recorded-condition mismatches: " + (", ".join(report["condition_mismatches"]) or "none"), "",
             "| Workload | Mode | Metric | Before n | After n | Before median | After median | Change % |",
             "|---|---|---|---:|---:|---:|---:|---:|"]
    def number(value):
        return "N/A" if value is None else f"{value:.3f}"
    for row in report["rows"]:
        lines.append(f"| {row['workload']} | {row['mode']} | {row['metric']} | {row['before_n']} | {row['after_n']} | " +
                     " | ".join(number(row[key]) for key in ("before_median", "after_median", "change_percent")) + " |")
    lines += ["", *["- " + note for note in report["notes"]]]
    (args.output / "comparison.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(args.output / "comparison.md")


if __name__ == "__main__":
    main()
