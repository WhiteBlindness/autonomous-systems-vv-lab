import hashlib
import json
import math
import os
import sys
from argparse import ArgumentTypeError
from pathlib import Path
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

from campaign_matrix import planned_matrix_counts
from campaign_report import build_summary, write_campaign_outputs
from m2_campaign import (
    M2CampaignError,
    fixture_checksums,
    load_m2_manifest,
    matched_geometry_errors,
    observation_discrepancy,
    scenario_for_variant,
    select_variant,
)
from check_platform_artifacts import (
    CampaignError as PlatformCampaignError,
    find_minimized_outputs,
    seed42_outputs,
)
from process_limits import (
    CampaignArtifactBudget,
    ProcessLimitError,
    run_bounded_process,
    tree_size_bytes,
)
from run_campaign import (
    execute_m2_case,
    parse_positive_bytes,
    parse_positive_int,
    parse_time_budget,
)


class M2ManifestTests(unittest.TestCase):
    def setUp(self):
        self.manifest = load_m2_manifest()
        self.by_name = {entry["name"]: entry for entry in self.manifest["scenarios"]}

    def test_manifest_pins_fixture_checksums_and_explicit_seed_expectations(self):
        checksums = fixture_checksums(self.manifest)
        self.assertEqual(len(checksums), len(self.manifest["scenarios"]) + 1)
        for entry in self.manifest["scenarios"]:
            for variant in entry["variants"].values():
                self.assertEqual(
                    set(variant["expected_outcomes"]), {"42", "1337", "2026"}
                )

    def test_fault_stress_is_a_bounded_named_sensor_override(self):
        entry = self.by_name["b-noise-recovery-loss"]
        policy, outcome, overrides = select_variant(entry, "fault-stress", 42)
        self.assertEqual(policy, {"stall_window_ticks": 8, "min_progress_mm": 1})
        self.assertEqual(outcome, "stalled")
        self.assertEqual(
            overrides, {"sensors.communication.packet_loss_permille": 150}
        )
        variant = scenario_for_variant(entry, overrides)
        self.assertEqual(
            variant["sensors"]["communication"]["packet_loss_permille"], 150
        )
        with self.assertRaises(M2CampaignError):
            scenario_for_variant(entry, {"vehicle.step_mm": 999})

    def test_matched_natural_faults_keep_the_same_geometry(self):
        self.assertEqual(
            matched_geometry_errors(self.by_name["g-zone-fault"], self.by_name), []
        )
        self.assertEqual(
            matched_geometry_errors(self.by_name["i-bounds-fault"], self.by_name), []
        )

    def test_observation_check_uses_the_sample_tick_truth(self):
        report = {
            "invariants": [
                {
                    "name": "restricted_zone",
                    "failures": [{"observed": {"sample_tick": 4}}],
                }
            ]
        }
        artifact = {
            "events": [
                {
                    "tick": 4,
                    "event": {
                        "kind": "gps_sample",
                        "truth_at_sample": {"x_mm": 2000, "y_mm": 1000},
                        "observation": {"x_mm": 2000, "y_mm": 1000},
                    },
                },
                {
                    "tick": 4,
                    "event": {
                        "kind": "tick_result",
                        "truth_from": {"x_mm": 2000, "y_mm": 1000},
                        "truth_position": {"x_mm": 2250, "y_mm": 1000},
                    },
                },
            ]
        }
        self.assertFalse(observation_discrepancy(report, artifact, "restricted_zone"))
        artifact["events"][0]["event"]["observation"] = {
            "x_mm": 2600,
            "y_mm": 1000,
        }
        self.assertTrue(observation_discrepancy(report, artifact, "restricted_zone"))

    def test_three_seed_default_matrix_plans_87_cases_and_324_commands(self):
        m1, m2, commands = planned_matrix_counts(
            entries=self.manifest["scenarios"],
            variants=("standard", "short-stall", "fault-stress"),
            seeds=(42, 1337, 2026),
            include_m1=True,
            include_m2=True,
        )
        self.assertEqual((m1, m2, m1 + m2, commands), (24, 63, 87, 324))

    def test_platform_fingerprints_include_m2_scenario_and_mission_hashes(self):
        hashes = {
            "scenario_sha256": "a" * 64,
            "events_sha256": "b" * 64,
            "report_sha256": "c" * 64,
            "mission_sha256": "d" * 64,
        }
        fingerprints = {
            "outputs": [
                {
                    "suite": "m2",
                    "scenario": "g-zone-fault",
                    "variant": "standard",
                    "seed": 42,
                    **hashes,
                }
            ]
        }
        self.assertEqual(
            seed42_outputs(fingerprints), {"m2:g-zone-fault:standard": hashes}
        )
        del fingerprints["outputs"][0]["mission_sha256"]
        with self.assertRaises(PlatformCampaignError):
            seed42_outputs(fingerprints)

    def test_minimized_platform_artifact_set_includes_scenario_and_mission(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as temporary:
            root = Path(temporary)
            output = root / "Linux-campaign" / "runs" / "ci-minimized-Linux"
            scenarios = output / "scenarios"
            final = output / "final"
            scenarios.mkdir(parents=True)
            final.mkdir()
            files = {
                scenarios / "minimized.json": b"scenario",
                final / "events.json": b"events",
                final / "report.json": b"report",
                final / "mission.json": b"mission",
            }
            for path, content in files.items():
                path.write_bytes(content)
            expected = {
                "scenario_sha256": hashlib.sha256(b"scenario").hexdigest(),
                "events_sha256": hashlib.sha256(b"events").hexdigest(),
                "report_sha256": hashlib.sha256(b"report").hexdigest(),
                "mission_sha256": hashlib.sha256(b"mission").hexdigest(),
            }
            self.assertEqual(find_minimized_outputs(root, "linux-campaign"), expected)


class ProcessBoundTests(unittest.TestCase):
    def test_time_and_integer_limits_reject_nonfinite_or_excessive_values(self):
        for value in ("nan", "inf", "-inf", "0", "3601"):
            with self.subTest(value=value), self.assertRaises(ArgumentTypeError):
                parse_time_budget(value)
        for value in ("0", "10001", "-1"):
            with self.subTest(value=value), self.assertRaises(ArgumentTypeError):
                parse_positive_int(value)
        for value in ("0", str(2 * 1024 * 1024 * 1024 + 1)):
            with self.subTest(value=value), self.assertRaises(ArgumentTypeError):
                parse_positive_bytes(value)

    def test_process_runner_rejects_invalid_timeout_and_output_limits(self):
        command = [sys.executable, "-c", "print('ok')"]
        for timeout in (float("nan"), math.inf, -math.inf, 0):
            with self.subTest(timeout=timeout), self.assertRaises(ProcessLimitError):
                run_bounded_process(
                    command,
                    cwd=ROOT,
                    timeout_seconds=timeout,
                    max_output_bytes=100,
                    description="test process",
                )
        with self.assertRaises(ProcessLimitError):
            run_bounded_process(
                command,
                cwd=ROOT,
                timeout_seconds=1,
                max_output_bytes=0,
                description="test process",
            )

    def test_process_runner_enforces_time_and_combined_output(self):
        with self.assertRaises(ProcessLimitError):
            run_bounded_process(
                [sys.executable, "-c", "import time; time.sleep(2)"],
                cwd=ROOT,
                timeout_seconds=0.05,
                max_output_bytes=100,
                description="slow process",
            )
        with self.assertRaises(ProcessLimitError):
            run_bounded_process(
                [sys.executable, "-c", "print('x' * 2000)"],
                cwd=ROOT,
                timeout_seconds=2,
                max_output_bytes=100,
                description="loud process",
            )

    def test_campaign_byte_budget_tracks_case_deltas_and_rejects_escape(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as temporary:
            root = Path(temporary) / "campaign"
            case = root / "scenario" / "seed-42"
            root.mkdir()
            budget = CampaignArtifactBudget(root, 12)
            case.mkdir(parents=True)
            self.assertEqual(budget.remaining(case), 12)
            file_path = case / "events.json"
            file_path.write_bytes(b"12345678")
            self.assertEqual(budget.remaining(case), 4)
            file_path.write_bytes(b"1234567890123")
            with self.assertRaises(ProcessLimitError):
                budget.remaining(case)
            self.assertEqual(tree_size_bytes(root), 13)
            with self.assertRaises(ProcessLimitError):
                budget.refresh(Path(temporary) / "outside")

    def test_report_byte_count_matches_utf8_files_and_unexpected_outcomes(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as temporary:
            campaign = Path(temporary) / "campaign"
            campaign.mkdir()
            (campaign / "case.txt").write_bytes("missão ✓\n".encode("utf-8"))
            command_counts = {
                name: {"attempted": 0, "completed": 0}
                for name in ("run", "replay", "analyze")
            }
            summary = build_summary(
                suite="all",
                cases=[
                    {
                        "suite": "m1",
                        "scenario": "basic-mission",
                        "seed": 42,
                        "result": "passed",
                        "expected_outcome": None,
                        "mission_metrics": {"outcome": "completed"},
                        "invariants": {},
                        "expected_invariants": {},
                    },
                    {
                        "suite": "m2",
                        "scenario": "fixture",
                        "seed": 42,
                        "result": "passed",
                        "expected_outcome": "completed",
                        "mission_metrics": {"outcome": "incomplete"},
                        "invariants": {},
                        "expected_invariants": {},
                    },
                ],
                command_counts=command_counts,
                campaign_started=time.monotonic(),
                campaign_directory="runs/campaign-test",
                checksums={},
                seeds=(42,),
                variants=("standard",),
                binary_name="vv-lab",
                max_artifact_bytes=100_000,
                max_runs=10,
                max_cases=2,
                planned_cases=2,
                time_budget_seconds=5,
            )
            write_campaign_outputs(campaign, {"schema_version": 1, "outputs": []}, summary, 100_000)
            self.assertEqual(summary["retained_artifact_bytes"], tree_size_bytes(campaign))
            self.assertEqual(summary["mission_outcome_counts"]["observed"], 2)
            self.assertEqual(summary["mission_outcome_counts"]["expected_checked"], 1)
            self.assertEqual(summary["mission_outcome_counts"]["unexpected"], 1)


class M2CliIntegrationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        configured = os.environ.get("VV_LAB_BINARY")
        candidates = [Path(configured)] if configured else []
        candidates.extend((ROOT / "target/release/vv-lab.exe", ROOT / "target/debug/vv-lab.exe"))
        if os.name != "nt":
            candidates.extend((ROOT / "target/release/vv-lab", ROOT / "target/debug/vv-lab"))
        cls.binary = next((path.resolve() for path in candidates if path.is_file()), None)
        if cls.binary is None:
            raise unittest.SkipTest("build vv-lab before running M2 CLI integration tests")

    def run_case(self, scenario_name, variant, seed):
        manifest = load_m2_manifest()
        entry = next(row for row in manifest["scenarios"] if row["name"] == scenario_name)
        policy, expected_outcome, overrides = select_variant(entry, variant, seed)
        temporary = tempfile.TemporaryDirectory(dir=ROOT)
        self.addCleanup(temporary.cleanup)
        campaign_dir = Path(temporary.name) / "campaign"
        campaign_dir.mkdir()
        budget = CampaignArtifactBudget(campaign_dir, 16 * 1024 * 1024)
        command_counts = {
            name: {"attempted": 0, "completed": 0}
            for name in ("run", "replay", "analyze")
        }
        result = execute_m2_case(
            self.binary,
            campaign_dir,
            entry,
            variant,
            seed,
            policy,
            expected_outcome,
            overrides,
            deadline=time.monotonic() + 30,
            artifact_budget=budget,
            command_counts=command_counts,
        )
        self.assertEqual(result["result"], "passed", result["errors"])
        self.assertTrue(result["deterministic"])
        self.assertTrue(result["analysis_matches_run"])
        self.assertEqual(command_counts["run"]["completed"], 2)
        self.assertEqual(command_counts["replay"]["completed"], 1)
        self.assertEqual(command_counts["analyze"]["completed"], 1)
        return campaign_dir, result

    def test_natural_sensor_fault_has_sampled_truth_discrepancy_and_replays(self):
        campaign_dir, result = self.run_case("g-zone-fault", "standard", 42)
        self.assertEqual(result["mission_metrics"]["outcome"], "incomplete")
        self.assertEqual(result["expected_invariants"]["restricted_zone"], False)
        self.assertTrue(result["invariant_failure_evidence"]["restricted_zone"])
        scenario = json.loads(
            (campaign_dir / result["output_paths"]["scenario"]).read_text(encoding="utf-8")
        )
        self.assertEqual(scenario["validation_mutants"], [])
        self.assertGreater(tree_size_bytes(campaign_dir), 0)

    def test_noise_recovery_requires_fresh_confidence_above_threshold(self):
        campaign_dir, result = self.run_case("b-noise-recovery-loss", "standard", 42)
        events_path = campaign_dir / result["output_paths"]["events"]
        artifact = json.loads(events_path.read_text(encoding="utf-8"))
        transitions = [
            (row["tick"], row["event"].get("reason"))
            for row in artifact["events"]
            if row["event"].get("kind") == "transition"
            and row["event"].get("subsystem") == "safety"
        ]
        self.assertIn((23, "confidence_recovered_for_configured_interval"), transitions)
        below_threshold_held = [
            row["event"]
            for row in artifact["events"]
            if row["event"].get("kind") == "tick_result"
            and row["event"].get("safety_state") == "fallback"
            and isinstance(row["event"].get("observed"), dict)
            and row["event"]["observed"].get("fresh") is True
            and row["event"]["observed"].get("confidence_permille", 1000) < 500
        ]
        self.assertTrue(below_threshold_held)
        self.assertTrue(
            all(event["action"].get("heading") is None for event in below_threshold_held)
        )
        aged_hold_samples = {
            (row["tick"], row["event"]["observed"]["sample_tick"])
            for row in artifact["events"]
            if row["event"].get("kind") == "tick_result"
            and row["event"].get("safety_state") == "fallback"
            and isinstance(row["event"].get("observed"), dict)
            and row["event"]["observed"].get("fresh") is True
            and row["event"]["observed"].get("age_ticks", 0) > 0
            and row["event"]["observed"].get("confidence_permille", 1000) < 500
        }
        accepted_aged_delivery = any(
            row["event"].get("kind") == "sensor_delivery"
            and row["event"].get("accepted") is True
            and (row["tick"], row["event"].get("sample_tick")) in aged_hold_samples
            and row["event"].get("sample_tick") < row["tick"]
            for row in artifact["events"]
        )
        self.assertTrue(accepted_aged_delivery)

    def test_intermittent_fault_recovery_repeats_for_selected_seed(self):
        campaign_dir, result = self.run_case("c-intermittent", "standard", 42)
        self.assertEqual(result["mission_metrics"]["outcome"], "completed")
        events_path = campaign_dir / result["output_paths"]["events"]
        artifact = json.loads(events_path.read_text(encoding="utf-8"))
        safety_transitions = [
            row["event"].get("reason")
            for row in artifact["events"]
            if row["event"].get("kind") == "transition"
            and row["event"].get("subsystem") == "safety"
        ]
        recoveries = safety_transitions.count("confidence_recovered_for_configured_interval")
        entries = safety_transitions.count("low_confidence_watchdog")
        self.assertGreaterEqual(recoveries, 2)
        self.assertGreaterEqual(entries, 2)


if __name__ == "__main__":
    unittest.main()
