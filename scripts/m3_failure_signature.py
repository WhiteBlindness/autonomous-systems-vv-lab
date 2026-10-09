"""Stable temporal-failure identity for M3 campaign and minimization tooling."""

from __future__ import annotations

from typing import Any


class M3FailureSignatureError(ValueError):
    """A temporal counterexample lacks enough evidence for safe comparison."""


def counterexample_actuator_outcomes(failure_evidence: dict[str, Any]) -> list[dict[str, Any]]:
    """Convert the bounded failure slice into directly observed actuator rows.

    M3 stores a trace hash for the complete actuator trace. A failed contract
    retains only the ticks around its obligation, including linked fault events.
    This adapter deliberately consumes that bounded evidence instead of
    reconstructing unrecorded actuator behavior.
    """
    if not isinstance(failure_evidence, dict):
        raise M3FailureSignatureError("failure evidence must be an object")
    ticks = failure_evidence.get("counterexample_ticks")
    if not isinstance(ticks, list) or not ticks:
        raise M3FailureSignatureError("failure evidence has no counterexample ticks")
    result = []
    for index, row in enumerate(ticks):
        if not isinstance(row, dict):
            raise M3FailureSignatureError(f"counterexample_ticks[{index}] must be an object")
        linked_events = row.get("fault_events", [])
        if not isinstance(linked_events, list):
            raise M3FailureSignatureError(f"counterexample_ticks[{index}].fault_events must be a list")
        events = []
        for event_index, linked in enumerate(linked_events):
            if not isinstance(linked, dict) or not isinstance(linked.get("event"), dict):
                raise M3FailureSignatureError(
                    f"counterexample_ticks[{index}].fault_events[{event_index}] is malformed"
                )
            events.append(linked["event"])
        result.append(
            {
                "tick": row.get("tick"),
                "behavior": row.get("actuator_behavior"),
                "fault_events": events,
            }
        )
    return result


_PROPERTY_TYPES = frozenset(
    {"always", "never", "bounded_response", "ordered_transition", "bounded_progress"}
)
_IDENTITY_FIELDS = (
    "contract_id",
    "contract_schema_version",
    "property_type",
    "trigger_tick",
    "deadline_tick",
    "violation_tick",
    "mechanism",
)
_BEHAVIOUR_MECHANISMS = {
    "identity": None,
    "lost": "command_not_applied",
    "delayed": "delayed_actuation",
    "delayed_execution": "delayed_actuation",
    "continued_movement": "continuing_actuation",
}


def _required_text(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise M3FailureSignatureError(f"{label} is missing")
    return value


def _optional_tick(value: Any, label: str) -> int | None:
    if value is None:
        return None
    if type(value) is not int or value < 1 or value > 2**32 - 1:
        raise M3FailureSignatureError(f"{label} must be a positive u32 tick or null")
    return value


def _fault_summary(value: Any, index: int) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise M3FailureSignatureError(f"fault event {index} must be an object")
    kind = _required_text(value.get("kind"), f"fault event {index} kind")
    fault_id = _required_text(value.get("fault_id"), f"fault event {index} fault_id")
    tick = _optional_tick(value.get("tick"), f"fault event {index} tick")
    source_command_tick = _optional_tick(
        value.get("source_command_tick"), f"fault event {index} source_command_tick"
    )
    if tick is None or source_command_tick is None:
        raise M3FailureSignatureError(f"fault event {index} is missing a source or observation tick")
    return {
        "kind": kind,
        "fault_id": fault_id,
        "tick": tick,
        "source_command_tick": source_command_tick,
    }


def temporal_failure_signature(
    contract_result: dict[str, Any], actuator_outcomes: list[dict[str, Any]]
) -> dict[str, Any]:
    """Summarize a failed ContractResult and directly observed actuator evidence.

    The temporal evaluator supplies the contract identity and obligation ticks.
    The broad mechanism comes only from recorded ActuationOutcome values; this
    function does not infer a cause from the trajectory or a scenario name.
    """
    if not isinstance(contract_result, dict) or contract_result.get("outcome") != "FAIL":
        raise M3FailureSignatureError("only an explicitly failed temporal contract is minimizable")
    contract_id = _required_text(contract_result.get("contract_id"), "contract_id")
    schema_version = contract_result.get("schema_version")
    if type(schema_version) is not int or not 1 <= schema_version <= 2**32 - 1:
        raise M3FailureSignatureError("schema_version must be a positive u32")
    property_type = _required_text(contract_result.get("property_type"), "property_type")
    if property_type not in _PROPERTY_TYPES:
        raise M3FailureSignatureError("property_type is not a supported temporal operator")

    summary_trigger_tick = _optional_tick(
        contract_result.get("first_trigger_tick"), "first_trigger_tick"
    )
    summary_deadline_tick = _optional_tick(
        contract_result.get("deadline_tick"), "deadline_tick"
    )
    violation_tick = _optional_tick(
        contract_result.get("first_violation_tick"), "first_violation_tick"
    )
    if violation_tick is None:
        raise M3FailureSignatureError("first_violation_tick is required")

    evidence = contract_result.get("evidence")
    if not isinstance(evidence, list) or not any(
        isinstance(item, dict) and item.get("outcome") == "FAIL" for item in evidence
    ):
        raise M3FailureSignatureError("failed contract has no failed obligation evidence")
    failed_evidence = [
        item
        for item in evidence
        if isinstance(item, dict)
        and item.get("outcome") == "FAIL"
        and type(item.get("violation_tick")) is int
    ]
    evidence_ticks = [item["violation_tick"] for item in failed_evidence]
    if not evidence_ticks or min(evidence_ticks) != violation_tick:
        raise M3FailureSignatureError("first_violation_tick disagrees with obligation evidence")
    first_violation_evidence = next(
        item for item in failed_evidence if item["violation_tick"] == violation_tick
    )
    evidence_trigger = _optional_tick(
        first_violation_evidence.get("trigger_tick"), "evidence trigger_tick"
    )
    evidence_deadline = _optional_tick(
        first_violation_evidence.get("deadline_tick"), "evidence deadline_tick"
    )
    if property_type in {"bounded_response", "bounded_progress"}:
        if evidence_trigger is None or evidence_deadline is None:
            raise M3FailureSignatureError(
                f"{property_type} failure evidence requires its own trigger and deadline"
            )
        summary_triggers = [
            item.get("trigger_tick")
            for item in evidence
            if isinstance(item, dict) and type(item.get("trigger_tick")) is int
        ]
        if not summary_triggers or min(summary_triggers) != summary_trigger_tick:
            raise M3FailureSignatureError(
                "first_trigger_tick disagrees with the earliest obligation evidence"
            )
        if summary_deadline_tick != evidence_deadline:
            raise M3FailureSignatureError(
                "deadline_tick disagrees with the first failure evidence"
            )
        if evidence_deadline < evidence_trigger or violation_tick < evidence_trigger:
            raise M3FailureSignatureError("temporal violation or deadline precedes its failed trigger")
        # A contract result reports the earliest trigger across all obligations.
        # Failure identity must instead use the obligation that actually failed.
        trigger_tick = evidence_trigger
        deadline_tick = evidence_deadline
    else:
        trigger_tick = evidence_trigger or summary_trigger_tick
        deadline_tick = summary_deadline_tick
        if deadline_tick is not None and trigger_tick is not None and deadline_tick < trigger_tick:
            raise M3FailureSignatureError("deadline_tick precedes first_trigger_tick")

    if not isinstance(actuator_outcomes, list) or not actuator_outcomes:
        raise M3FailureSignatureError("actuator outcomes are required to establish the failure window")
    first_relevant_tick = trigger_tick or 1
    relevant = []
    mechanisms: set[str] = set()
    fault_events: list[dict[str, Any]] = []
    for index, outcome in enumerate(actuator_outcomes):
        if not isinstance(outcome, dict):
            raise M3FailureSignatureError(f"actuator_outcomes[{index}] must be an object")
        tick = _optional_tick(outcome.get("tick"), f"actuator_outcomes[{index}].tick")
        behavior = _required_text(outcome.get("behavior"), f"actuator_outcomes[{index}].behavior")
        if behavior not in _BEHAVIOUR_MECHANISMS:
            raise M3FailureSignatureError(f"unknown actuator behavior: {behavior}")
        if tick is None or not first_relevant_tick <= tick <= violation_tick:
            continue
        relevant.append({"tick": tick, "behavior": behavior})
        mechanism = _BEHAVIOUR_MECHANISMS[behavior]
        if mechanism is None:
            continue
        mechanisms.add(mechanism)
        events = outcome.get("fault_events")
        if not isinstance(events, list) or not events:
            raise M3FailureSignatureError(
                f"faulted actuator outcome at tick {tick} has no fault event evidence"
            )
        for event in events:
            fault_events.append(_fault_summary(event, len(fault_events)))
    if not mechanisms:
        if not relevant:
            raise M3FailureSignatureError(
                "no actuator behavior overlaps the failed obligation"
            )
        mechanisms.add("no_actuator_fault_observed")

    return {
        "kind": "temporal",
        "contract_id": contract_id,
        "contract_schema_version": schema_version,
        "property_type": property_type,
        "trigger_tick": trigger_tick,
        "deadline_tick": deadline_tick,
        "violation_tick": violation_tick,
        "mechanism": sorted(mechanisms),
        "actuator_behaviors": relevant,
        "fault_events": fault_events,
    }


def same_temporal_failure(
    original_signature: dict[str, Any],
    candidate_result: dict[str, Any],
    candidate_actuator_outcomes: list[dict[str, Any]],
) -> bool:
    """Check whether a candidate preserves the same temporal failure identity."""
    try:
        candidate_signature = temporal_failure_signature(
            candidate_result, candidate_actuator_outcomes
        )
    except M3FailureSignatureError:
        return False
    if not isinstance(original_signature, dict):
        return False
    return all(
        original_signature.get(field) == candidate_signature[field]
        for field in _IDENTITY_FIELDS
    )
