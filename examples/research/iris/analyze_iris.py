#!/usr/bin/env python3
"""UCI Iris 三类特征均值的确定性多步骤分析（仅使用 Python 标准库）。

用法：
    python3 -I analyze_iris.py iris.data analysis-config.json iris-baseline.json

步骤：
    1. 读取并校验配置、样本数据和独立基线；
    2. 按类别分组，计算 4 个特征的组内均值与总体均值；
    3. 与基线逐项核对（不一致时继续输出，但标记 baseline_match=false）；
    4. 把确定性结果写入 $TEAMFS_OUTPUT/result.json。

校验失败时写入 $TEAMFS_OUTPUT/failure.json 并以退出码 2 结束。
"""

import hashlib
import json
import math
import os
import sys
from pathlib import Path

EXIT_INVALID = 2
SCOPE = "只对 iris.data 的 150 行记录计算分组均值；不读取文档、未使用数据文件或网络资源，也不据此提出新的植物学结论。"


def canonical(value):
    return json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def digest(data):
    return hashlib.sha256(data).hexdigest()


def write_failure(output, message, inputs):
    """把失败原因写成报告目录内的证据文件，而不是只留在终端。"""
    if output is not None:
        output.mkdir(parents=True, exist_ok=True)
        record = {
            "analysis": "uci-iris-class-means",
            "status": "failed",
            "error": message,
            "inputs": inputs,
            "scope": "失败记录只说明本次输入未通过校验，不作为科研结论。",
        }
        (output / "failure.json").write_text(canonical(record), encoding="utf-8")
    print(f"分析失败：{message}", file=sys.stderr)
    return EXIT_INVALID


def parse_config(config):
    analysis = config.get("analysis")
    if not isinstance(analysis, str) or not analysis:
        raise ValueError("analysis-config.json 缺少非空 analysis 字段")
    features = config.get("features")
    if (
        not isinstance(features, list)
        or len(features) != 4
        or len(set(features)) != 4
        or not all(isinstance(name, str) and name for name in features)
    ):
        raise ValueError("features 必须是 4 个互不相同的非空字符串")
    if config.get("data_columns") != 5:
        raise ValueError("data_columns 必须为 5（4 个特征加 1 个类别）")
    expected_rows = config.get("expected_rows")
    if not isinstance(expected_rows, int) or isinstance(expected_rows, bool) or expected_rows <= 0:
        raise ValueError("expected_rows 必须是正整数")
    classes = config.get("expected_classes")
    if not isinstance(classes, dict) or not classes:
        raise ValueError("expected_classes 必须是非空对象")
    for name, count in classes.items():
        if not isinstance(name, str) or not name:
            raise ValueError("expected_classes 的类别名必须是非空字符串")
        if not isinstance(count, int) or isinstance(count, bool) or count <= 0:
            raise ValueError(f"类别 {name} 的预期数量必须是正整数")
    decimals = config.get("rounding_decimals")
    if not isinstance(decimals, int) or isinstance(decimals, bool) or not 0 <= decimals <= 12:
        raise ValueError("rounding_decimals 必须是 0 到 12 之间的整数")
    tolerance = config.get("baseline_tolerance")
    if (
        not isinstance(tolerance, (int, float))
        or isinstance(tolerance, bool)
        or tolerance < 0
    ):
        raise ValueError("baseline_tolerance 必须是非负数")
    return analysis, features, expected_rows, dict(classes), decimals, float(tolerance)


def parse_data(text, columns, expected_rows, expected_classes):
    records = []
    for number, raw_line in enumerate(text.splitlines(), 1):
        line = raw_line.strip()
        if not line:
            continue
        parts = [part.strip() for part in line.split(",")]
        if len(parts) != columns:
            raise ValueError(f"iris.data 第 {number} 行有 {len(parts)} 列，预期 {columns} 列")
        try:
            values = [float(part) for part in parts[:4]]
        except ValueError:
            raise ValueError(f"iris.data 第 {number} 行包含非数值特征") from None
        if not all(math.isfinite(value) for value in values):
            raise ValueError(f"iris.data 第 {number} 行包含非有限数值")
        label = parts[4]
        if label not in expected_classes:
            raise ValueError(f"iris.data 第 {number} 行出现未知类别 {label!r}")
        records.append((values, label))
    if len(records) != expected_rows:
        raise ValueError(f"iris.data 有 {len(records)} 行，预期 {expected_rows} 行")
    counts = {name: 0 for name in expected_classes}
    for _, label in records:
        counts[label] += 1
    if counts != expected_classes:
        raise ValueError(f"类别分布为 {counts}，预期 {expected_classes}")
    return records


def check_baseline(baseline, features, class_means, tolerance):
    if baseline.get("feature_order") != features:
        raise ValueError("iris-baseline.json 的 feature_order 与配置不一致")
    reference = baseline.get("class_means")
    if not isinstance(reference, dict):
        raise ValueError("iris-baseline.json 缺少 class_means 对象")
    mismatches = []
    for name in sorted(class_means):
        expected = reference.get(name)
        if not isinstance(expected, list) or len(expected) != len(features):
            raise ValueError(f"iris-baseline.json 中类别 {name} 的均值列表无效")
        for feature, reference_value, computed in zip(features, expected, class_means[name]):
            if not isinstance(reference_value, (int, float)) or isinstance(reference_value, bool):
                raise ValueError(f"iris-baseline.json 中 {name}.{feature} 不是数值")
            if abs(computed - float(reference_value)) > tolerance:
                mismatches.append(
                    {
                        "class": name,
                        "feature": feature,
                        "baseline": float(reference_value),
                        "computed": round(computed, 9),
                    }
                )
    return {
        "matched": not mismatches,
        "mismatch_count": len(mismatches),
        "mismatches": mismatches[:12],
        "tolerance": tolerance,
        "note": "该核对只比较三类 4 个特征的均值；不一致不阻止输出，但会明确标记。",
    }


def main(argv):
    if len(argv) != 4:
        print(
            "用法：python3 -I analyze_iris.py <iris.data> <analysis-config.json> <iris-baseline.json>",
            file=sys.stderr,
        )
        return EXIT_INVALID
    output_raw = os.environ.get("TEAMFS_OUTPUT")
    output = Path(output_raw) if output_raw else None
    paths = [Path(value) for value in argv[1:]]
    inputs = {
        "data": {"path": argv[1]},
        "config": {"path": argv[2]},
        "baseline": {"path": argv[3]},
    }
    raw = {}
    try:
        for name, path in zip(("data", "config", "baseline"), paths):
            raw[name] = path.read_bytes()
            inputs[name]["sha256"] = digest(raw[name])
            inputs[name]["bytes"] = len(raw[name])
    except OSError as error:
        return write_failure(output, f"无法读取输入：{error}", inputs)

    try:
        config = json.loads(raw["config"].decode("utf-8"))
        baseline = json.loads(raw["baseline"].decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        return write_failure(output, f"JSON 解析失败：{error}", inputs)

    try:
        analysis, features, expected_rows, expected_classes, decimals, tolerance = parse_config(config)
        records = parse_data(
            raw["data"].decode("utf-8"), config["data_columns"], expected_rows, expected_classes
        )
    except (UnicodeDecodeError, ValueError, KeyError, TypeError) as error:
        return write_failure(output, str(error), inputs)

    raw_means = {}
    for name in sorted(expected_classes):
        subset = [values for values, label in records if label == name]
        raw_means[name] = [
            sum(values[index] for values in subset) / len(subset) for index in range(len(features))
        ]
    overall_raw = [
        sum(values[index] for values, _ in records) / len(records)
        for index in range(len(features))
    ]

    try:
        baseline_check = check_baseline(baseline, features, raw_means, tolerance)
    except ValueError as error:
        return write_failure(output, str(error), inputs)

    class_means = {
        name: {
            feature: round(raw_means[name][index], decimals)
            for index, feature in enumerate(features)
        }
        for name in sorted(expected_classes)
    }
    result = {
        "analysis": analysis,
        "status": "succeeded",
        "inputs": inputs,
        "dataset": {
            "rows": len(records),
            "features": features,
            "class_counts": {name: expected_classes[name] for name in sorted(expected_classes)},
        },
        "class_means": class_means,
        "overall_means": {
            feature: round(overall_raw[index], decimals)
            for index, feature in enumerate(features)
        },
        "rounding_decimals": decimals,
        "baseline_check": baseline_check,
        "scope": SCOPE,
    }
    if output is None:
        print("未设置 TEAMFS_OUTPUT，无法写出分析结果。", file=sys.stderr)
        return EXIT_INVALID
    output.mkdir(parents=True, exist_ok=True)
    text = canonical(result)
    (output / "result.json").write_text(text, encoding="utf-8")
    print(
        json.dumps(
            {
                "rows": len(records),
                "baseline_match": baseline_check["matched"],
                "result_sha256": digest(text.encode("utf-8")),
            },
            ensure_ascii=False,
            sort_keys=True,
        )
    )
    if not baseline_check["matched"]:
        print(
            f"警告：结果与独立基线有 {baseline_check['mismatch_count']} 项差异，已在 result.json 中标记。",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
