from __future__ import annotations

import argparse
import copy
import json
import os
import sys
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

from m3_campaign import (  # noqa: E402
    ExpectedOutcome,
    M3CampaignError,
    classify_outputs,
    load_m3_manifest,
    m3_fixture_checksums,
    parse_seeds,
    plan_matrix,
    validate_m3_manifest,
)
from check_platform_artifacts import (
    CampaignError,
    merge_fingerprints,
    seed42_outputs,
)  # noqa: E402


def compiled_binary() -> Path | None:
    names = ("vv-lab.exe", "vv-lab") if os.name == "nt" else ("vv-lab", "vv-lab.exe")
    binary = next(
        (
            ROOT / "target" / configuration / name
            for configuration in ("release", "debug")
            for name in names
            if (ROOT / "target" / configuration / name).is_file()
        ),
        None,
    )
    if binary is None:
        return None
    probe = subprocess.run(
        [
            str(binary),
            "run",
            str(ROOT / "scenarios" / "m3" / "nominal-stop.json"),
            "--seed",
            "42",
            "--output",
            str(ROOT / "runs" / "m3-cli-probe-not-created"),
            "--contracts",
            str(ROOT / "scenarios" / "m3" / "missing-probe-contracts.json"),
            "--actuator",
            str(ROOT / "scenarios" / "m3" / "actuator-none.json"),
            "--max-output-bytes",
            "1024",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=3,
        check=False,
    )
    diagnostic = (probe.stdout + probe.stderr).lower()
    if "unknown run option: --contracts" in diagnostic:
        return None
    return binary


M3_BINARY = compiled_binary()


def valid_manifest() -> dict:
    expected = {
        "temporal": "PASS",
        "safety": "passed",
        "progress": "completed",
        "contracts": {
            "fallback_response": "PASS",
            "actuator_stop": "PASS",
        },
    }
    return {
        "schema_version": 1,
        "default_seeds": [42, 1337],
        "contract_sets": {"base": "contracts/base.json"},
        "fault_combinations": {
            "nominal": "actuators/nominal.json",
            "stuck": "actuators/stuck.json",
        },
        "cases": [
            {
                "id": "nominal-run",
                "scenario": "nominal.json",
                "contract_set": "base",
                "fault_combination": "nominal",
                "expected_by_seed": {
                    "42": copy.deepcopy(expected),
                    "1337": copy.deepcopy(expected),
                },
                "minimized_counterexample": {
                    "original": "minimized/original/events.json",
                    "reduced": "minimized/final/events.json",
                    "summary": "minimized/summary.json",
                },
            },
            {
                "id": "stuck-stop",
                "scenario": "stop.json",
                "contract_set": "base",
                "fault_combination": "stuck",
                "expected_by_seed": {
                    "42": {
                        "temporal": "FAIL",
                        "safety": "passed",
                        "progress": "incomplete",
                        "contracts": {
                            "fallback_response": "PASS",
                            "actuator_stop": "FAIL",
                        },
                    },
                    "1337": {
                        "temporal": "FAIL",
                        "safety": "passed",
                        "progress": "incomplete",
                        "contracts": {
                            "fallback_response": "PASS",
                            "actuator_stop": "FAIL",
                        },
                    },
                },
            },
        ],
        "budgets": {
            "max_cases": 80,
            "max_runs": 256,
            "max_artifact_bytes": 67_108_864,
            "time_budget_seconds": 120,
        },
    }


class M3ManifestValidationTests(unittest.TestCase):
    def test_repository_manifest_lists_the_bounded_m3_catalogue_and_all_input_hashes(self):
        manifest = load_m3_manifest()
        ids = {case["id"] for case in manifest["cases"]}
        self.assertEqual(
            ids,
            {
                "nominal-control",
                "gps-fallback-timely",
                "gps-fallback-late",
                "compliant-stop",
                "ignored-stop",
                "late-command-after-fallback",
                "simultaneous-gps-actuator-faults",
                "intermittent-actuation-recovery",
                "safe-incomplete-mission",
                "unsafe-complete-mission",
            },
        )
        plan = plan_matrix(manifest)
        self.assertEqual(len(plan.cases), 30)
        self.assertEqual(plan.simulator_invocations, 90)
        checksums = m3_fixture_checksums(manifest)
        self.assertIn("m3/expectations.json", checksums)
        expected_inputs = {
            "expectations.json",
            *(case["scenario"] for case in manifest["cases"]),
            *manifest["contract_sets"].values(),
            *manifest["fault_combinations"].values(),
        }
        self.assertEqual(len(checksums), len(expected_inputs))

    def test_accepts_versioned_cases_with_separate_temporal_safety_and_progress_outcomes(self):
        manifest = validate_m3_manifest(valid_manifest())

        self.assertEqual(manifest["schema_version"], 1)
        self.assertEqual(manifest["default_seeds"], [42, 1337])
        self.assertEqual(manifest["cases"][1]["expected_by_seed"]["42"]["temporal"], "FAIL")
        self.assertEqual(manifest["cases"][1]["expected_by_seed"]["42"]["safety"], "passed")
        self.assertEqual(manifest["cases"][1]["expected_by_seed"]["42"]["progress"], "incomplete")

    def test_rejects_unknown_versions_fields_and_contract_references(self):
        unsupported = valid_manifest()
        unsupported["schema_version"] = 2
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(unsupported)

        unknown_field = valid_manifest()
        unknown_field["surprise"] = True
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(unknown_field)

        unknown_contract_set = valid_manifest()
        unknown_contract_set["cases"][0]["contract_set"] = "missing"
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(unknown_contract_set)

        unknown_fault_combo = valid_manifest()
        unknown_fault_combo["cases"][0]["fault_combination"] = "missing"
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(unknown_fault_combo)

    def test_rejects_duplicate_case_ids_and_seed_expectations_without_a_boolean_as_integer(self):
        duplicate_id = valid_manifest()
        duplicate_id["cases"][1]["id"] = duplicate_id["cases"][0]["id"]
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(duplicate_id)

        boolean_seed = valid_manifest()
        boolean_seed["default_seeds"] = [True]
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(boolean_seed)

        boolean_budget = valid_manifest()
        boolean_budget["budgets"]["max_runs"] = True
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(boolean_budget)

    def test_rejects_unknown_outcomes_missing_contracts_and_unsafe_paths(self):
        invalid_status = valid_manifest()
        invalid_status["cases"][0]["expected_by_seed"]["42"]["temporal"] = "MAYBE"
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(invalid_status)

        missing_contract = valid_manifest()
        missing_contract["cases"][0]["expected_by_seed"]["42"]["contracts"] = {}
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(missing_contract)

        escaping_scenario = valid_manifest()
        escaping_scenario["cases"][0]["scenario"] = "../outside.json"
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(escaping_scenario)

        platform_specific_path = valid_manifest()
        platform_specific_path["cases"][0]["scenario"] = r"nested\scenario.json"
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(platform_specific_path)

        escaping_reference = valid_manifest()
        escaping_reference["cases"][0]["minimized_counterexample"]["reduced"] = "../events.json"
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(escaping_reference)

    def test_rejects_conflicting_aggregate_and_contract_statuses(self):
        inconsistent_pass = valid_manifest()
        inconsistent_pass["cases"][0]["expected_by_seed"]["42"]["temporal"] = "INCONCLUSIVE"
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(inconsistent_pass)

        inconsistent_failure = valid_manifest()
        inconsistent_failure["cases"][1]["expected_by_seed"]["42"]["temporal"] = "INCONCLUSIVE"
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(inconsistent_failure)

    def test_rejects_contract_set_drift_between_seed_expectations(self):
        manifest = valid_manifest()
        manifest["cases"][0]["expected_by_seed"]["1337"]["contracts"].pop("actuator_stop")
        with self.assertRaises(M3CampaignError):
            validate_m3_manifest(manifest)

    def test_loader_rejects_duplicate_json_fields(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as temporary:
            path = Path(temporary) / "expectations.json"
            path.write_text('{"schema_version":1,"schema_version":1}', encoding="utf-8")

            with self.assertRaises(M3CampaignError):
                load_m3_manifest(path)

    def test_seed_parser_requires_unique_bounded_unsigned_values(self):
        self.assertEqual(parse_seeds("42, 1337"), (42, 1337))
        for value in ("", "42,42", "-1", str(2**64), ",", "1,"):
            with self.subTest(value=value), self.assertRaises(argparse.ArgumentTypeError):
                parse_seeds(value)


class M3MatrixPlanningTests(unittest.TestCase):
    def test_builds_deterministic_case_seed_matrix_with_resolved_property_and_fault_paths(self):
        manifest = validate_m3_manifest(valid_manifest())

        plan = plan_matrix(
            manifest,
            contract_sets=("base",),
            scenario_names=("nominal-run", "stuck-stop"),
            fault_combinations=("nominal", "stuck"),
            seeds=(42, 1337),
        )

        self.assertEqual(len(plan.cases), 4)
        self.assertEqual(plan.simulator_invocations, 12)
        self.assertEqual(
            [(case.case_id, case.seed) for case in plan.cases],
            [
                ("nominal-run", 42),
                ("nominal-run", 1337),
                ("stuck-stop", 42),
                ("stuck-stop", 1337),
            ],
        )
        self.assertEqual(plan.cases[0].properties_path, "contracts/base.json")
        self.assertEqual(plan.cases[0].actuator_config_path, "actuators/nominal.json")
        self.assertEqual(plan.cases[0].expected.temporal, "PASS")
        self.assertEqual(plan.cases[0].minimized_counterexample["reduced"], "minimized/final/events.json")

    def test_filters_by_contract_set_scenario_and_fault_combination(self):
        manifest = valid_manifest()

        plan = plan_matrix(
            validate_m3_manifest(manifest),
            contract_sets=("base",),
            scenario_names=("stuck-stop",),
            fault_combinations=("stuck",),
            seeds=(42,),
        )

        self.assertEqual([(case.case_id, case.seed) for case in plan.cases], [("stuck-stop", 42)])
        self.assertEqual(plan.cases[0].expected.contracts["actuator_stop"], "FAIL")

    def test_rejects_missing_seed_expectation_and_unknown_filters(self):
        with self.assertRaises(M3CampaignError):
            plan_matrix(validate_m3_manifest(valid_manifest()), seeds=(2026,))

        with self.assertRaises(M3CampaignError):
            plan_matrix(validate_m3_manifest(valid_manifest()), contract_sets=("missing",))

        with self.assertRaises(M3CampaignError):
            plan_matrix(validate_m3_manifest(valid_manifest()), scenario_names=("missing",))

        with self.assertRaises(M3CampaignError):
            plan_matrix(validate_m3_manifest(valid_manifest()), fault_combinations=("missing",))

    def test_enforces_case_run_and_duplicate_seed_budgets_before_execution(self):
        manifest = validate_m3_manifest(valid_manifest())
        with self.assertRaises(M3CampaignError):
            plan_matrix(manifest, seeds=(42, 42))

        with self.assertRaises(M3CampaignError):
            plan_matrix(manifest, seeds=(42, 1337), max_cases=3)

        with self.assertRaises(M3CampaignError):
            plan_matrix(manifest, seeds=(42, 1337), max_runs=11)

        with self.assertRaises(M3CampaignError):
            plan_matrix(manifest, max_artifact_bytes=1)

        with self.assertRaises(M3CampaignError):
            plan_matrix(manifest, time_budget_seconds=0)


class M3ClassificationTests(unittest.TestCase):
    def test_keeps_temporal_safety_and_progress_outcomes_separate(self):
        expected = ExpectedOutcome(
            temporal="FAIL",
            safety="passed",
            progress="incomplete",
            contracts={"actuator_stop": "FAIL", "fallback_response": "PASS"},
        )
        classification = classify_outputs(
            expected,
            {"status": "passed"},
            {"outcome": "incomplete"},
            {
                "contract_results": [
                    {"contract_id": "actuator_stop", "outcome": "FAIL"},
                    {"contract_id": "fallback_response", "outcome": "PASS"},
                ]
            },
        )

        self.assertEqual(classification.temporal, "FAIL")
        self.assertEqual(classification.safety, "passed")
        self.assertEqual(classification.progress, "incomplete")
        self.assertEqual(classification.errors, ())

    def test_reports_each_expectation_mismatch_without_collapsing_the_outcomes(self):
        classification = classify_outputs(
            ExpectedOutcome(
                temporal="PASS",
                safety="invariant_failed",
                progress="completed",
                contracts={"actuator_stop": "PASS"},
            ),
            {"status": "passed"},
            {"outcome": "incomplete"},
            {"contract_results": [{"contract_id": "actuator_stop", "outcome": "INCONCLUSIVE"}]},
        )

        self.assertEqual(classification.temporal, "INCONCLUSIVE")
        self.assertEqual(classification.safety, "passed")
        self.assertEqual(classification.progress, "incomplete")
        self.assertEqual(len(classification.errors), 4)

    def test_rejects_missing_duplicate_or_unrecognized_contract_results(self):
        expected = ExpectedOutcome("PASS", "passed", "completed", {"actuator_stop": "PASS"})
        for verification in (
            {},
            {"contract_results": [{"contract_id": "actuator_stop", "outcome": "PASS"}] * 2},
            {"contract_results": [{"contract_id": "actuator_stop", "outcome": "UNKNOWN"}]},
        ):
            with self.subTest(verification=verification), self.assertRaises(M3CampaignError):
                classify_outputs(
                    expected,
                    {"status": "passed"},
                    {"outcome": "completed"},
                    verification,
                )


class M3FingerprintTests(unittest.TestCase):
    def test_fingerprint_identity_covers_contract_set_fault_combo_and_m3_artifacts(self):
        fingerprints = seed42_outputs(
            {
                "outputs": [
                    {
                        "suite": "m3",
                        "scenario": "ignored-stop",
                        "variant": "stop-response+continued-stop",
                        "seed": 42,
                        "scenario_sha256": "s" * 64,
                        "properties_sha256": "p" * 64,
                        "actuator_config_sha256": "a" * 64,
                        "events_sha256": "e" * 64,
                        "report_sha256": "r" * 64,
                        "mission_sha256": "m" * 64,
                        "verification_sha256": "v" * 64,
                    }
                ]
            }
        )

        self.assertEqual(
            list(fingerprints), ["m3:ignored-stop:stop-response+continued-stop"]
        )
        self.assertEqual(
            set(fingerprints["m3:ignored-stop:stop-response+continued-stop"]),
            {
                "scenario_sha256",
                "properties_sha256",
                "actuator_config_sha256",
                "events_sha256",
                "report_sha256",
                "mission_sha256",
                "verification_sha256",
            },
        )

    def test_rejects_incomplete_m3_artifact_fingerprints(self):
        with self.assertRaises(CampaignError):
            seed42_outputs(
                {
                    "outputs": [
                        {
                            "suite": "m3",
                            "scenario": "ignored-stop",
                            "variant": "stop-response+continued-stop",
                            "seed": 42,
                            "events_sha256": "e" * 64,
                        }
                    ]
                }
            )

    def test_merges_separately_budgeted_m1_m2_and_m3_platform_fingerprints(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as temporary:
            root = Path(temporary)
            legacy = root / "linux-campaign" / "legacy" / "fingerprints.json"
            m3 = root / "linux-campaign" / "m3" / "fingerprints.json"
            legacy.parent.mkdir(parents=True)
            m3.parent.mkdir(parents=True)
            legacy.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "suite": "all",
                        "suites": ["m1", "m2"],
                        "variants": ["standard"],
                        "m2_scenarios": ["case-a"],
                        "fixture_checksums_sha256": {"m1/a.json": "a", "m2/b.json": "b"},
                        "outputs": [
                            {
                                "suite": "m1",
                                "scenario": "a",
                                "variant": "default",
                                "seed": 42,
                                "events_sha256": "1",
                                "report_sha256": "2",
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            m3.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "suite": "m3",
                        "suites": ["m3"],
                        "m3_scenarios": ["case-b"],
                        "m3_contract_sets": ["contracts"],
                        "m3_fault_combinations": ["fault"],
                        "fixture_checksums_sha256": {"m3/b.json": "c"},
                        "outputs": [
                            {
                                "suite": "m3",
                                "scenario": "case-b",
                                "variant": "contracts+fault",
                                "seed": 42,
                                "events_sha256": "3",
                                "verification_sha256": "4",
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )

            merged = merge_fingerprints([legacy, m3])

            self.assertEqual(merged["suites"], ["m1", "m2", "m3"])
            self.assertEqual(
                merged["fixture_checksums_sha256"],
                {"m1/a.json": "a", "m2/b.json": "b", "m3/b.json": "c"},
            )
            self.assertEqual(
                {entry["suite"] for entry in merged["outputs"]}, {"m1", "m3"}
            )
            self.assertEqual(merged["m3_scenarios"], ["case-b"])


@unittest.skipUnless(M3_BINARY, "build vv-lab with the M3 CLI before running integration tests")
class M3CampaignCliTests(unittest.TestCase):
    def test_nominal_and_real_actuator_failure_runs_replay_and_remain_classified_separately(self):
        binary = M3_BINARY
        assert binary is not None
        with tempfile.TemporaryDirectory(dir=ROOT) as temporary:
            output_root = Path(temporary) / "campaign"
            process = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "scripts" / "run_campaign.py"),
                    "--suite",
                    "m3",
                    "--binary",
                    str(binary),
                    "--seeds",
                    "42",
                    "--m3-scenarios",
                    "nominal-control,ignored-stop",
                    "--output-root",
                    str(output_root),
                ],
                cwd=ROOT,
                capture_output=True,
                text=True,
                timeout=45,
                check=False,
            )
            self.assertEqual(process.returncode, 0, process.stdout + process.stderr)
            summaries = list(output_root.rglob("summary.json"))
            self.assertEqual(len(summaries), 1)
            summary = json.loads(summaries[0].read_text(encoding="utf-8"))
            self.assertEqual(summary["status"], "passed")
            self.assertEqual(summary["execution_counts"]["simulator_invocations"], 6)
            by_id = {case["case_id"]: case for case in summary["cases"]}
            self.assertEqual(by_id["nominal-control"]["actual"]["temporal"], "PASS")
            self.assertTrue(by_id["nominal-control"]["deterministic"])
            self.assertEqual(by_id["nominal-control"]["replay_exit_code"], 0)
            self.assertGreaterEqual(by_id["nominal-control"]["replay_elapsed_seconds"], 0.0)
            self.assertEqual(by_id["ignored-stop"]["actual"]["temporal"], "FAIL")
            self.assertEqual(by_id["ignored-stop"]["actual"]["safety"], "passed")
            self.assertEqual(by_id["ignored-stop"]["actual"]["progress"], "completed")
            self.assertTrue(by_id["ignored-stop"]["deterministic"])
            self.assertEqual(by_id["ignored-stop"]["replay_exit_code"], 0)
            self.assertAlmostEqual(
                summary["replay_elapsed_seconds"],
                sum(case["replay_elapsed_seconds"] for case in summary["cases"]),
            )


if __name__ == "__main__":
    unittest.main()
