"""Consolidated regression record for scripts/check.sh.

Reads the suite runs that check.sh appended (one JSON object per line), re-reads each suite's own
log and JSON report, and writes artifacts/regression-summary.json and .md. A suite only counts as
passed when it exited 0 and every artifact it owns was rewritten during this run; unsupported and
environment-skip cases are listed separately and never counted as passes.
"""
import collections
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
ART = ROOT / "artifacts"
STATUS = re.compile(r"^(PASS|FAIL|UNSUPPORTED|ENVIRONMENT_SKIP)\b(?: \d+)?:? ?(.*)$")
# Suite -> (log, JSON report in the shared Report format or None) pairs written by its script.
SUITES = {
    "unit": [("model-tests.txt", None)],
    "smoke": [("acceptance.txt", None)],
    "v2": [("v2-acceptance.txt", None)],
    "v3": [("v3-acceptance.txt", None)],
    "v4": [("v4-acceptance.txt", None)],
    "v5": [("v5-acceptance.txt", None)],
    "v6": [("v6-acceptance.txt", "v6-acceptance.json"), ("compatibility-v6.txt", "compatibility-v6.json"),
           ("randomized-v6.txt", "randomized-v6.json")],
    "blocks": [("block-foundation.txt", None)],
    "experiments": [("experiment-acceptance.txt", None)],
    "attribution": [("attribution-acceptance.txt", "attribution-acceptance.json")],
    "member3": [("member3-acceptance.txt", None)],
}
HIGHLIGHTS = ("Git workflow", "rsync round trip", "repeated Vim saves", "as experiment commands")


def command(*args):
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def parse_log(path):
    """Status counts from a text log: cargo's result lines, or `PASS NN:`-style case lines."""
    counts = collections.Counter()
    for line in path.read_text(errors="replace").splitlines():
        if path.name == "model-tests.txt":
            m = re.match(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored", line)
            if m:
                counts.update({"pass": int(m[1]), "fail": int(m[2]), "ignored": int(m[3])})
        elif (m := STATUS.match(line)):
            counts[m[1].lower()] += 1
    return counts


def main(runs_path):
    runs = [json.loads(line) for line in Path(runs_path).read_text().splitlines() if line.strip()]
    meta = runs.pop(0)
    suites, non_passing, highlights = [], [], []
    for run in runs:
        started = run["started_ns"] / 1e9
        owned = [name for pair in SUITES[run["suite"]] for name in pair if name]
        stale = [name for name in owned if not (ART / name).exists() or (ART / name).stat().st_mtime < started - 1]
        counts = collections.Counter()
        for log_name, report_name in SUITES[run["suite"]]:
            if report_name is None:
                if log_name not in stale:
                    counts.update(parse_log(ART / log_name))
                continue
            if report_name in stale:
                continue
            # Report-format suites: the JSON is authoritative for their cases.
            for case in json.loads((ART / report_name).read_text())["cases"]:
                counts[case["status"]] += 1
                if case["status"] != "pass":
                    non_passing.append({"suite": run["suite"], "report": report_name, "status": case["status"],
                                        "name": case["name"], "detail": str(case["detail"]).strip()[:600]})
                if any(h in case["name"] for h in HIGHLIGHTS):
                    highlights.append({"suite": run["suite"], "name": case["name"], "status": case["status"]})
        counts = dict(counts)
        passed = run["exit_code"] == 0 and not stale and not counts.get("fail")
        suites.append({"suite": run["suite"], "command": run["command"], "exit_code": run["exit_code"],
                       "seconds": round(run["elapsed_ns"] / 1e9, 2), "passed": passed, "counts": counts,
                       "artifacts": owned, "stale_or_missing_artifacts": stale})
    failed = [s["suite"] for s in suites if not s["passed"]]
    totals = collections.Counter()
    for suite in suites:
        totals.update(suite["counts"])
    summary = {
        "schema_version": 1,
        "measured_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "check_command": "bash scripts/check.sh",
        "exit_code": 1 if failed else 0,
        "source": meta["source"],
        "environment": {
            "kernel": platform.release(),
            "os": next((l.split("=", 1)[1].strip('"') for l in Path("/etc/os-release").read_text().splitlines()
                        if l.startswith("PRETTY_NAME=")), platform.platform()) if Path("/etc/os-release").exists() else platform.platform(),
            "wsl": "microsoft" in platform.release().lower(),
            "rustc": command("rustc", "--version"), "cargo": command("cargo", "--version"),
            "libfuse": command("pkg-config", "--modversion", "fuse"), "fusermount3": command("fusermount3", "--version"),
            "python": platform.python_version(), "uid": os.getuid(), "build": "debug",
            "require_tools": os.environ.get("TEAMFS_REQUIRE_TOOLS") == "1",
            "ci": os.environ.get("GITHUB_ACTIONS") == "true",
            "ci_run": f"{os.environ['GITHUB_SERVER_URL']}/{os.environ['GITHUB_REPOSITORY']}/actions/runs/{os.environ['GITHUB_RUN_ID']}"
            if os.environ.get("GITHUB_RUN_ID") else None,
        },
        "totals": dict(totals),
        "suites": suites,
        "failed_suites": failed,
        "non_passing_cases": non_passing,
        "application_highlights": highlights,
        "scope": "Bounded mounted scenarios on this machine and build. Unsupported and environment-skip cases are "
                 "listed separately and never counted as passes; this is not a POSIX, production or arbitrary-application guarantee.",
    }
    (ART / "regression-summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
    (ART / "regression-summary.md").write_text(markdown(summary))
    print(markdown(summary))
    return 1 if failed else 0


def markdown(s):
    src, env = s["source"], s["environment"]
    lines = [
        "# TeamFS 全套回归记录",
        "",
        f"- 结论：{'通过' if s['exit_code'] == 0 else '未通过：' + '、'.join(s['failed_suites'])}（`{s['check_command']}` 退出码 {s['exit_code']}）",
        f"- 源码提交：`{src['commit']}`{'（运行前工作区有未提交的源码修改）' if src['dirty'] else ''}；分支 {src['branch']}",
        f"- 环境：{env['os']}，内核 {env['kernel']}，{env['rustc']}，libfuse {env['libfuse']}，Python {env['python']}，uid {env['uid']}，debug 构建",
        f"- 时间：{s['measured_at']}" + (f"；CI：{env['ci_run']}" if env["ci_run"] else ""),
        "",
        "| 套件 | 退出码 | 通过 | 失败 | 未支持 | 环境跳过 | 耗时 s |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for suite in s["suites"]:
        c = suite["counts"]
        name = suite["suite"] + ("（产物缺失或未更新）" if suite["stale_or_missing_artifacts"] else "")
        lines.append(f"| {name} | {suite['exit_code']} | {c.get('pass', 0)} | {c.get('fail', 0)} | "
                     f"{c.get('unsupported', 0)} | {c.get('environment_skip', 0)} | {suite['seconds']} |")
    t = s["totals"]
    lines += [f"| 合计 | | {t.get('pass', 0)} | {t.get('fail', 0)} | {t.get('unsupported', 0)} | {t.get('environment_skip', 0)} | |", ""]
    lines += ["## 应用场景（Git、Vim、rsync 及实验模式工具）", ""]
    lines += [f"- {h['status']}：{h['suite']} / {h['name']}" for h in s["application_highlights"]] or ["- 无"]
    lines += ["", "## 未支持与跳过项（单列，不计为通过）", ""]
    lines += [f"- {c['status']}：{c['suite']} / {c['name']} — {c['detail'].splitlines()[0] if c['detail'] else ''}"
              for c in s["non_passing_cases"]] or ["- 无"]
    lines += ["", s["scope"], ""]
    return "\n".join(lines)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
