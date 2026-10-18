"""Member 4 end-to-end case: public Iris data, same-name change, report comparison and replay."""
import hashlib
import json
import platform
import shutil
import time

from fs_harness import Mount, ROOT

CASE = ROOT / "examples/research/iris"
CASE_FILES = (
    "analyze_iris.py",
    "iris.data",
    "analysis-config.json",
    "iris-baseline.json",
    "bezdekIris.data",
    "iris.names",
    "Index",
    "iris.json",
    "README.md",
)
ARTIFACTS = ROOT / "artifacts"
COMMAND = ("python3", "-I", "analyze_iris.py", "iris.data", "analysis-config.json", "iris-baseline.json")


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def manifest(out):
    return json.loads((out / "manifest.json").read_bytes())


def input_map(report):
    return {row["path_display"]: row["sha256"] for row in report["capture"]["inputs"]}


def output_map(report):
    return {
        row["path_display"]: row["sha256"] for row in report["outputs"]["files"]
    }


def result(out):
    return json.loads((out / "outputs/result.json").read_bytes())


def compare(left, right):
    return json.loads(
        h.cli("experiment", "compare", left, right, "--json").stdout.decode("utf-8")
    )


h = Mount("member4-case")
stamp = time.strftime("%Y%m%d-%H%M%S") + "-" + h.base.name
destination = ARTIFACTS / "member4-case" / stamp
destination.mkdir(parents=True)
ARTIFACTS.mkdir(exist_ok=True)

summary = {
    "measured_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    "kernel": platform.release(),
    "python": platform.python_version(),
    "case": "UCI Iris（公开真实数据，非合成）",
    "data_source": {
        "repository": "UCI Machine Learning Repository, dataset 53",
        "url": "https://archive.ics.uci.edu/dataset/53/iris",
        "doi": "10.24432/C56C76",
        "license": "CC BY 4.0",
        "retrieved_at": "2026-10-18",
        "data_sha256": sha256(CASE / "iris.data"),
        "rows": 150,
        "features": 4,
        "classes": 3,
    },
    "checks": {},
    "scope": "同机同环境下的确定性重放；环境摘要不是完整环境归档，公开数据案例不证明任意科研程序都可完全复现。",
}

try:
    h.start("--no-audit-file")
    for name in CASE_FILES:
        (h.mount / name).write_bytes((CASE / name).read_bytes())

    print("1. 用当前公开数据执行分析程序。", flush=True)
    baseline = h.base / "baseline"
    h.cli("experiment", "run", h.mount, "iris-baseline", "--output", baseline, "--", *COMMAND)
    baseline_report = manifest(baseline)
    baseline_result = result(baseline)
    # Export the original live inputs before the same-name edit, so the retained
    # backup really corresponds to the baseline report.
    backup_source = h.base / "export-backup"
    h.cli("backup", "create", h.mount, backup_source)
    h.cli("backup", "verify", backup_source)

    print("2. 修改同名 iris.data 后重新运行。", flush=True)
    original_data = (CASE / "iris.data").read_bytes()
    marker = b"5.1,3.5,1.4,0.2,Iris-setosa"
    assert marker in original_data
    (h.mount / "iris.data").write_bytes(
        original_data.replace(marker, b"5.2,3.5,1.4,0.2,Iris-setosa", 1)
    )
    changed = h.base / "changed"
    h.cli("experiment", "run", h.mount, "iris-data-changed", "--output", changed, "--", *COMMAND)
    changed_report = manifest(changed)
    changed_result = result(changed)

    print("3. 按第一次报告重放旧输入。", flush=True)
    replay = h.base / "replay"
    h.cli("experiment", "replay", h.mount, baseline, "iris-replay", "--output", replay)
    replay_report = manifest(replay)
    replay_result = result(replay)

    print("4. 保留报告、独立输入备份和比较结果。", flush=True)
    for label, source in (("baseline", baseline), ("changed", changed), ("replay", replay)):
        shutil.copytree(source, destination / label)
    shutil.copytree(backup_source, destination / "backup")
    h.cli("backup", "verify", destination / "backup")

    kept_baseline = destination / "baseline"
    kept_changed = destination / "changed"
    kept_replay = destination / "replay"
    compared_changed = compare(kept_baseline, kept_changed)
    compared_replay = compare(kept_baseline, kept_replay)
    (destination / "compare-changed.json").write_text(
        json.dumps(compared_changed, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    (destination / "compare-replay.json").write_text(
        json.dumps(compared_replay, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    (ARTIFACTS / "member4-compare-changed.json").write_text(
        json.dumps(compared_changed, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    (ARTIFACTS / "member4-compare-replay.json").write_text(
        json.dumps(compared_replay, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )

    h.cli("experiment", "verify", h.mount, kept_baseline)
    h.cli("experiment", "verify", h.mount, kept_replay)

    baseline_bytes = (kept_baseline / "outputs/result.json").read_bytes()
    replay_bytes = (kept_replay / "outputs/result.json").read_bytes()
    changed_bytes = (kept_changed / "outputs/result.json").read_bytes()
    baseline_sha = hashlib.sha256(baseline_bytes).hexdigest()
    replay_sha = hashlib.sha256(replay_bytes).hexdigest()
    changed_sha = hashlib.sha256(changed_bytes).hexdigest()

    expected_inputs = {
        "analyze_iris.py",
        "iris.data",
        "analysis-config.json",
        "iris-baseline.json",
    }
    baseline_inputs = input_map(baseline_report)
    assert set(baseline_inputs) == expected_inputs, set(baseline_inputs)
    assert "bezdekIris.data" not in baseline_inputs
    assert baseline_inputs["iris.data"] == sha256(CASE / "iris.data")
    assert set(input_map(changed_report)) == expected_inputs
    assert set(input_map(replay_report)) == expected_inputs
    assert baseline_bytes == replay_bytes
    assert baseline_sha == replay_sha
    assert changed_sha != baseline_sha
    assert changed_result["class_means"]["Iris-setosa"]["sepal_length_cm"] != (
        baseline_result["class_means"]["Iris-setosa"]["sepal_length_cm"]
    )
    assert baseline_result["baseline_check"]["matched"] is True
    assert replay_result["baseline_check"]["matched"] is True
    assert changed_result["baseline_check"]["matched"] is False

    changed_inputs = [
        row["path_display"] for row in compared_changed["inputs"]["content_changed"]
    ]
    changed_outputs = [
        row["path_display"] for row in compared_changed["outputs"]["changed"]
    ]
    assert changed_inputs == ["iris.data"]
    assert "outputs/result.json" in changed_outputs
    assert compared_changed["verdict"]["inputs_content_identical"] is False
    assert compared_changed["verdict"]["outputs_identical"] is False

    assert compared_replay["inputs"]["summary"]["content_changed"] == 0
    assert compared_replay["outputs"]["summary"]["changed"] == 0
    assert compared_replay["verdict"]["inputs_content_identical"] is True
    assert compared_replay["verdict"]["outputs_identical"] is True

    summary["checks"] = {
        "unused_file_bezdekExcluded": True,
        "exact_inputs": sorted(expected_inputs),
        "output_digest_files": sorted(output_map(baseline_report)),
        "baseline_result_sha256": baseline_sha,
        "changed_result_sha256": changed_sha,
        "replay_result_sha256": replay_sha,
        "baseline_replay_byte_identical": True,
        "changed_result_differs": True,
        "changed_inputs": changed_inputs,
        "changed_outputs": changed_outputs,
        "replay_comparison_has_no_content_change": True,
        "verify_baseline_and_replay": True,
    }
    summary["reports"] = {
        "baseline": {
            "experiment": baseline_report["experiment"],
            "snapshot": baseline_report["snapshot"],
            "inputs": baseline_inputs,
            "outputs": output_map(baseline_report),
            "result": baseline_result,
            "environment": baseline_report["environment"],
        },
        "changed": {
            "experiment": changed_report["experiment"],
            "snapshot": changed_report["snapshot"],
            "inputs": input_map(changed_report),
            "outputs": output_map(changed_report),
            "result": changed_result,
            "environment": changed_report["environment"],
        },
        "replay": {
            "experiment": replay_report["experiment"],
            "snapshot": replay_report["snapshot"],
            "inputs": input_map(replay_report),
            "outputs": output_map(replay_report),
            "result": replay_result,
            "environment": replay_report["environment"],
        },
    }
    summary["comparisons"] = {
        "changed": {
            "artifact": str(ARTIFACTS / "member4-compare-changed.json"),
            "inputs": compared_changed["inputs"]["summary"],
            "outputs": compared_changed["outputs"]["summary"],
            "verdict": compared_changed["verdict"],
        },
        "replay": {
            "artifact": str(ARTIFACTS / "member4-compare-replay.json"),
            "inputs": compared_replay["inputs"]["summary"],
            "outputs": compared_replay["outputs"]["summary"],
            "verdict": compared_replay["verdict"],
        },
    }

    (destination / "summary.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    (ARTIFACTS / "member4-case-latest.json").write_text(
        json.dumps(
            {"directory": str(destination), "summary": summary},
            ensure_ascii=False,
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    print(json.dumps(summary["checks"], ensure_ascii=False, indent=2), flush=True)
    print("报告、比较结果与独立输入备份已保留：", destination, flush=True)
finally:
    h.close()
