"""Member 4: public-data case, output/environment evidence, comparison and replay."""
import hashlib
import json
import shutil
from pathlib import Path

from fs_harness import Mount, ROOT
from reporting import Report

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

report = Report("member4-acceptance")
report.context = {
    "case": str(CASE),
    "data": "UCI Iris dataset 53 (public, CC BY 4.0)",
    "scope": "Real FUSE mount; one deterministic standard-library analysis; environment summary is not an environment archive.",
}
h = Mount("member4")
m = h.mount
state = {}


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def manifest(out):
    return json.loads((Path(out) / "manifest.json").read_bytes())


def inputs(report_value):
    return {row["path_display"]: row for row in report_value["capture"]["inputs"]}


def outputs(report_value):
    return {row["path_display"]: row for row in report_value["outputs"]["files"]}


def compare(left, right):
    value = json.loads(
        h.cli("experiment", "compare", left, right, "--json").stdout.decode("utf-8")
    )
    return value


def run(name, data=None, config=None, success=True):
    if data is not None:
        (m / "iris.data").write_bytes(data)
    if config is not None:
        (m / "analysis-config.json").write_text(config, encoding="utf-8")
    out = h.base / ("report-" + name)
    result = h.cli(
        "experiment",
        "run",
        m,
        name,
        "--output",
        out,
        "--",
        "python3",
        "-I",
        "analyze_iris.py",
        "iris.data",
        "analysis-config.json",
        "iris-baseline.json",
        success=success,
    )
    return out, manifest(out), result


def load_result(out):
    return json.loads((Path(out) / "outputs/result.json").read_bytes())


def baseline_case():
    out, value, _ = run("iris-baseline")
    state["baseline_out"] = out
    state["baseline"] = value
    names = set(inputs(value))
    assert names == {
        "analyze_iris.py",
        "iris.data",
        "analysis-config.json",
        "iris-baseline.json",
    }, names
    assert "bezdekIris.data" not in names
    assert "iris.names" not in names and "Index" not in names
    assert inputs(value)["iris.data"]["sha256"] == sha256(CASE / "iris.data")
    assert inputs(value)["iris.data"]["access_class"] == "content_read"
    assert inputs(value)["analyze_iris.py"]["access_class"] == "content_read"

    digest = outputs(value)
    assert set(digest) == {"outputs/result.json", "stdout.txt", "stderr.txt"}, set(digest)
    assert value["outputs"]["captured"] is True
    assert value["outputs"]["complete"] is True
    assert value["outputs"]["count"] == 3
    assert digest["outputs/result.json"]["sha256"] == sha256(out / "outputs/result.json")
    assert digest["outputs/result.json"]["bytes"] == (out / "outputs/result.json").stat().st_size

    environment = value["environment"]
    assert "不是完整环境归档" in environment["disclaimer"]
    assert environment["os_release"] and environment["kernel"] and environment["machine"]
    assert environment["python3"] and environment["python3"].startswith("Python 3")
    assert environment["cpu_count"] and environment["cpu_count"] >= 1
    assert environment["mem_total_kib"] and environment["teamfs_version"] == h.status()["version"]

    result = load_result(out)
    assert result["baseline_check"]["matched"] is True
    assert result["class_means"]["Iris-setosa"]["sepal_length_cm"] == 5.006
    assert result["class_means"]["Iris-virginica"]["petal_width_cm"] == 2.026

    verified = h.cli("experiment", "verify", m, out).stdout.decode("utf-8")
    assert "输出验证通过" in verified
    return "4 exact inputs; unused Bezdek/names/Index excluded; 3 output digests; environment summary present; verify re-hashes outputs"


def metadata_case():
    baseline = state["baseline_out"]
    copy = h.base / "report-metadata-only"
    shutil.copytree(baseline, copy)
    value = manifest(copy)
    value["experiment"] = "iris-metadata-only"
    value["snapshot"] = "experiment.iris-metadata-only"
    value["started_unix"] += 1000
    value["finished_unix"] += 1000
    value["process_group"] += 1
    value["checkpoint_sequence"] = value.get("checkpoint_sequence", 0) + 3
    value["daemon_session"] = "different-session"
    value["capture"]["daemon_session"] = "different-session"
    for row in value["capture"]["inputs"]:
        row["read_calls"] += 17
        row["read_bytes"] += 31
        row["pids"] = [999999]
    (copy / "manifest.json").write_text(
        json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )

    comparison = compare(baseline, copy)
    assert comparison["inputs"]["summary"] == {
        "added": 0,
        "removed": 0,
        "content_changed": 0,
        "access_class_changed": 0,
        "unchanged": 4,
    }, comparison["inputs"]["summary"]
    assert comparison["outputs"]["summary"]["changed"] == 0
    assert comparison["outputs"]["summary"]["added"] == 0
    assert comparison["outputs"]["summary"]["removed"] == 0
    assert comparison["verdict"]["inputs_content_identical"] is True
    assert comparison["verdict"]["outputs_identical"] is True
    assert comparison["run_context"]["experiment"]["changed"] is True
    assert comparison["run_context"]["started_unix"]["changed"] is True
    assert comparison["run_context"]["finished_unix"]["changed"] is True
    assert comparison["run_context"]["process_group"]["changed"] is True
    assert comparison["run_context"]["command"]["changed"] is False
    ignored = set(comparison["volatile_fields_ignored"])
    assert {"experiment", "process_group", "started_unix", "exit_code"} <= ignored
    assert "capture.inputs[].read_calls" in ignored
    return "renamed experiment, new PID/timestamps/read counters produce zero content changes and are listed as run metadata"


def changed_case():
    baseline = state["baseline_out"]
    original_data = (CASE / "iris.data").read_bytes()
    original_config = (CASE / "analysis-config.json").read_bytes()
    state["original_data"] = original_data
    state["original_config"] = original_config

    first = b"5.1,3.5,1.4,0.2,Iris-setosa"
    assert first in original_data
    modified_data = original_data.replace(first, b"5.2,3.5,1.4,0.2,Iris-setosa", 1)
    changed, changed_manifest, _ = run("iris-data-changed", data=modified_data)
    state["changed_out"] = changed
    changed_result = load_result(changed)
    assert changed_result["inputs"]["data"]["sha256"] != sha256(CASE / "iris.data")
    assert changed_result["class_means"]["Iris-setosa"]["sepal_length_cm"] != 5.006
    assert changed_result["baseline_check"]["matched"] is False
    assert changed_result["baseline_check"]["mismatch_count"] >= 1

    comparison = compare(baseline, changed)
    assert comparison["inputs"]["summary"]["content_changed"] == 1
    assert [row["path_display"] for row in comparison["inputs"]["content_changed"]] == [
        "iris.data"
    ]
    unchanged = {row["path_display"] for row in comparison["inputs"]["unchanged"]}
    assert unchanged == {"analyze_iris.py", "analysis-config.json", "iris-baseline.json"}
    changed_outputs = {row["path_display"] for row in comparison["outputs"]["changed"]}
    assert "outputs/result.json" in changed_outputs, changed_outputs
    assert "stdout.txt" in changed_outputs
    assert comparison["verdict"]["inputs_content_identical"] is False
    assert comparison["verdict"]["outputs_identical"] is False
    assert outputs(changed_manifest)["outputs/result.json"]["sha256"] != (
        outputs(state["baseline"])["outputs/result.json"]["sha256"]
    )

    config = json.loads(original_config)
    config["rounding_decimals"] = 2
    config_changed, config_manifest, _ = run(
        "iris-config-changed",
        data=original_data,
        config=json.dumps(config, ensure_ascii=False, indent=2) + "\n",
    )
    state["config_changed_out"] = config_changed
    config_result = load_result(config_changed)
    assert config_result["rounding_decimals"] == 2
    assert config_result["class_means"]["Iris-setosa"]["sepal_length_cm"] == 5.01
    assert config_result["baseline_check"]["matched"] is True

    comparison = compare(baseline, config_changed)
    assert [
        row["path_display"] for row in comparison["inputs"]["content_changed"]
    ] == ["analysis-config.json"]
    assert "outputs/result.json" in {
        row["path_display"] for row in comparison["outputs"]["changed"]
    }
    assert (
        outputs(config_manifest)["outputs/result.json"]["sha256"]
        != outputs(state["baseline"])["outputs/result.json"]["sha256"]
    )

    (m / "analysis-config.json").write_bytes(original_config)
    return "same-name iris.data edit changes a reported mean; same-name config edit changes rounding and output; comparison names both changed inputs"


def failure_case():
    baseline = state["baseline_out"]
    original_data = state["original_data"]
    invalid = b"oops,3.5,1.4,0.2,Iris-setosa\n" + original_data.split(b"\n", 1)[1]
    failed, failed_manifest, process = run("iris-failed", data=invalid, success=False)
    assert process.returncode == 2
    assert failed_manifest["outcome"] == "program_failed"
    assert failed_manifest["exit_code"] == 2
    assert failed_manifest["capture_complete"] is True
    assert failed_manifest["outputs"]["captured"] is True
    assert failed_manifest["outputs"]["complete"] is True
    assert failed_manifest["environment"]["kernel"]
    failure = json.loads((failed / "outputs/failure.json").read_bytes())
    assert failure["status"] == "failed" and "非数值" in failure["error"]
    assert "outputs/failure.json" in outputs(failed_manifest)

    h.cli("experiment", "verify", m, failed)

    comparison = compare(baseline, failed)
    assert comparison["inputs"]["summary"]["content_changed"] == 1
    assert [
        row["path_display"] for row in comparison["inputs"]["content_changed"]
    ] == ["iris.data"]
    assert comparison["outputs"]["summary"]["added"] == 1
    assert comparison["outputs"]["summary"]["removed"] == 1
    assert [row["path_display"] for row in comparison["outputs"]["added"]] == [
        "outputs/failure.json"
    ]
    assert [row["path_display"] for row in comparison["outputs"]["removed"]] == [
        "outputs/result.json"
    ]
    changed = {row["path_display"] for row in comparison["outputs"]["changed"]}
    assert {"stdout.txt", "stderr.txt"} <= changed

    (m / "iris.data").write_bytes(original_data)
    return "program exit 2 still records failure.json, exit status and environment; comparison reports added failure.json and removed result.json"


def replay_case():
    baseline = state["baseline_out"]
    replay_out = h.base / "report-iris-replay"
    h.cli("experiment", "replay", m, baseline, "iris-replay", "--output", replay_out)
    replay_manifest = manifest(replay_out)
    state["replay_out"] = replay_out

    original = (baseline / "outputs/result.json").read_bytes()
    reproduced = (replay_out / "outputs/result.json").read_bytes()
    assert original == reproduced
    assert sha256(baseline / "outputs/result.json") == sha256(
        replay_out / "outputs/result.json"
    )
    assert (baseline / "stdout.txt").read_bytes() == (replay_out / "stdout.txt").read_bytes()
    assert (baseline / "stderr.txt").read_bytes() == (replay_out / "stderr.txt").read_bytes()
    assert set(inputs(replay_manifest)) == set(inputs(state["baseline"]))

    comparison = compare(baseline, replay_out)
    assert comparison["inputs"]["summary"] == {
        "added": 0,
        "removed": 0,
        "content_changed": 0,
        "access_class_changed": 0,
        "unchanged": 4,
    }
    assert comparison["outputs"]["summary"]["changed"] == 0
    assert comparison["outputs"]["summary"]["added"] == 0
    assert comparison["outputs"]["summary"]["removed"] == 0
    assert comparison["verdict"]["inputs_content_identical"] is True
    assert comparison["verdict"]["outputs_identical"] is True
    assert comparison["run_context"]["experiment"]["changed"] is True
    h.cli("experiment", "verify", m, replay_out)
    return "replay from the baseline snapshot reproduces result/stdout/stderr byte-for-byte and SHA256; comparison shows no content change"


def tamper_case():
    baseline = state["baseline_out"]
    tampered = h.base / "report-tampered"
    shutil.copytree(baseline, tampered)
    (tampered / "outputs/result.json").write_text("{}\n", encoding="utf-8")
    result = h.cli("experiment", "verify", m, tampered, success=False)
    assert "输出校验不一致" in result.stderr.decode("utf-8")

    unsafe = h.base / "report-unsafe-output"
    shutil.copytree(baseline, unsafe)
    value = manifest(unsafe)
    target = next(
        row for row in value["outputs"]["files"] if row["path_display"] == "outputs/result.json"
    )
    target["path_bytes"] = list(b"../escape")
    (unsafe / "manifest.json").write_text(
        json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    result = h.cli("experiment", "verify", m, unsafe, success=False)
    message = result.stderr.decode("utf-8")
    assert "输出路径" in message and (".." in message or "相对路径" in message), message
    return "verify rejects a changed output digest and an output path escaping the report directory"


try:
    h.start("--no-audit-file")
    for name in CASE_FILES:
        (m / name).write_bytes((CASE / name).read_bytes())
    report.case(
        "public Iris case records exact inputs, excluded unused files, output digests and a lightweight environment summary",
        baseline_case,
    )
    report.case(
        "comparison ignores experiment name, PID, timestamps and read counters",
        metadata_case,
    )
    report.case(
        "same-name data and config changes alter results and appear as content changes",
        changed_case,
    )
    report.case(
        "failed program keeps verifiable failure evidence and compares output add/remove",
        failure_case,
    )
    report.case(
        "old-input replay is byte-identical and compares without content change",
        replay_case,
    )
    report.case(
        "verify rejects tampered output content and unsafe report paths",
        tamper_case,
    )
finally:
    h.close()

report.finish()
