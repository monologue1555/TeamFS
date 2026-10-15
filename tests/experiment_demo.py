"""A real, isolated research-input demonstration with independently retained backup."""
import hashlib
import json
import platform
import shutil
import time
from fs_harness import Mount, ROOT

h = Mount("experiment-demo")
destination = ROOT / "artifacts" / "experiment-demo" / (time.strftime("%Y%m%d-%H%M%S") + f"-{h.base.name}")
destination.mkdir(parents=True)

def execute(name):
    output = h.base / name
    result = h.cli("experiment", "run", h.mount, name, "--output", output, "--", "python3", "analyze.py")
    print(result.stdout.decode(), end="", flush=True)
    return output

try:
    h.start()
    for file in (ROOT / "examples/research").iterdir():
        if file.is_file():
            (h.mount / file.name).write_bytes(file.read_bytes())
    print("1. 用当前输入执行真实分析程序。", flush=True)
    baseline = execute("baseline")
    report = json.loads((baseline / "manifest.json").read_bytes())
    print("实际正文输入：", [row["path_display"] for row in report["capture"]["inputs"]], flush=True)
    print("2. 修改同名数据，再运行一次。", flush=True)
    (h.mount / "measurements.csv").write_text("sample,value\na,50\nb,60\nc,70\n")
    changed = execute("changed")
    print("3. 按第一次报告重放：使用旧代码和旧数据。", flush=True)
    replay = h.base / "replay"
    print(h.cli("experiment", "replay", h.mount, baseline, "replay", "--output", replay).stdout.decode(), end="", flush=True)
    original = (baseline / "outputs/result.json").read_bytes()
    reproduced = (replay / "outputs/result.json").read_bytes()
    assert original == reproduced
    summary = {"measured_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "kernel": platform.release(), "version": h.status()["version"],
               "baseline": json.loads(original), "changed": json.loads((changed / "outputs/result.json").read_bytes()),
               "replayed": json.loads(reproduced), "identical_result_bytes": True,
               "baseline_sha256": hashlib.sha256(original).hexdigest(), "replay_sha256": hashlib.sha256(reproduced).hexdigest(),
               "scope": "Deterministic small Python data-analysis example. Standard Python libraries and environment are not archived by the experiment capture."}
    for label, source in (("baseline", baseline), ("changed", changed), ("replay", replay)):
        shutil.copytree(source, destination / label)
    # Publish backup on Linux: the Windows shared filesystem does not support
    # the directory fsync/renameat2 guarantees required by atomic backup create.
    linux_backup = h.base / "export-backup"
    h.cli("backup", "create", h.mount, linux_backup)
    h.cli("backup", "verify", linux_backup)
    shutil.copytree(linux_backup, destination / "backup")
    h.cli("backup", "verify", destination / "backup")
    (destination / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
    (ROOT / "artifacts/experiment-demo-latest.json").write_text(json.dumps({"directory": str(destination), "summary": summary}, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(summary, ensure_ascii=False, indent=2), flush=True)
    print("报告、结果与独立输入备份已保留：", destination, flush=True)
finally:
    h.close()
