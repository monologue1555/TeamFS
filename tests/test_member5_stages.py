import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("member5_stages", Path(__file__).resolve().parents[1] / "scripts/analyze-member5-stages.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def metric(name, calls=1, total=1000, commit=0):
    return {"operation": "management." + name, "calls": calls, "total_us": total,
            "commit_total_us": commit, "errors": 0}


class StageTests(unittest.TestCase):
    def test_nested_commit_kept_separate(self):
        result = module.extract({}, {"operations": [metric("experiment_start", total=10000, commit=8000), metric("experiment_finish")]})
        self.assertEqual(result["daemon_start_total_ms"], 10)
        self.assertEqual(result["daemon_start_commit_ms"], 8)
        self.assertIsNone(result["daemon_checkpoint_total_ms"])

    def test_checkpoint_counts_reconcile_with_report(self):
        metrics = {"operations": [metric("experiment_start"), metric("experiment_finish"),
                                  metric("experiment_checkpoint", calls=5, total=75000)]}
        result = module.extract({"checkpoint_sequence": 5}, metrics)
        self.assertEqual(result["daemon_checkpoint_total_ms"], 75)
        self.assertEqual(result["daemon_checkpoint_mean_ms"], 15)
        with self.assertRaises(ValueError):
            module.extract({"checkpoint_sequence": 4}, metrics)

    def test_missing_required_checkpoint_metric_rejected(self):
        with self.assertRaises(ValueError):
            module.extract({"checkpoint_sequence": 2}, {"operations": [metric("experiment_start"), metric("experiment_finish")]})

    def test_invalid_nested_duration_rejected(self):
        with self.assertRaises(ValueError):
            module.extract({}, {"operations": [metric("experiment_start", total=1000, commit=1001), metric("experiment_finish")]})


if __name__ == "__main__":
    unittest.main()
