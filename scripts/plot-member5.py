"""Render preserved samples; no benchmark rerun and no filtering of successful samples."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path, help="member5 result directory")
    args = parser.parse_args()
    source = args.run / "summary.json"
    data = json.loads(source.read_text(encoding="utf-8"))
    if data["status"] != "pass":
        raise SystemExit("Refusing to chart an incomplete/failed suite as a baseline")
    workloads, modes = data["workloads"], data["modes"]
    colors = {"native": "#546E7A", "teamfs": "#247BA0", "experiment": "#D57924"}
    labels = {"native": "Native Linux", "teamfs": "TeamFS", "experiment": "Experiment"}
    fig, axes = plt.subplots(1, 2, figsize=(12, 4.6), layout="constrained")
    for ax, field, title in zip(axes, ("command_wall_ms", "workload_body_ms"),
                               ("Whole command", "Workload body (includes configured sleeps)")):
        width = 0.75 / len(modes)
        for mode_index, mode in enumerate(modes):
            for workload_index, workload in enumerate(workloads):
                samples = [r[field] for r in data["samples"] if r["status"] == "pass"
                           and r["mode"] == mode and r["workload"] == workload]
                if not samples:
                    raise SystemExit(f"Missing samples: {mode}/{workload}")
                x = workload_index + (mode_index - (len(modes)-1)/2) * width
                median = statistics.median(samples)
                ax.bar(x, median, width=width * .88, color=colors[mode], alpha=.8,
                       label=labels[mode] if workload_index == 0 else None)
                offsets = [(i-(len(samples)-1)/2) * min(.025, width/len(samples)) for i in range(len(samples))]
                ax.scatter([x+offset for offset in offsets], samples, color="#1A2530", s=13, zorder=3)
                ax.annotate(f"{median:.1f}", (x, max(samples)), xytext=(0, 4),
                            textcoords="offset points", ha="center", fontsize=8)
        ax.set_xticks(range(len(workloads)), workloads)
        ax.set_ylabel("Milliseconds (linear scale)")
        ax.set_title(title, fontsize=11)
        ax.set_ylim(0, ax.get_ylim()[1] * 1.17)
        ax.grid(axis="y", alpha=.2)
        ax.set_axisbelow(True)
        ax.spines[["top", "right"]].set_visible(False)
        ax.legend(fontsize=8)
    fig.suptitle(f"TeamFS 0.8 synthetic baseline | {data['profile']} | {data['samples_per_group']} samples/group\n"
                 "Bars: medians; dots: all samples. Prepared/repeated reads; OS caches not cleared.", fontsize=11)
    for extension in ("png", "svg"):
        fig.savefig(args.run / f"timing-comparison.{extension}", dpi=180)
    plt.close(fig)
    (args.run / "chart-source.json").write_text(json.dumps({
        "source": "summary.json", "sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
        "plot_script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    }, indent=2) + "\n")
    print(args.run / "timing-comparison.png")


if __name__ == "__main__":
    main()
