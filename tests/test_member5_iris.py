"""Reject wrong scientific results and incorrect output evidence before timing use."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

import member5_iris as case


class IrisChecks(unittest.TestCase):
    def setUp(self):
        self.expected = case.oracle(case.CASE)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.report = Path(self.temp.name)
        (self.report / "outputs").mkdir()

    def test_decimal_oracle_matches_known_public_statistics(self):
        self.assertEqual(self.expected["dataset"]["rows"], 150)
        self.assertEqual(self.expected["class_means"]["Iris-setosa"]["sepal_length_cm"], 5.006)
        self.assertEqual(self.expected["overall_means"]["petal_length_cm"], 3.758667)

    def test_wrong_statistics_rejected_even_if_program_reports_success(self):
        result = copy.deepcopy(self.expected)
        result.update(status="succeeded", baseline_check={"matched": True})
        result["class_means"]["Iris-setosa"]["sepal_length_cm"] += 1
        (self.report / "outputs/result.json").write_text(json.dumps(result))
        with self.assertRaisesRegex(ValueError, "oracle mismatch"):
            case.validate_result(self.report, self.expected)

    def test_baseline_failure_rejected(self):
        result = {**self.expected, "status": "succeeded", "baseline_check": {"matched": False}}
        (self.report / "outputs/result.json").write_text(json.dumps(result))
        with self.assertRaisesRegex(ValueError, "baseline check"):
            case.validate_result(self.report, self.expected)

    def test_tampered_output_and_unused_input_rejected(self):
        hashes = {name: "a" * 64 for name in case.INPUTS}
        for name in ("outputs/result.json", "stdout.txt", "stderr.txt"):
            (self.report / name).write_bytes(b"original")
        manifest = {"capture_complete": True, "phase": "finished", "exit_code": 0, "outcome": "succeeded",
                    "capture": {"inputs": [{"path_bytes": list(name.encode()), "sha256": digest, "access_class": "content_read"}
                                           for name, digest in hashes.items()], "unpinned_mount_accesses": [], "external_link_accesses": []},
                    "outputs": {"captured": True, "complete": True, "errors": [], "count": 3, "total_bytes": 24,
                                "files": [{"path_bytes": list(name.encode()), "bytes": 8, "sha256": case.bench.sha(self.report / name)}
                                          for name in ("outputs/result.json", "stdout.txt", "stderr.txt")]},
                    "environment": {"python3": "Python 3"}}
        path = self.report / "manifest.json"
        path.write_text(json.dumps(manifest))
        case.validate_manifest(self.report, hashes)
        (self.report / "outputs/result.json").write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "digest/size"):
            case.validate_manifest(self.report, hashes)
        (self.report / "outputs/result.json").write_bytes(b"original")
        manifest["capture"]["inputs"].append({"path_bytes": list(b"bezdekIris.data")})
        path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "unused files"):
            case.validate_manifest(self.report, hashes)


if __name__ == "__main__":
    unittest.main()
