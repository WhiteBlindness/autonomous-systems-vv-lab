from argparse import ArgumentTypeError as ArgparseError
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

from run_campaign import check_run_result, parse_seeds, summarize_invariants


class CampaignResultTests(unittest.TestCase):
    def setUp(self):
        self.report = {
            "status": "passed",
            "event_count": 1,
            "final_hash": "abc",
        }
        self.artifact = {
            "events": [{}],
            "event_count": 1,
            "final_hash": "abc",
            "expected_report": self.report.copy(),
        }

    def test_accepts_expected_success(self):
        expected = {"restricted_zone": True, "world_bounds": True, "safe_fallback": True, "valid_state_transitions": True}
        self.report["invariants"] = [
            {"name": name, "passed": passed, "failures": []}
            for name, passed in expected.items()
        ]
        self.artifact["expected_report"] = self.report.copy()
        self.assertEqual(
            check_run_result(0, self.report, self.artifact, "passed", expected), []
        )

    def test_accepts_expected_invariant_failure(self):
        report = {**self.report, "status": "invariant_failed"}
        artifact = {**self.artifact, "expected_report": report}
        expected = {"restricted_zone": False, "world_bounds": True, "safe_fallback": True, "valid_state_transitions": True}
        report["invariants"] = [
            {"name": name, "passed": passed, "failures": []}
            for name, passed in expected.items()
        ]
        artifact["expected_report"] = report
        self.assertEqual(
            check_run_result(2, report, artifact, "invariant_failed", expected), []
        )

    def test_rejects_unexpected_exit_code_or_status(self):
        expected = {"restricted_zone": False, "world_bounds": True, "safe_fallback": True, "valid_state_transitions": True}
        self.report["invariants"] = [
            {"name": name, "passed": True, "failures": []}
            for name in expected
        ]
        self.artifact["expected_report"] = self.report.copy()
        errors = check_run_result(0, self.report, self.artifact, "invariant_failed", expected)
        self.assertTrue(any("expected exit code" in error for error in errors))
        self.assertTrue(any("expected report status" in error for error in errors))

    def test_rejects_infrastructure_exit_and_inconsistent_artifact(self):
        artifact = {**self.artifact, "event_count": 2}
        expected = {"restricted_zone": True, "world_bounds": True, "safe_fallback": True, "valid_state_transitions": True}
        self.report["invariants"] = [
            {"name": name, "passed": passed, "failures": []}
            for name, passed in expected.items()
        ]
        self.artifact["expected_report"] = self.report.copy()
        errors = check_run_result(1, self.report, artifact, "passed", expected)
        self.assertTrue(any("failed with exit code 1" in error for error in errors))
        self.assertTrue(any("event_count" in error for error in errors))

    def test_seed_parser_requires_unique_u64_values(self):
        self.assertEqual(parse_seeds("42, 1337"), (42, 1337))
        with self.assertRaises(ArgparseError):
            parse_seeds("42,42")
        with self.assertRaises(ArgparseError):
            parse_seeds("-1")

    def test_invariant_summary_separates_expected_and_unexpected_failures(self):
        cases = [
            {
                "scenario": "restricted-zone-failure",
                "seed": 42,
                "expected_invariants": {"restricted_zone": False},
                "invariants": {"restricted_zone": False},
                "invariant_failure_evidence": {
                    "restricted_zone": [{"tick": 4, "expected": "outside zone"}]
                },
                "output_paths": {"report": "runs/zone/report.json"},
            },
            {
                "scenario": "basic-mission",
                "seed": 99,
                "expected_invariants": {"restricted_zone": True},
                "invariants": {"restricted_zone": False},
            },
        ]
        counts, failures = summarize_invariants(cases)
        self.assertEqual(counts["restricted_zone"]["checks"], 2)
        self.assertEqual(counts["restricted_zone"]["failed"], 2)
        self.assertEqual(counts["restricted_zone"]["expected_failures"], 1)
        self.assertEqual(counts["restricted_zone"]["unexpected_failures"], 1)
        self.assertEqual(failures["restricted_zone"][0]["seed"], 42)
        self.assertEqual(
            failures["restricted_zone"][0]["evidence"][0]["tick"], 4
        )


if __name__ == "__main__":
    unittest.main()
