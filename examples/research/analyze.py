"""Deterministic teaching experiment; all file inputs reside beside this script."""
import csv
import json
import os
from pathlib import Path

base = Path(__file__).resolve().parent
with (base / "measurements.csv").open() as stream:
    values = [float(row["value"]) for row in csv.DictReader(stream)]
config = json.loads((base / "parameters.json").read_text())
bias = float((base / "calibration.txt").read_text())
result = {"sample_count": len(values), "mean": sum(values) / len(values),
          "calibrated_mean": sum(values) / len(values) * config["scale"] + bias}
target = Path(os.environ["TEAMFS_OUTPUT"]) / "result.json"
target.write_text(json.dumps(result, sort_keys=True, indent=2) + "\n")
print(json.dumps(result, sort_keys=True))
