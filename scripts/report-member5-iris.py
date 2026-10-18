"""Render completed public-case evidence without mixing it with synthetic samples."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import zipfile

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    summary = json.loads((args.run / "summary.json").read_bytes())
    archive = json.loads((args.run / "evidence-archive.json").read_bytes())
    sha = lambda data: hashlib.sha256(data).hexdigest()
    if summary["kind"] != "member5_public_iris" or summary["status"] != "pass":
        raise ValueError("expected a completed Iris study")
    if sha((args.run / archive["archive"]).read_bytes()) != archive["archive_sha256"]:
        raise ValueError("archive hash mismatch")
    with zipfile.ZipFile(args.run / archive["archive"]) as saved:
        for name, digest in archive["files_sha256"].items():
            if sha(saved.read(name)) != digest:
                raise ValueError("archived evidence changed: " + name)
        if saved.read("summary.json") != (args.run / "summary.json").read_bytes():
            raise ValueError("summary differs from archive")
    modes = summary["modes"]
    groups = {mode: [row for row in summary["samples"] if row["mode"] == mode] for mode in modes}
    for mode, rows in groups.items():
        if sorted(row["repetition"] for row in rows) != list(range(1, summary["samples_per_group"] + 1)):
            raise ValueError("incomplete or duplicate repetitions")
        for row in rows:
            if row["status"] != "pass" or row["output_matches_oracle"] is not True:
                raise ValueError("invalid result")
            if mode == "experiment" and not all(row.get(k) is True for k in ("output_digests_verified", "replay_byte_identical", "compare_confirmed")):
                raise ValueError("missing replay/output evidence")
    if len({row["result_file_sha256"] for row in summary["samples"]}) != 1:
        raise ValueError("results differ between modes")
    args.output.mkdir(parents=True, exist_ok=False)
    def median(mode, key):
        values = [row[key] for row in groups[mode] if row.get(key) is not None]
        return statistics.median(values) if values else None
    def fmt(value):
        return "N/A" if value is None else f"{value:.3f}"
    fig, ax = plt.subplots(figsize=(8, 4.8), layout="constrained")
    for index, mode in enumerate(modes):
        values = [row["command_wall_ms"] for row in groups[mode]]
        ax.bar(index, statistics.median(values), color=("#70818C", "#287DA8", "#D27824")[index], width=.55)
        ax.scatter([index + (i-(len(values)-1)/2)*.045 for i in range(len(values))], values, color="#202B33", s=22, zorder=3)
    ax.set_xticks(range(len(modes)), modes)
    ax.set_ylabel("Command wall (ms)")
    ax.set_title(f"Public UCI Iris | 150 rows | release | n={summary['samples_per_group']}/mode\nBars: medians; dots: all samples; no cache drop")
    ax.set_axisbelow(True)
    ax.grid(axis="y", alpha=.2)
    ax.spines[["top", "right"]].set_visible(False)
    for extension in ("png", "svg"):
        fig.savefig(args.output / ("iris-comparison." + extension), dpi=170)
    plt.close(fig)
    import os
    relative = Path(os.path.relpath(args.run, args.output)).as_posix()
    lines = ["# 公开 Iris 案例性能评估", "", f"目标 `{summary['source']['commit']}`，release，每组 {summary['samples_per_group']} 次。",
             "", "使用成员 4 原始分析程序与 UCI 公开数据，150 行、4 个特征、3 类；数据文件 4551 字节。全部结果通过独立 Decimal 均值复算，三组输出逐字节一致。",
             "", "| 模式 | 命令中位数 ms | 最小 ms | 最大 ms | daemon 采样峰值 MiB | 最终报告元数据字节 |",
             "|---|---:|---:|---:|---:|---:|"]
    for mode, rows in groups.items():
        values = [row["command_wall_ms"] for row in rows]
        memory = median(mode, "daemon_rss_sampled_peak_bytes")
        lines.append(f"| {mode} | {fmt(statistics.median(values))} | {fmt(min(values))} | {fmt(max(values))} | {fmt(memory/1048576 if memory is not None else None)} | {fmt(median(mode, 'report_metadata_bytes'))} |")
    lines += ["", "## 独立的后续命令", "", "以下命令在原实验结束后执行，不能与其当作嵌套阶段相加。重放前修改当前同名 iris.data，重放仍需使用旧快照并得到相同输出。", "",
              "| 命令 | n | 中位数 ms |", "|---|---:|---:|"]
    for key in ("verify_cli_ms", "replay_cli_ms", "replay_verify_cli_ms", "compare_cli_ms"):
        lines.append(f"| {key} | {len(groups['experiment'])} | {fmt(median('experiment', key))} |")
    lines += ["", "## 测量边界", "",
              "- 各样本独立挂载和存储，模式轮换；输入准备、挂载与归档不计入命令耗时，没有清理系统缓存。",
              "- 实验命令包含快照、初始摘要、输入/输出哈希、环境采集和报告发布；无法把模式差值全部归因于输出摘要。",
              "- 三组均写出同样的业务结果，原生/普通模式没有实验 manifest。元数据字节指最终文件长度，不是物理写入量。",
              "- 保持成员 4 分析程序原样，未注入 body/进程 RSS 计时，这两项为 N/A。",
              "- 小数据案例主要体现启动和追溯固定成本，不代表大规模科学计算吞吐；单次模式顺序也未严格控制后台活动。",
              "- 输出摘要按实际文件大小与 SHA256 核验；报告比较确认输入与输出相同；每次原实验和重放均实际 verify。",
              "", "![Iris 三组耗时](iris-comparison.png)", "",
              f"[原始汇总]({relative}/summary.json) / [完整证据 ZIP]({relative}/raw-evidence.zip)"]
    (args.output / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    (args.output / "plot-source.json").write_text(json.dumps({"summary_sha256": sha((args.run / "summary.json").read_bytes()),
        "archive_sha256": archive["archive_sha256"], "plot_script_sha256": sha(Path(__file__).read_bytes()), "samples": len(summary["samples"])}, indent=2) + "\n")
    print(args.output / "report.md")


if __name__ == "__main__":
    main()
