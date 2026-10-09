from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))


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

from m3_failure_signature import (  # noqa: E402
    M3FailureSignatureError,
    counterexample_actuator_outcomes,
    same_temporal_failure,
    temporal_failure_signature,
)


def temporal_failure(**overrides):
    failure = {
        "schema_version": 1,
        "contract_id": "actuator_stop",
        "property_type": "bounded_response",
        "outcome": "FAIL",
        "first_trigger_tick": 7,
        "deadline_tick": 9,
        "first_violation_tick": 9,
        "evidence": [
            {
                "outcome": "FAIL",
                "trigger_tick": 7,
                "deadline_tick": 9,
                "violation_tick": 9,
                "trigger_trace_index": 5,
                "violation_trace_index": 7,
                "expected": {"predicate": "vehicle_stationary"},
                "observed_fact": {"predicate": "vehicle_stationary", "value": False},
                "observed_transition": None,
                "monitor_derived_command_outcome": None,
                "observation": "present",
                "reason": "required predicate remained false at the deadline",
                "detail": None,
            }
        ],
    }
    return {**failure, **overrides}


def actuator_outcomes(**overrides):
    outcomes = [
        {
            "tick": 8,
            "requested_action": {"heading": None, "distance_mm": 0},
            "realized_action": {"heading": "east", "distance_mm": 10},
            "behavior": "continued_movement",
            "source_command_tick": 5,
            "fault_events": [
                {
                    "tick": 8,
                    "fault_id": "stop_stuck",
                    "kind": "continued_movement",
                    "source_command_tick": 5,
                    "due_tick": None,
                    "execution_tick": 8,
                }
            ],
            "pending_commands_at_termination": [],
        }
    ]
    if overrides.get("behavior"):
        outcomes[0]["behavior"] = overrides["behavior"]
    if overrides.get("tick"):
        outcomes[0]["tick"] = overrides["tick"]
    if overrides.get("fault_events") is not None:
        outcomes[0]["fault_events"] = overrides["fault_events"]
    return outcomes


class M3TemporalFailureSignatureTests(unittest.TestCase):
    def test_signature_records_contract_obligation_violation_and_fault_mechanism(self):
        signature = temporal_failure_signature(temporal_failure(), actuator_outcomes())

        self.assertEqual(signature["contract_id"], "actuator_stop")
        self.assertEqual(signature["contract_schema_version"], 1)
        self.assertEqual(signature["property_type"], "bounded_response")
        self.assertEqual(signature["trigger_tick"], 7)
        self.assertEqual(signature["deadline_tick"], 9)
        self.assertEqual(signature["violation_tick"], 9)
        self.assertEqual(signature["mechanism"], ["continuing_actuation"])
        self.assertEqual(signature["fault_events"][0]["fault_id"], "stop_stuck")

    def test_equivalence_requires_same_contract_temporal_obligation_and_first_violation(self):
        original = temporal_failure_signature(temporal_failure(), actuator_outcomes())
        self.assertTrue(same_temporal_failure(original, temporal_failure(), actuator_outcomes()))
        for changed in (
            {"contract_id": "fallback_response"},
            {"schema_version": 2},
            {"property_type": "never"},
            {"first_trigger_tick": 6},
            {"deadline_tick": 10},
            {"first_violation_tick": 10},
        ):
            with self.subTest(changed=changed):
                self.assertFalse(same_temporal_failure(original, temporal_failure(**changed), actuator_outcomes()))
        self.assertFalse(
            same_temporal_failure(
                original, temporal_failure(), actuator_outcomes(behavior="lost")
            )
        )

    def test_signature_groups_delayed_scheduling_and_execution_as_same_mechanism(self):
        delayed = temporal_failure_signature(
            temporal_failure(), actuator_outcomes(behavior="delayed")
        )
        executed = temporal_failure_signature(
            temporal_failure(), actuator_outcomes(behavior="delayed_execution")
        )

        self.assertEqual(delayed["mechanism"], ["delayed_actuation"])
        self.assertTrue(
            same_temporal_failure(
                delayed, temporal_failure(), actuator_outcomes(behavior="delayed_execution")
            )
        )

    def test_signature_does_not_require_identical_source_event_ticks_after_reduction(self):
        original = temporal_failure_signature(temporal_failure(), actuator_outcomes())
        reduced = actuator_outcomes(tick=7)

        self.assertTrue(same_temporal_failure(original, temporal_failure(), reduced))

    def test_signature_uses_the_failed_obligation_after_an_earlier_trigger_passed(self):
        result = temporal_failure(
            first_trigger_tick=1,
            evidence=[
                {
                    "outcome": "PASS",
                    "trigger_tick": 1,
                    "deadline_tick": 3,
                    "violation_tick": None,
                },
                {
                    "outcome": "FAIL",
                    "trigger_tick": 7,
                    "deadline_tick": 9,
                    "violation_tick": 9,
                },
            ],
        )
        signature = temporal_failure_signature(result, actuator_outcomes())

        self.assertEqual(signature["trigger_tick"], 7)
        self.assertEqual(signature["deadline_tick"], 9)

    def test_signature_can_preserve_temporal_failures_without_an_actuator_fault(self):
        no_fault = [
            {
                "tick": 8,
                "behavior": "identity",
                "fault_events": [],
            }
        ]
        signature = temporal_failure_signature(temporal_failure(), no_fault)

        self.assertEqual(signature["mechanism"], ["no_actuator_fault_observed"])
        self.assertTrue(same_temporal_failure(signature, temporal_failure(), no_fault))
        self.assertFalse(
            same_temporal_failure(signature, temporal_failure(), actuator_outcomes())
        )

    def test_signature_rejects_a_summary_that_does_not_identify_the_first_failure(self):
        original = temporal_failure(
            evidence=[
                {
                    "outcome": "FAIL",
                    "trigger_tick": 7,
                    "deadline_tick": 9,
                    "violation_tick": 9,
                },
                {
                    "outcome": "FAIL",
                    "trigger_tick": 8,
                    "deadline_tick": 10,
                    "violation_tick": 10,
                },
            ]
        )
        self.assertEqual(
            temporal_failure_signature(original, actuator_outcomes())["violation_tick"], 9
        )
        with self.assertRaises(M3FailureSignatureError):
            temporal_failure_signature(
                temporal_failure(first_violation_tick=10, evidence=original["evidence"]),
                actuator_outcomes(),
            )

    def test_rejects_incomplete_or_ambiguous_temporal_failure_evidence(self):
        invalid = (
            {},
            temporal_failure(outcome="PASS"),
            temporal_failure(first_violation_tick=None),
            temporal_failure(deadline_tick=6),
            temporal_failure(first_trigger_tick=True),
            temporal_failure(contract_id=""),
            temporal_failure(property_type="unclassified_operator"),
        )
        for failure in invalid:
            with self.subTest(failure=failure), self.assertRaises(M3FailureSignatureError):
                temporal_failure_signature(failure, actuator_outcomes())
        for invalid_outcomes in ([], [{}], actuator_outcomes(fault_events=[])):
            with self.subTest(invalid_outcomes=invalid_outcomes), self.assertRaises(M3FailureSignatureError):
                temporal_failure_signature(temporal_failure(), invalid_outcomes)

    def test_candidate_with_invalid_evidence_cannot_match_original(self):
        original = temporal_failure_signature(temporal_failure(), actuator_outcomes())

        self.assertFalse(
            same_temporal_failure(
                original, temporal_failure(outcome="INCONCLUSIVE"), actuator_outcomes()
            )
        )

    def test_conclusive_command_failure_may_violate_before_its_response_deadline(self):
        result = temporal_failure(
            first_violation_tick=8,
            evidence=[
                {
                    "outcome": "FAIL",
                    "trigger_tick": 7,
                    "deadline_tick": 9,
                    "violation_tick": 8,
                    "trigger_trace_index": 6,
                    "violation_trace_index": 7,
                    "expected": {"predicate": "command_applied"},
                    "observed_fact": {"predicate": "command_resolved", "value": "explicit_failure"},
                    "observed_transition": None,
                    "monitor_derived_command_outcome": None,
                    "observation": "present",
                    "reason": "explicit actuator failure",
                    "detail": "command_id=12",
                }
            ],
        )
        outcome = [
            {
                "tick": 8,
                "behavior": "lost",
                "fault_events": [
                    {
                        "tick": 8,
                        "fault_id": "lost_command",
                        "kind": "command_lost",
                        "source_command_tick": 7,
                        "due_tick": None,
                        "execution_tick": None,
                    }
                ],
            }
        ]

        signature = temporal_failure_signature(result, outcome)

        self.assertEqual(signature["deadline_tick"], 9)
        self.assertEqual(signature["violation_tick"], 8)

    def test_adapts_bounded_contract_failure_ticks_without_inventing_actuator_events(self):
        failure = {
            "counterexample_ticks": [
                {
                    "tick": 7,
                    "actuator_behavior": "continued_movement",
                    "fault_events": [
                        {
                            "event": {
                                "tick": 7,
                                "fault_id": "stop_stuck",
                                "kind": "continued_movement",
                                "source_command_tick": 5,
                                "due_tick": None,
                                "execution_tick": 7,
                            },
                            "observed_event_sequence": 12,
                        }
                    ],
                },
                {"tick": 8, "actuator_behavior": "identity", "fault_events": []},
            ]
        }

        outcomes = counterexample_actuator_outcomes(failure)

        self.assertEqual(outcomes[0]["behavior"], "continued_movement")
        self.assertEqual(outcomes[0]["fault_events"][0]["fault_id"], "stop_stuck")
        self.assertEqual(outcomes[1]["behavior"], "identity")
        self.assertEqual(outcomes[1]["fault_events"], [])

    def test_rejects_missing_or_malformed_bounded_counterexample_ticks(self):
        for failure in ({}, {"counterexample_ticks": []}, {"counterexample_ticks": [None]}):
            with self.subTest(failure=failure), self.assertRaises(M3FailureSignatureError):
                counterexample_actuator_outcomes(failure)

        with self.assertRaises(M3FailureSignatureError):
            counterexample_actuator_outcomes(
                {"counterexample_ticks": [{"tick": 7, "fault_events": ["invented"]}]}
            )


@unittest.skipUnless(M3_BINARY, "build vv-lab with the M3 CLI before running minimization integration tests")
class M3StopCounterexampleMinimizationTests(unittest.TestCase):
    def test_continued_stop_failure_is_reduced_and_both_traces_replay(self):
        binary = M3_BINARY
        assert binary is not None
        scenario = ROOT / "scenarios" / "m3" / "nominal-stop.json"
        contracts = ROOT / "scenarios" / "m3" / "contracts-stop.json"
        actuator = ROOT / "scenarios" / "m3" / "actuator-continued-stop.json"
        with tempfile.TemporaryDirectory(dir=ROOT) as temporary:
            output = Path(temporary) / "minimized"
            process = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "scripts" / "m3_minimize_failure.py"),
                    str(scenario),
                    "--contracts",
                    str(contracts),
                    "--actuator",
                    str(actuator),
                    "--contract-id",
                    "stop_response",
                    "--seed",
                    "42",
                    "--output",
                    str(output),
                    "--binary",
                    str(binary),
                    "--max-candidates",
                    "8",
                    "--time-budget-seconds",
                    "30",
                ],
                cwd=ROOT,
                capture_output=True,
                text=True,
                timeout=40,
                check=False,
            )
            self.assertEqual(process.returncode, 0, process.stdout + process.stderr)
            summary = json.loads((output / "summary.json").read_text(encoding="utf-8"))
            self.assertEqual(summary["status"], "reduced")
            self.assertEqual(summary["contract_id"], "stop_response")
            self.assertLess(summary["reduced_steps"], summary["original_steps"])
            for field in (
                "contract_id",
                "contract_schema_version",
                "property_type",
                "trigger_tick",
                "deadline_tick",
                "violation_tick",
                "mechanism",
            ):
                self.assertEqual(summary["original_signature"][field], summary["reduced_signature"][field])
            signature = summary["original_signature"]
            self.assertEqual(signature["trigger_tick"], 7)
            self.assertEqual(signature["deadline_tick"], 9)
            self.assertEqual(signature["violation_tick"], 9)
            self.assertEqual(signature["mechanism"], ["continuing_actuation"])
            self.assertTrue(
                any(event["fault_id"] == "stuck_after_stop" for event in signature["fault_events"])
            )
            for folder in ("original", "reduced"):
                for name in ("events.json", "report.json", "mission.json", "verification.json"):
                    self.assertTrue((output / folder / name).is_file())
                replay = subprocess.run(
                    [str(binary), "replay", str(output / folder / "events.json")],
                    cwd=ROOT,
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                self.assertEqual(replay.returncode, 0, replay.stdout + replay.stderr)


if __name__ == "__main__":
    unittest.main()
