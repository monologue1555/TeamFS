"""Guard against invalid scientific comparisons; fixtures are synthetic test records."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("compare_member5", Path(__file__).resolve().parents[1] / "scripts/compare-member5.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ComparisonTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.before = Path(self.temp.name) / "before"
        self.after = Path(self.temp.name) / "after"
        self.record = {
            "schema_version": 1, "status": "pass", "samples_per_group": 3,
            "workloads": ["small"], "modes": ["native"], "profile": "release", "measurement_notes": ["same"],
            "environment": {key: "same" for key in module.ENVIRONMENT},
            "source": {"commit": "before", "sha256": {key: "hash" for key in module.MEASUREMENT_SOURCES}},
            "samples": [{"workload": "small", "mode": "native", "repetition": i,
                         "status": "pass", "output_matches_oracle": True, "file_count": 1, "file_bytes": 4096,
                         "rounds": 3, "result_file_sha256": "a" * 64,
                         **{key: i * 10 for key in module.REQUIRED}} for i in (1, 2, 3)],
        }
        self.save(self.before, self.record)
        self.save(self.after, self.record)

    def save(self, directory, record):
        directory.mkdir(exist_ok=True)
        (directory / "summary.json").write_text(json.dumps(record))
        (directory / "fixture-small.json").write_text(json.dumps({"count": 1, "file_bytes": 4096, "rounds": 3, "file_sha256": {"data/0000.bin": "a" * 64}}))

    def changed(self, change):
        record = copy.deepcopy(self.record)
        change(record)
        self.save(self.after, record)

    def test_medians_and_missing_stages(self):
        self.changed(lambda r: [row.update(command_wall_ms=row["command_wall_ms"] * 2) for row in r["samples"]])
        result = module.compare(self.before, self.after)
        wall = next(row for row in result["rows"] if row["metric"] == "command_wall_ms")
        self.assertEqual((wall["before_median"], wall["after_median"], wall["change_percent"]), (20, 40, 100))
        stage = next(row for row in result["rows"] if row["metric"] == "snapshot_ms")
        self.assertIsNone(stage["change_percent"])
        self.assertIsNone(stage["before_median"])

    def test_failed_run_is_rejected_even_with_override(self):
        self.changed(lambda r: r.update(status="fail"))
        with self.assertRaises(ValueError):
            module.compare(self.before, self.after, True)

    def test_smoke_run_is_rejected(self):
        self.changed(lambda r: r.update(samples_per_group=1, samples=r["samples"][:1]))
        with self.assertRaises(ValueError):
            module.compare(self.before, self.after)

    def test_duplicate_repetition_is_rejected(self):
        self.changed(lambda r: r["samples"][2].update(repetition=2))
        with self.assertRaises(ValueError):
            module.compare(self.before, self.after)

    def test_changed_inputs_cannot_be_overridden(self):
        path = self.after / "fixture-small.json"
        fixture = json.loads(path.read_text())
        fixture["file_sha256"]["data/0000.bin"] = "b" * 64
        path.write_text(json.dumps(fixture))
        with self.assertRaises(ValueError):
            module.compare(self.before, self.after, True)

    def test_different_outputs_cannot_be_overridden(self):
        self.changed(lambda r: [row.update(result_file_sha256="b" * 64) for row in r["samples"]])
        with self.assertRaises(ValueError):
            module.compare(self.before, self.after, True)

    def test_environment_difference_requires_explicit_exploratory_mode(self):
        self.changed(lambda r: r["environment"].update(cpu_model="different"))
        with self.assertRaises(ValueError):
            module.compare(self.before, self.after)
        result = module.compare(self.before, self.after, True)
        self.assertEqual(result["assessment"], "exploratory_conditions_differ")
        self.assertIn("environment.cpu_model", result["condition_mismatches"])

    def test_profile_and_measurement_source_difference(self):
        self.changed(lambda r: r.update(profile="debug"))
        with self.assertRaises(ValueError):
            module.compare(self.before, self.after)
        self.changed(lambda r: r["source"]["sha256"].update({module.MEASUREMENT_SOURCES[0]: "changed"}))
        with self.assertRaises(ValueError):
            module.compare(self.before, self.after)

    def test_missing_nonfinite_and_partial_metrics(self):
        for change in (lambda r: r["samples"][0].pop("command_wall_ms"),
                       lambda r: r["samples"][0].update(command_wall_ms=float("nan")),
                       lambda r: r["samples"][0].update(snapshot_ms=2)):
            with self.subTest(change=change):
                self.changed(change)
                with self.assertRaises(ValueError):
                    module.compare(self.before, self.after)

    def test_zero_baseline_has_no_percentage(self):
        for directory in (self.before, self.after):
            record = copy.deepcopy(self.record)
            for row in record["samples"]:
                row["command_wall_ms"] = 0
            self.save(directory, record)
        result = module.compare(self.before, self.after)
        wall = next(row for row in result["rows"] if row["metric"] == "command_wall_ms")
        self.assertEqual(wall["delta"], 0)
        self.assertIsNone(wall["change_percent"])

    def test_storage_keeps_each_measurement_point_separate(self):
        for directory, amount in ((self.before, 4096), (self.after, 8192)):
            record = copy.deepcopy(self.record)
            for row in record["samples"]:
                row["storage_after_run"] = {"state.sqlite3-wal": amount}
                row["storage_after_unmount"] = {"state.sqlite3-wal": 0}
            self.save(directory, record)
        result = module.compare(self.before, self.after)
        live = next(r for r in result["rows"] if r["metric"] == "storage_after_run/state.sqlite3-wal")
        closed = next(r for r in result["rows"] if r["metric"] == "storage_after_unmount/state.sqlite3-wal")
        self.assertEqual(live["change_percent"], 100)
        self.assertEqual(closed["delta"], 0)


if __name__ == "__main__":
    unittest.main()
