"""Extract bounded daemon management timings from existing verified evidence."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import zipfile


def extract(sample, metrics):
    operations = {row["operation"]: row for row in metrics["operations"]}
    if len(operations) != len(metrics["operations"]):
        raise ValueError("duplicate management metric")
    def operation(name, calls=None, optional=False):
        value = operations.get("management." + name)
        if value is None:
            if optional:
                return None
            raise ValueError("missing management metric: " + name)
        for field in ("calls", "errors", "total_us", "commit_total_us"):
            if type(value.get(field)) is not int or value[field] < 0:
                raise ValueError("invalid management counter: " + name)
        if value["errors"] or value["calls"] < 1 or (calls is not None and value["calls"] != calls):
            raise ValueError("unexpected management calls/errors: " + name)
        if value["commit_total_us"] > value["total_us"]:
            raise ValueError("nested commit exceeds parent duration")
        return value
    start = operation("experiment_start", 1)
    finish = operation("experiment_finish", 1)
    sequence = sample.get("checkpoint_sequence")
    checkpoint = operation("experiment_checkpoint", sequence, optional=sequence is None)
    if sequence is None and checkpoint is not None:
        raise ValueError("checkpoint timings exist without report sequence")
    version_check = operation("experiment_input_version", 1, optional=True)
    return {
        "daemon_start_total_ms": start["total_us"] / 1000,
        "daemon_start_commit_ms": start["commit_total_us"] / 1000,
        "daemon_checkpoint_calls": None if checkpoint is None else checkpoint["calls"],
        "daemon_checkpoint_total_ms": None if checkpoint is None else checkpoint["total_us"] / 1000,
        "daemon_checkpoint_mean_ms": None if checkpoint is None else checkpoint["total_us"] / checkpoint["calls"] / 1000,
        "daemon_finish_total_ms": finish["total_us"] / 1000,
        "daemon_version_check_ms": None if version_check is None else version_check["total_us"] / 1000,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runs", nargs="+", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if len(set(path.resolve() for path in args.runs)) != len(args.runs):
        parser.error("duplicate run directories")
    rows, sources = [], []
    for directory in args.runs:
        summary = json.loads((directory / "summary.json").read_text())
        if summary["status"] != "pass":
            parser.error("incomplete/failed run")
        manifest = json.loads((directory / "evidence-archive.json").read_text())
        archive_path = directory / manifest["archive"]
        if hashlib.sha256(archive_path.read_bytes()).hexdigest() != manifest["archive_sha256"]:
            parser.error("archive hash mismatch")
        summary_hash = hashlib.sha256((directory / "summary.json").read_bytes()).hexdigest()
        if summary_hash != manifest["files_sha256"]["summary.json"]:
            parser.error("summary differs from archived run")
        sources.append({"run": directory.name, "summary_sha256": summary_hash,
                        "archive_sha256": manifest["archive_sha256"]})
        with zipfile.ZipFile(archive_path) as archive:
            for sample in summary["samples"]:
                if sample["status"] != "pass":
                    parser.error("failed sample in passing run")
                if sample["mode"] != "experiment":
                    continue
                name = sample["evidence"] + "/metrics-after.json"
                content = archive.read(name)
                digest = hashlib.sha256(content).hexdigest()
                if digest != manifest["files_sha256"][name]:
                    parser.error("metrics hash mismatch")
                row = {"run": directory.name, "target_commit": summary["source"]["commit"],
                       "workload": sample["workload"], "repetition": sample["repetition"],
                       "metrics_file": name, "metrics_sha256": digest,
                       **extract(sample, json.loads(content))}
                rows.append(row)
    notes = [
        "Timings are existing daemon management counters, measured inside Service::fsync while the service lock is already held.",
        "Start includes snapshot transaction; commit is nested inside start, never add them as separate stages.",
        "Checkpoint total includes initial and periodic summaries, content hashes and serialization; it excludes runner report fsync/rename and transport/kernel waiting.",
        "Finish includes capture/hash/serialization, not runner final report publication. Version check is part of the separate post-run verify command.",
        "Do not sum these with command wall or workload body. Checkpoints may run while the workload sleeps or waits for FUSE.",
        "Missing checkpoint support is null/N/A. No precise pure hashing, periodic-only save or external report write duration is inferred.",
    ]
    args.output.mkdir(parents=True, exist_ok=False)
    result = {"schema_version": 1, "sources": sources, "samples": rows, "scope_notes": notes}
    (args.output / "daemon-stages.json").write_text(json.dumps(result, indent=2) + "\n")
    lines = ["# 现有 daemon 指标中的子阶段耗时", "", "以下是原始 management 指标的提取，中位数单位 ms，所有样本均保留。",
             "", "不同运行批次分别统计，不跨环境合并。", "",
             "| 负载 | 运行批次 | 目标提交 | n | 启动处理 | 其中事务提交 | 摘要次数范围 | 摘要累计处理 | 每次摘要平均处理 | 最终采集处理 |",
             "|---|---|---|---:|---:|---:|---|---:|---:|---:|"]
    keys = list(dict.fromkeys((r["workload"], r["target_commit"], r["run"]) for r in rows))
    def formatted(group, field):
        values = [r[field] for r in group if r[field] is not None]
        return "N/A" if not values else f"{statistics.median(values):.3f}"
    for workload, commit, run in keys:
        group = [r for r in rows if r["workload"] == workload and r["target_commit"] == commit and r["run"] == run]
        counts = [r["daemon_checkpoint_calls"] for r in group if r["daemon_checkpoint_calls"] is not None]
        span = f"{min(counts)}–{max(counts)}" if counts else "N/A"
        lines.append(f"| {workload} | {run} | {commit[:7]} | {len(group)} | " +
                     " | ".join(formatted(group, key) for key in ("daemon_start_total_ms", "daemon_start_commit_ms")) +
                     f" | {span} | " + " | ".join(formatted(group, key) for key in
                     ("daemon_checkpoint_total_ms", "daemon_checkpoint_mean_ms", "daemon_finish_total_ms")) + " |")
    lines += ["", "边界：启动事务是启动处理的子集；摘要累计包含初始摘要与周期摘要，不包括运行器向外部报告目录写文件。最终采集处理包含输入哈希与序列化，但没有单独拆出哈希时间。",
              "", "这些数值不是互相独立的端到端阶段，不应再加到 command wall/body 上。测量位置见 src/service.rs 的 management 指标记录；已有指标不包含获取服务锁前的等待和内核请求传递。"]
    (args.output / "daemon-stages.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(args.output / "daemon-stages.md")


if __name__ == "__main__":
    main()
