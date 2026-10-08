from __future__ import annotations

import contextlib
import argparse
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

import minimize_failure
from failure_reduction import (
    CandidateRejected,
    Evaluation,
    MinimizationError,
    evaluate_scenario,
    invariant_failure_signature,
    propose_horizons,
    same_property,
    target_signature,
)
from process_limits import ProcessLimitError, run_bounded_process
from minimize_failure import write_results


def find_binary() -> Path:
    configured = os.environ.get("VV_LAB_BINARY")
    if configured:
        return Path(configured).resolve()
    names = ("vv-lab.exe", "vv-lab") if os.name == "nt" else ("vv-lab", "vv-lab.exe")
    for configuration in ("release", "debug"):
        for name in names:
            candidate = ROOT / "target" / configuration / name
            if candidate.is_file():
                return candidate
    raise AssertionError("o executável vv-lab não existe; compile o pacote antes dos testes")


class MinimizerIntegrationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.binary = find_binary()

    def temporary_directory(self):
        return tempfile.TemporaryDirectory(prefix="minimize-test-", dir=ROOT)

    def invoke(self, scenario: Path, output: Path, *options: str) -> tuple[int, str, str]:
        stdout = io.StringIO()
        stderr = io.StringIO()
        arguments = [
            str(scenario),
            "--seed",
            "42",
            "--output",
            str(output),
            "--binary",
            str(self.binary),
            *options,
        ]
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = minimize_failure.main(arguments)
        return status, stdout.getvalue(), stderr.getvalue()

    def test_natural_zone_failure_reduces_deterministically_and_retains_replayable_artifacts(self):
        scenario = ROOT / "scenarios" / "m2" / "g-zone-fault.json"
        with self.temporary_directory() as temporary:
            root = Path(temporary)
            outputs = [root / "first", root / "second"]
            summaries = []
            for output in outputs:
                status, _stdout, stderr = self.invoke(
                    scenario,
                    output,
                    "--property",
                    "invariant:restricted_zone",
                    "--max-candidates",
                    "1",
                    "--time-budget-seconds",
                    "120",
                )
                self.assertEqual(status, 0, stderr)
                summary = json.loads((output / "summary.json").read_text(encoding="utf-8"))
                summaries.append(summary)
                self.assertEqual(summary["status"], "candidate_limit_reached")
                self.assertEqual(summary["candidate_counts"]["attempted"], 1)
                self.assertEqual(summary["candidate_counts"]["accepted"], 1)
                self.assertEqual(
                    summary["reproducible_commands_working_directory"],
                    "minimization output directory",
                )
                self.assertTrue(
                    all("<" not in command and ">" not in command for command in summary["reproducible_commands"])
                )
                self.assertIn("scenarios/original.json", summary["reproducible_commands"][0])
                self.assertIn("scenarios/minimized.json", summary["reproducible_commands"][3])
                self.assertLess(
                    summary["complexity"]["final"]["score"],
                    summary["complexity"]["original"]["score"],
                )
                minimized = json.loads(
                    (output / "scenarios" / "minimized.json").read_text(encoding="utf-8")
                )
                self.assertEqual(minimized["sensors"]["gps"]["noise_max_mm"], 250)
                original_mission = json.loads(
                    (output / "original" / "mission.json").read_text(encoding="utf-8")
                )
                final_mission = json.loads(
                    (output / "final" / "mission.json").read_text(encoding="utf-8")
                )
                self.assertEqual(original_mission["outcome"], final_mission["outcome"])
                self.assertEqual(original_mission["reason"], final_mission["reason"])
                for label, mission in (("original", original_mission), ("final", final_mission)):
                    artifact = json.loads(
                        (output / label / "events.json").read_text(encoding="utf-8")
                    )
                    self.assertEqual(mission["source_final_hash"], artifact["final_hash"])
                for label in ("original", "final"):
                    replay = run_bounded_process(
                        [str(self.binary), "replay", str(output / label / "events.json")],
                        cwd=ROOT,
                        timeout_seconds=30,
                        max_output_bytes=64 * 1024,
                        description="reprodução de teste",
                    )
                    self.assertEqual(replay.returncode, 0, replay.stderr)
            for name in ("events.json", "report.json", "mission.json"):
                self.assertEqual(
                    (outputs[0] / "final" / name).read_bytes(),
                    (outputs[1] / "final" / name).read_bytes(),
                )
            self.assertEqual(summaries[0]["final_signature"], summaries[1]["final_signature"])

    def test_initial_inside_failure_with_no_fault_knobs_has_explicit_no_reduction(self):
        source = json.loads(
            (ROOT / "scenarios" / "m2" / "g-zone-fault.json").read_text(encoding="utf-8")
        )
        source["steps"] = 1
        source["sensors"]["gps"]["noise_max_mm"] = 0
        source["restricted_zone"]["vertices"] = [
            {"x_mm": 750, "y_mm": 1750},
            {"x_mm": 1250, "y_mm": 1750},
            {"x_mm": 1250, "y_mm": 2250},
            {"x_mm": 750, "y_mm": 2250},
        ]
        with self.temporary_directory() as temporary:
            root = Path(temporary)
            scenario = root / "inside.json"
            scenario.write_bytes(minimize_failure.compact_json(source))
            output = root / "result"
            status, _stdout, stderr = self.invoke(
                scenario,
                output,
                "--property",
                "invariant:restricted_zone",
                "--time-budget-seconds",
                "120",
            )
            self.assertEqual(status, 0, stderr)
            summary = json.loads((output / "summary.json").read_text(encoding="utf-8"))
            self.assertEqual(summary["status"], "no_reduction")
            self.assertEqual(summary["candidate_counts"]["attempted"], 0)
            self.assertEqual(summary["final_signature"]["evidence"]["condition"], "started_inside_or_on_zone")

    def test_safe_held_stall_reduction_keeps_fault_hold_reason_and_progress_policy(self):
        scenario = ROOT / "scenarios" / "m2" / "d-safe-held-stall.json"
        with self.temporary_directory() as temporary:
            output = Path(temporary) / "result"
            status, _stdout, stderr = self.invoke(
                scenario,
                output,
                "--property",
                "mission:stalled",
                "--max-candidates",
                "3",
                "--time-budget-seconds",
                "120",
            )
            self.assertEqual(status, 0, stderr)
            summary = json.loads((output / "summary.json").read_text(encoding="utf-8"))
            self.assertEqual(summary["property"], "mission:stalled")
            self.assertEqual(summary["candidate_counts"]["attempted"], 3)
            self.assertEqual(summary["candidate_counts"]["accepted"], 1)
            signature = summary["final_signature"]
            self.assertEqual(signature["outcome"], "stalled")
            self.assertEqual(signature["reason"], "expected_fault_hold")
            self.assertEqual(signature["policy"]["stall_window_ticks"], 8)
            self.assertEqual(signature["criteria"]["waypoints_remaining"], 1)
            self.assertEqual(signature["failed_invariants"], [])

    def test_safe_nominal_input_is_rejected_without_creating_output(self):
        scenario = ROOT / "scenarios" / "m2" / "f-zone-nominal.json"
        with self.temporary_directory() as temporary:
            output = Path(temporary) / "result"
            status, _stdout, stderr = self.invoke(
                scenario, output, "--time-budget-seconds", "120"
            )
            self.assertEqual(status, 1)
            self.assertIn("não apresenta uma falha", stderr)
            self.assertFalse(output.exists())

    def test_temporary_and_artifact_limits_fail_before_retaining_results(self):
        scenario, _raw = minimize_failure.load_scenario(
            ROOT / "scenarios" / "m2" / "g-zone-fault.json"
        )
        with self.assertRaises(CandidateRejected):
            evaluate_scenario(
                scenario,
                binary=self.binary,
                seed=42,
                stall_window_ticks=8,
                min_progress_mm=1,
                max_output_bytes=16 * 1024 * 1024,
                max_temp_bytes=1,
                deadline=10**12,
            )
        with self.temporary_directory() as temporary:
            output = Path(temporary) / "result"
            status, _stdout, _stderr = self.invoke(
                ROOT / "scenarios" / "m2" / "g-zone-fault.json",
                output,
                "--max-output-bytes",
                "1",
                "--time-budget-seconds",
                "120",
            )
            self.assertEqual(status, 1)
            self.assertFalse(output.exists())

    def test_time_parser_rejects_nonfinite_values_and_deadline_prevents_a_process(self):
        self.assertEqual(minimize_failure.parse_seed("0"), 0)
        self.assertEqual(minimize_failure.parse_seed(str(2**64 - 1)), 2**64 - 1)
        for seed in ("-1", "+1", str(2**64), "x", "0" * 21):
            with self.subTest(seed=seed), self.assertRaises(argparse.ArgumentTypeError):
                minimize_failure.parse_seed(seed)
        for value in ("nan", "inf", "-inf", "0", "-1"):
            with self.subTest(value=value), self.assertRaises(SystemExit):
                with contextlib.redirect_stderr(io.StringIO()):
                    minimize_failure.parser().parse_args(
                        [
                            "scenario.json",
                            "--seed",
                            "42",
                            "--output",
                            "out",
                            f"--time-budget-seconds={value}",
                        ]
                    )
        with self.assertRaises(MinimizationError):
            evaluate_scenario(
                json.loads((ROOT / "scenarios" / "m2" / "g-zone-fault.json").read_text(encoding="utf-8")),
                binary=self.binary,
                seed=42,
                stall_window_ticks=8,
                min_progress_mm=1,
                max_output_bytes=16 * 1024 * 1024,
                max_temp_bytes=32 * 1024 * 1024,
                deadline=0,
            )

    def test_retained_output_limit_is_checked_before_copying_any_artifact(self):
        evaluation = Evaluation({}, {}, {}, {name: b"{}" for name in ("events.json", "report.json", "mission.json")})
        summary = {
            "status": "no_reduction",
            "property": "invariant:restricted_zone",
            "seed": 42,
            "candidate_counts": {
                "attempted": 0,
                "accepted": 0,
                "rejected_property_mismatch": 0,
                "rejected_invalid_or_unreproducible": 0,
            },
            "complexity": {"original": {"score": 1}, "final": {"score": 1}, "reduction_ratio": 0.0},
            "elapsed_seconds": 0.1,
            "time_budget_seconds": 30.0,
            "final_signature": {},
            "reduction_steps": [],
            "reproducible_commands": [],
        }
        with self.temporary_directory() as temporary:
            output = Path(temporary) / "limited"
            with self.assertRaisesRegex(MinimizationError, "artefactos retidos"):
                write_results(
                    output,
                    original_input=b"{}",
                    minimized_input=b"{}",
                    original=evaluation,
                    final=evaluation,
                    summary=summary,
                    max_output_bytes=1,
                )
            self.assertFalse(output.exists())

    def test_subprocess_output_limit_is_enforced(self):
        with self.assertRaisesRegex(ProcessLimitError, "output limit"):
            run_bounded_process(
                [sys.executable, "-c", "print('x' * 10000)"],
                cwd=ROOT,
                timeout_seconds=5,
                max_output_bytes=8,
                description="processo de teste",
            )


class FailureSignatureTests(unittest.TestCase):
    def make_evaluation(self, *, sample_noisy: bool, edge_to: dict[str, int]) -> Evaluation:
        zone = [
            {"x_mm": 0, "y_mm": 0},
            {"x_mm": 10, "y_mm": 0},
            {"x_mm": 10, "y_mm": 10},
            {"x_mm": 0, "y_mm": 10},
        ]
        sample_truth = {"x_mm": -2, "y_mm": 5}
        sample_observation = {"x_mm": -1 if sample_noisy else -2, "y_mm": 5}
        artifact = {
            "scenario": {
                "restricted_zone": {"vertices": zone},
                "bounds": {"min_x_mm": -100, "max_x_mm": 100, "min_y_mm": -100, "max_y_mm": 100},
            },
            "events": [
                {
                    "sequence": 1,
                    "tick": 1,
                    "event": {
                        "kind": "gps_sample",
                        "packet_sequence": 1,
                        "truth_at_sample": sample_truth,
                        "observation": sample_observation,
                        "dropped": False,
                    },
                },
                {
                    "sequence": 2,
                    "tick": 1,
                    "event": {"kind": "packet_outcome", "packet_sequence": 1, "delivered": True},
                },
                {
                    "sequence": 3,
                    "tick": 1,
                    "event": {
                        "kind": "tick_result",
                        "truth_from": {"x_mm": -2, "y_mm": 5},
                        "truth_position": edge_to,
                    },
                },
            ],
        }
        evidence = {
            "tick": 1,
            "trigger_event_sequence": 3,
            "trigger_event_kind": "tick_result",
            "expected": "truth position and swept segment outside restricted polygon",
            "observed": {
                "position": sample_observation,
                "sample_tick": 1,
                "age_ticks": 0,
            },
        }
        report = {
            "invariants": [
                {"name": name, "passed": name != "restricted_zone", "failures": [evidence] if name == "restricted_zone" else []}
                for name in (
                    "restricted_zone",
                    "world_bounds",
                    "safe_fallback",
                    "valid_state_transitions",
                )
            ]
        }
        mission = {
            "outcome": "completed",
            "reason": None,
            "policy": {"stall_window_ticks": 8, "min_progress_mm": 1},
            "waypoint_count": 1,
            "waypoints_completed": 1,
            "waypoints_remaining": 0,
            "controller_waypoint_index": 1,
            "completion_tick": 1,
            "stall_detected_tick": None,
            "ticks_since_meaningful_progress": 0,
            "last_meaningful_progress_tick": 1,
            "hold_reason_counts": {"low_confidence": 0, "fallback": 0, "other": 0},
        }
        return Evaluation(artifact, report, mission, {})

    def add_between_sample_faults(
        self, evaluation: Evaluation, *, gps_dropout: bool, packet_loss: bool
    ) -> None:
        violation = evaluation.artifact["events"][-1]
        violation.update({"sequence": 7, "tick": 3})
        evidence = evaluation.report["invariants"][0]["failures"][0]
        evidence.update(
            {
                "tick": 3,
                "trigger_event_sequence": 7,
                "observed": {
                    "position": {"x_mm": -2, "y_mm": 5},
                    "sample_tick": 1,
                    "age_ticks": 2,
                },
            }
        )
        later_events = []
        for tick in (2, 3):
            later_events.extend(
                [
                    {
                        "sequence": len(later_events) + 3,
                        "tick": tick,
                        "event": {
                            "kind": "gps_sample",
                            "packet_sequence": tick,
                            "truth_at_sample": {"x_mm": -2, "y_mm": 5},
                            "observation": None
                            if gps_dropout
                            else {"x_mm": -2, "y_mm": 5},
                            "dropped": gps_dropout,
                        },
                    },
                    {
                        "sequence": len(later_events) + 4,
                        "tick": tick,
                        "event": {
                            "kind": "packet_outcome",
                            "packet_sequence": tick,
                            "delivered": not packet_loss,
                        },
                    },
                ]
            )
        evaluation.artifact["events"] = [
            *evaluation.artifact["events"][:2],
            *later_events,
            violation,
        ]

    def make_stall_evaluation(self, fault_rows: list[dict[str, bool]]) -> Evaluation:
        evaluation = self.make_evaluation(
            sample_noisy=False, edge_to={"x_mm": 10, "y_mm": 5}
        )
        events = []
        for tick, causes in enumerate(fault_rows, start=2):
            truth = {"x_mm": -2, "y_mm": 5}
            dropped = causes["gps_dropout"]
            observation = None if dropped else {
                "x_mm": -1 if causes["noise"] else -2,
                "y_mm": 5,
            }
            delivered = not causes["packet_loss"]
            delay = 1 if causes["delay"] else 0
            events.extend(
                [
                    {
                        "sequence": len(events) + 1,
                        "tick": tick,
                        "event": {
                            "kind": "gps_sample",
                            "packet_sequence": tick,
                            "truth_at_sample": truth,
                            "observation": observation,
                            "dropped": dropped,
                        },
                    },
                    {
                        "sequence": len(events) + 2,
                        "tick": tick,
                        "event": {
                            "kind": "packet_outcome",
                            "packet_sequence": tick,
                            "delivered": delivered,
                            "delay_ticks": delay,
                        },
                    },
                    {
                        "sequence": len(events) + 3,
                        "tick": tick,
                        "event": {
                            "kind": "tick_result",
                            "observed": {
                                "age_ticks": delay,
                                "fresh": delay == 0,
                                "confidence_permille": 1000 if delay == 0 else 500,
                            },
                        },
                    },
                ]
            )
        evaluation.artifact["events"] = events
        evaluation.report = {
            "ticks": 4,
            "invariants": [
                {
                    "name": name,
                    "passed": True,
                    "failures": [],
                }
                for name in (
                    "restricted_zone",
                    "world_bounds",
                    "safe_fallback",
                    "valid_state_transitions",
                )
            ],
        }
        evaluation.mission.update(
            {
                "outcome": "stalled",
                "reason": "expected_fault_hold",
                "policy": {"stall_window_ticks": 3, "min_progress_mm": 1},
                "waypoints_completed": 0,
                "waypoints_remaining": 1,
                "controller_waypoint_index": 0,
                "stall_detected_tick": 4,
                "ticks_since_meaningful_progress": 3,
                "fallback_ticks": 3,
                "hold_reason_counts": {
                    "low_confidence": 3,
                    "fallback": 3,
                    "other": 0,
                },
            }
        )
        return evaluation

    def test_same_invariant_with_changed_gps_cause_is_rejected(self):
        edge = {"x_mm": 10, "y_mm": 5}
        original = self.make_evaluation(sample_noisy=True, edge_to=edge)
        candidate = self.make_evaluation(sample_noisy=False, edge_to=edge)
        signature = target_signature("invariant:restricted_zone", original)
        self.assertNotEqual(
            invariant_failure_signature(
                "restricted_zone",
                original.report["invariants"][0]["failures"][0],
                original.artifact,
            ),
            invariant_failure_signature(
                "restricted_zone",
                candidate.report["invariants"][0]["failures"][0],
                candidate.artifact,
            ),
        )
        self.assertFalse(same_property("invariant:restricted_zone", signature, candidate))

    def test_later_matching_failure_does_not_hide_a_different_first_failure(self):
        edge = {"x_mm": 10, "y_mm": 5}
        original = self.make_evaluation(sample_noisy=True, edge_to=edge)
        candidate = self.make_evaluation(sample_noisy=True, edge_to=edge)
        failures = candidate.report["invariants"][0]["failures"]
        matching_evidence = failures[0]
        failures.insert(0, {**matching_evidence, "expected": "a different causal condition"})
        signature = target_signature("invariant:restricted_zone", original)

        self.assertNotEqual(
            target_signature("invariant:restricted_zone", candidate)["evidence"],
            signature["evidence"],
        )
        self.assertFalse(same_property("invariant:restricted_zone", signature, candidate))

    def test_observation_age_signature_distinguishes_gps_dropout_from_packet_loss(self):
        edge = {"x_mm": 10, "y_mm": 5}
        gps_loss = self.make_evaluation(sample_noisy=True, edge_to=edge)
        communication_loss = self.make_evaluation(sample_noisy=True, edge_to=edge)
        self.add_between_sample_faults(gps_loss, gps_dropout=True, packet_loss=False)
        self.add_between_sample_faults(
            communication_loss, gps_dropout=False, packet_loss=True
        )
        signature = target_signature("invariant:restricted_zone", gps_loss)
        causes = signature["evidence"]["mechanisms"]

        self.assertEqual(causes["gps_dropout_after_sample"], True)
        self.assertEqual(causes["packet_loss_after_sample"], False)
        self.assertFalse(
            same_property("invariant:restricted_zone", signature, communication_loss)
        )

    def test_missing_observation_uses_prior_faults_without_inventing_gps_noise(self):
        edge = {"x_mm": 10, "y_mm": 5}
        gps_loss = self.make_evaluation(sample_noisy=False, edge_to=edge)
        communication_loss = self.make_evaluation(sample_noisy=False, edge_to=edge)
        self.add_between_sample_faults(gps_loss, gps_dropout=True, packet_loss=False)
        self.add_between_sample_faults(
            communication_loss, gps_dropout=False, packet_loss=True
        )
        for evaluation, gps_dropout, packet_loss in (
            (gps_loss, True, False),
            (communication_loss, False, True),
        ):
            first_gps = evaluation.artifact["events"][0]["event"]
            first_gps["dropped"] = gps_dropout
            first_gps["observation"] = None if gps_dropout else first_gps["truth_at_sample"]
            evaluation.artifact["events"][1]["event"]["delivered"] = not packet_loss
            evaluation.report["invariants"][0]["failures"][0]["observed"] = None

        gps_signature = target_signature("invariant:restricted_zone", gps_loss)
        comm_signature = target_signature("invariant:restricted_zone", communication_loss)
        gps_causes = gps_signature["evidence"]["mechanisms"]
        comm_causes = comm_signature["evidence"]["mechanisms"]

        self.assertTrue(gps_causes["missing_observation"])
        self.assertTrue(gps_causes["gps_dropout_after_sample"])
        self.assertFalse(gps_causes["packet_loss_after_sample"])
        self.assertFalse(gps_causes["gps_noise"])
        self.assertFalse(comm_causes["gps_dropout_after_sample"])
        self.assertTrue(comm_causes["packet_loss_after_sample"])
        self.assertFalse(comm_causes["gps_noise"])
        self.assertFalse(
            same_property("invariant:restricted_zone", gps_signature, communication_loss)
        )

    def test_stall_signature_keeps_window_fault_categories_without_timestamps(self):
        common = lambda gps_tick, loss_tick, delay_tick, noise_tick: [
            {
                "gps_dropout": tick == gps_tick,
                "packet_loss": tick == loss_tick,
                "delay": tick == delay_tick,
                "noise": tick == noise_tick,
            }
            for tick in range(3)
        ]
        original = self.make_stall_evaluation(common(0, 1, 2, 2))
        shifted = self.make_stall_evaluation(common(2, 0, 1, 1))
        signature = target_signature("mission:stalled", original)
        categories = signature["criteria"]["stall_window_mechanisms"]

        self.assertEqual(
            categories,
            {
                "gps_dropout": True,
                "packet_loss": True,
                "delayed_sample": True,
                "gps_noise": True,
            },
        )
        self.assertNotIn("ticks", categories)
        self.assertTrue(same_property("mission:stalled", signature, shifted))

        no_dropout = self.make_stall_evaluation(common(-1, 0, 1, 1))
        self.assertFalse(same_property("mission:stalled", signature, no_dropout))

    def test_restricted_zone_keeps_boundary_condition_and_from_to_relation(self):
        original = self.make_evaluation(
            sample_noisy=True, edge_to={"x_mm": 10, "y_mm": 5}
        )
        candidate = self.make_evaluation(
            sample_noisy=True, edge_to={"x_mm": 10, "y_mm": 4}
        )
        original_signature = target_signature("invariant:restricted_zone", original)
        self.assertTrue(
            same_property("invariant:restricted_zone", original_signature, candidate)
        )

    def test_incomplete_mission_does_not_accept_a_shorter_horizon(self):
        scenario = json.loads(
            (ROOT / "scenarios" / "m2" / "e-incomplete-safe.json").read_text(encoding="utf-8")
        )
        evaluation = self.make_evaluation(
            sample_noisy=False, edge_to={"x_mm": 10, "y_mm": 5}
        )
        evaluation.mission.update(
            {
                "outcome": "incomplete",
                "reason": None,
                "waypoint_count": 1,
                "waypoints_completed": 0,
                "waypoints_remaining": 1,
                "controller_waypoint_index": 0,
                "completion_tick": None,
                "stall_detected_tick": None,
                "ticks_since_meaningful_progress": 1,
                "last_meaningful_progress_tick": None,
                "distance_to_next_waypoint_mm": 500,
                "best_distance_to_next_waypoint_mm": 500,
                "hold_reason_counts": {"low_confidence": 0, "fallback": 0, "other": 0},
            }
        )
        self.assertEqual(list(propose_horizons(scenario, "mission:incomplete", evaluation)), [])

    def test_transition_failure_keeps_the_invalid_from_to_edge(self):
        original = self.make_evaluation(
            sample_noisy=False, edge_to={"x_mm": 10, "y_mm": 5}
        )
        original.report["invariants"] = [
            {
                "name": name,
                "passed": name != "valid_state_transitions",
                "failures": [],
            }
            for name in (
                "restricted_zone",
                "world_bounds",
                "safe_fallback",
                "valid_state_transitions",
            )
        ]
        evidence = {
            "tick": 1,
            "trigger_event_sequence": 3,
            "trigger_event_kind": "transition",
            "expected": "mission Pending->Running->Completed; safety Nominal<->Fallback",
            "observed": None,
        }
        original.report["invariants"][-1]["failures"] = [evidence]
        original.artifact["events"][-1]["event"] = {
            "kind": "transition",
            "subsystem": "mission",
            "from": "running",
            "to": "pending",
        }
        candidate = self.make_evaluation(
            sample_noisy=False, edge_to={"x_mm": 10, "y_mm": 5}
        )
        candidate.report["invariants"] = json.loads(json.dumps(original.report["invariants"]))
        candidate.artifact["events"][-1]["event"] = {
            "kind": "transition",
            "subsystem": "mission",
            "from": "running",
            "to": "completed",
        }
        signature = target_signature("invariant:valid_state_transitions", original)
        self.assertFalse(
            same_property("invariant:valid_state_transitions", signature, candidate)
        )


if __name__ == "__main__":
    unittest.main()
