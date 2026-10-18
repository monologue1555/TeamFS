"""Generate a study report and plots from completed before/after evidence."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import statistics

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("member5_compare", ROOT / "scripts/compare-member5.py")
comparison = importlib.util.module_from_spec(spec)
spec.loader.exec_module(comparison)


def stratified_report(studies, destination):
    """Keep batches separate, including any recorded cross-batch differences."""
    inputs, baseline, refs, workloads = [], None, None, None
    lines = ["# 目标版本对照汇总（按批次分别展示）", "",
             "不同批次不合并计算中位数，不作为统一基线。样本数与环境信息逐批列出。", ""]
    count, hashes, seen = 0, {}, set()
    for index, path in enumerate(studies, 1):
        state = json.loads((path / "study.json").read_text())
        directories = {label: ROOT / state["runs"][label]["directory"] for label in ("before", "after")}
        if state["status"] != "pass" or (refs is not None and (state["refs"] != refs or state["workloads"] != workloads)):
            raise ValueError("studies must pass and use the same refs and workloads")
        if any(p.resolve() in seen for p in directories.values()):
            raise ValueError("duplicate run")
        seen.update(p.resolve() for p in directories.values())
        refs, workloads = state["refs"], state["workloads"]
        result = comparison.compare(directories["before"], directories["after"])
        mismatches = [] if baseline is None else comparison.compare(baseline, directories["before"], allow_mismatch=True)["condition_mismatches"]
        baseline = baseline or directories["before"]
        relative = Path(os.path.relpath(path, destination)).as_posix()
        lines += [f"## 批次 {index}：{' → '.join(state['order'])}", "",
                  f"每组 {state['samples_per_group']} 次；与首批记录条件差异：{', '.join(mismatches) or '无'}。", ""]
        for label, directory in directories.items():
            data = comparison.read_json(directory / "summary.json")
            count += len(data["samples"])
            hashes[directory.relative_to(ROOT).as_posix()] = hashlib.sha256((directory / "summary.json").read_bytes()).hexdigest()
            lines.append(f"- {label} Python：`{data['environment']['python']}`")
        lines += ["", "| 实验模式负载 | 旧版 ms | 新版 ms | 变化 % |", "|---|---:|---:|---:|"]
        for row in result["rows"]:
            if row["mode"] == "experiment" and row["metric"] == "command_wall_ms":
                lines.append(f"| {row['workload']} | {row['before_median']:.3f} | {row['after_median']:.3f} | {row['change_percent']:+.2f} |")
        lines += ["", f"[本批全部模式、资源与原始证据]({relative}/report.md)", "", f"![批次 {index} 全部样本]({relative}/version-comparison.png)", ""]
        inputs.append({"study": path.name, "condition_mismatches_from_first": mismatches})
    lines += [f"共 {count} 个样本。若环境信息与执行顺序同时变化，不能从批次差异中单独推导顺序效应。每批内部条件匹配，仍须考虑背景活动及缓存波动。",
              "", "长任务包含计划等待 4 秒。版本差值包含目标提交间整组功能变化，不是单项功能开关的因果对照。",
              "", "[按运行批次提取的 daemon 子阶段](daemon-stages/daemon-stages.md)"]
    destination.mkdir(parents=True, exist_ok=False)
    (destination / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    (destination / "plot-source.json").write_text(json.dumps({"aggregation": "stratified_no_pooling", "sample_count": count,
        "studies": inputs, "summary_sha256": hashes, "plot_script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}, indent=2) + "\n")
    print(destination / "report.md")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("study", type=Path)
    parser.add_argument("--replicate", type=Path, action="append", default=[], help="additional completed study of the same refs")
    parser.add_argument("--output", type=Path, help="new report directory; required when combining studies")
    parser.add_argument("--stratified", action="store_true", help="present batches separately without pooling samples")
    args = parser.parse_args()
    if args.stratified:
        if not args.output:
            parser.error("stratified reports need a new --output directory")
        stratified_report([args.study, *args.replicate], args.output)
        return
    state = json.loads((args.study / "study.json").read_text())
    if state["status"] != "pass":
        parser.error("study must have completed successfully")
    directories = {label: ROOT / state["runs"][label]["directory"] for label in ("before", "after")}
    comparison.compare(directories["before"], directories["after"])
    records = {label: json.loads((path / "summary.json").read_text()) for label, path in directories.items()}
    study_inputs = [{"study": args.study, "state": state, "directories": directories}]
    if args.replicate and args.output is None:
        parser.error("combined studies need a new --output directory")
    for path in args.replicate:
        other = json.loads((path / "study.json").read_text())
        if other["status"] != "pass" or other["refs"] != state["refs"] or other["workloads"] != state["workloads"]:
            parser.error("replicate must be a passing study of the same refs and workloads")
        additional = {label: ROOT / other["runs"][label]["directory"] for label in ("before", "after")}
        if any(additional[label] == original["directories"][label] for label in additional for original in study_inputs):
            parser.error("duplicate study would count samples twice")
        for label in additional:
            comparison.compare(directories[label], additional[label])
            records[label]["samples"].extend(json.loads((additional[label] / "summary.json").read_text())["samples"])
        study_inputs.append({"study": path, "state": other, "directories": additional})
    destination = args.output or args.study
    if args.output:
        destination.mkdir(parents=True, exist_ok=False)
    per_group = sum(item["state"]["samples_per_group"] for item in study_inputs)
    workloads = state["workloads"]
    modes = records["before"]["modes"]
    groups = {(label, workload, mode): [row for row in data["samples"] if row["workload"] == workload and row["mode"] == mode]
              for label, data in records.items() for workload in workloads for mode in modes}
    def median(label, workload, mode, metric):
        values = [row[metric] for row in groups[label, workload, mode] if row.get(metric) is not None]
        return statistics.median(values) if values else None
    def fmt(value):
        return "N/A" if value is None else f"{value:.3f}"
    fig, axes = plt.subplots((len(workloads) + 2) // 3, 3, figsize=(14, 8), layout="constrained", squeeze=False)
    for index, workload in enumerate(workloads):
        ax = axes.flat[index]
        for j, label in enumerate(("before", "after")):
            for k, mode in enumerate(modes):
                values = [row["command_wall_ms"] for row in groups[label, workload, mode]]
                x = k + (j - .5) * .34
                ax.bar(x, statistics.median(values), width=.30, color=("#287DA8", "#D27824")[j],
                       label=label if k == 0 else None, alpha=.82)
                ax.scatter([x + (i - (len(values)-1)/2)*.035 for i in range(len(values))], values,
                           color="#202B33", s=12, zorder=3)
        ax.set_xticks(range(len(modes)), modes)
        ax.set_title(workload + (" (4 s planned sleep)" if workload.startswith("paced_") else ""), fontsize=10)
        ax.set_ylabel("Command wall (ms)")
        ax.set_ylim(0, ax.get_ylim()[1] * 1.16)
        ax.grid(axis="y", alpha=.18)
        ax.set_axisbelow(True)
        ax.spines[["top", "right"]].set_visible(False)
        ax.legend(fontsize=8)
    for index in range(len(workloads), axes.size):
        axes.flat[index].set_visible(False)
    fig.suptitle(f"Target version comparison | release | {per_group} samples/group | bars: medians; dots: all samples\n"
                 "Whole-version comparison; no cache drop; sequential version order; not checkpoint on/off isolation", fontsize=12)
    for extension in ("png", "svg"):
        fig.savefig(destination / f"version-comparison.{extension}", dpi=170)
    plt.close(fig)
    lines = ["# 目标提交前后性能对照", "", f"旧版 `{state['refs']['before']}`；新版 `{state['refs']['after']}`。",
             "", f"批次顺序：{'；'.join(' → '.join(item['state']['order']) for item in study_inputs)}；汇总每组 {per_group} 次，release 构建。",
             "", "全部基准输出均通过预期结果检查，两版输入 fixture 和确定性输出一致。新版长任务要求至少两次周期保存，初始保存另算一次。",
             "", "这是整个版本的比较；差异范围以两个目标提交为准。不能把版本差值全部归为周期保存或输出摘要，也不能宣称已完成单项功能开关对照或优化。",
             "", "## 命令总耗时中位数", "", "单位 ms。正变化表示新版更慢，负变化表示本次样本中新版更快；不等同于显著性或因果结论。",
             "", "| 负载 | 模式 | 旧版 | 新版 | 变化 % |", "|---|---|---:|---:|---:|"]
    for workload in workloads:
        for mode in modes:
            old = median("before", workload, mode, "command_wall_ms")
            new = median("after", workload, mode, "command_wall_ms")
            lines.append(f"| {workload} | {mode} | {old:.3f} | {new:.3f} | {(new/old-1)*100:+.2f} |")
    lines += ["", "## 新版实验保存与资源", "",
              "周期保存次数为总成功摘要序号减去启动前的 1 次；报告大小仅指最终顶层文件长度，不能当作累计保存写入量。",
              "", "| 负载 | 周期保存次数范围 | 命令 ms | body ms | 实际等待 ms | active body ms | verify ms | daemon 采样峰值 MiB | 报告 KiB |",
              "|---|---|---:|---:|---:|---:|---:|---:|---:|"]
    if "experiment" in modes:
        for workload in workloads:
            rows = groups["after", workload, "experiment"]
            counts = [r["periodic_checkpoint_count"] for r in rows if r.get("periodic_checkpoint_count") is not None]
            span = f"{min(counts)}–{max(counts)}" if counts else "N/A"
            values = [median("after", workload, "experiment", key) for key in
                      ("command_wall_ms", "workload_body_ms", "sleep_observed_ms", "active_body_ms", "verify_cli_ms")]
            memory = median("after", workload, "experiment", "daemon_rss_sampled_peak_bytes")
            report_bytes = median("after", workload, "experiment", "report_metadata_bytes")
            lines.append(f"| {workload} | {span} | " + " | ".join(fmt(v) for v in values) +
                         f" | {fmt(memory/1048576 if memory is not None else None)} | {fmt(report_bytes/1024)} |")
    lines += ["", "## 边界与复查", "",
              "- 每个批次内版本按顺序执行而非逐样本交错；若汇总了正反两种批次顺序，也未严格控制后台活动、电源和缓存。原生/普通模式的漂移应一并查看，不能只挑实验模式的差值。",
              "- paced 负载每轮等待 0.5 秒，共 8 轮，计划等待 4 秒；固定读取量和等待策略相同。它不是持续吞吐测试。active body 只减去实际等待，不减去 FUSE 阻塞。",
              "- summary 中尚无独立的纯哈希/外部报告保存耗时，仍为 null。已有 daemon 管理操作可用 analyze-member5-stages.py 另行提取；此次没有保存开关、间隔对照或核心优化。",
              "- verify 的语义可能随版本增加整树指纹或输出校验；耗时变化要结合目标提交间的功能变化解释。",
              "- 每个正式目录保存 summary、fixture、CSV 和带逐文件哈希的 raw-evidence.zip；全部原始报告和源码快照位于 ZIP。",
              "- 此报告仅覆盖合成负载；真实案例使用独立报告。尚缺的 runner/纯哈希计时接口及最终整合验收不由此报告证明。", ""]
    for number, item in enumerate(study_inputs, 1):
        for label, directory in item["directories"].items():
            relative = Path(os.path.relpath(directory, destination)).as_posix()
            lines.append(f"- [批次 {number} {label} 原始汇总]({relative}/summary.json) / [证据归档]({relative}/raw-evidence.zip)")
        relative_study = Path(os.path.relpath(item["study"], destination)).as_posix()
        lines.append(f"- [批次 {number} 详细指标对比]({relative_study}/comparison/comparison.md)")
    lines += ["- [图表](version-comparison.png)"]
    (destination / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    (destination / "plot-source.json").write_text(json.dumps({
        "summary_sha256": {directory.relative_to(ROOT).as_posix(): hashlib.sha256((directory / "summary.json").read_bytes()).hexdigest()
                           for item in study_inputs for directory in item["directories"].values()},
        "sample_count": sum(len(data["samples"]) for data in records.values()),
        "samples_per_group": per_group,
        "plot_script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    }, indent=2) + "\n")
    print(destination / "report.md")


if __name__ == "__main__":
    main()
