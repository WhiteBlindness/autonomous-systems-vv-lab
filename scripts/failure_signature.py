"""Assinaturas causais das falhas e propriedades das missões."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from failure_reduction import Evaluation

INVARIANTS = (
    "restricted_zone",
    "world_bounds",
    "safe_fallback",
    "valid_state_transitions",
)
MISSION_FAILURES = {"incomplete", "stalled", "invalid_terminated"}

def failed_invariant_names(report: dict[str, Any]) -> tuple[str, ...]:
    return tuple(sorted(item["name"] for item in report.get("invariants", []) if item.get("passed") is False))


def get_mission_property(mission: dict[str, Any]) -> str | None:
    outcome = mission.get("outcome")
    return f"mission:{outcome}" if outcome in MISSION_FAILURES else None

def find_event(artifact: dict[str, Any], sequence: int) -> dict[str, Any] | None:
    for record in artifact.get("events", []):
        if record.get("sequence") == sequence:
            return record
    return None


def sample_mechanisms(
    artifact: dict[str, Any], evidence: dict[str, Any]
) -> dict[str, Any]:
    """Classifica falhas na amostra GPS, sem comparar com a verdade pós-movimento."""
    observed = evidence.get("observed")
    sample_tick = observed.get("sample_tick") if isinstance(observed, dict) else None
    if type(sample_tick) is not int:
        # Sem observação, a execução ainda não aceitou qualquer entrega.
        sample_tick = 0
    sample = next(
        (
            record.get("event", {})
            for record in artifact.get("events", [])
            if record.get("event", {}).get("kind") == "gps_sample"
            and record.get("event", {}).get("packet_sequence") == sample_tick
        ),
        None,
    )
    packet = next(
        (
            record.get("event", {})
            for record in artifact.get("events", [])
            if record.get("event", {}).get("kind") == "packet_outcome"
            and record.get("event", {}).get("packet_sequence") == sample_tick
        ),
        None,
    )
    # O ruído compara a observação com a verdade no instante da amostra.
    noisy_sample = bool(
        isinstance(sample, dict)
        and isinstance(sample.get("truth_at_sample"), dict)
        and isinstance(sample.get("observation"), dict)
        and sample["truth_at_sample"] != sample["observation"]
    )
    age = observed.get("age_ticks") if isinstance(observed, dict) else None
    violation_tick = evidence.get("tick")
    later_events = events_after_sample(artifact, sample_tick, violation_tick)
    return {
        "gps_sample_available": sample is not None,
        "gps_dropout": bool(sample and sample.get("dropped")),
        "gps_noise": noisy_sample,
        "packet_loss": bool(packet and packet.get("delivered") is False),
        "delayed_sample": type(age) is int and age > 0,
        "missing_observation": not isinstance(observed, dict),
        "gps_dropout_after_sample": any(
            record.get("event", {}).get("kind") == "gps_sample"
            and record.get("event", {}).get("dropped") is True
            for record in later_events
        ),
        "packet_loss_after_sample": any(
            record.get("event", {}).get("kind") == "packet_outcome"
            and record.get("event", {}).get("delivered") is False
            for record in later_events
        ),
    }


def events_after_sample(
    artifact: dict[str, Any], sample_tick: Any, violation_tick: Any
) -> list[dict[str, Any]]:
    if type(sample_tick) is not int or type(violation_tick) is not int:
        return []
    return [
        record
        for record in artifact.get("events", [])
        if type(record.get("tick")) is int
        and sample_tick < record["tick"] <= violation_tick
    ]


def stall_window_mechanisms(
    artifact: dict[str, Any], end_tick: int, window_ticks: int
) -> dict[str, bool]:
    start_tick = max(1, end_tick - window_ticks + 1)
    records = [
        record
        for record in artifact.get("events", [])
        if type(record.get("tick")) is int
        and start_tick <= record["tick"] <= end_tick
    ]
    gps_dropout = False
    packet_loss = False
    delayed_sample = False
    gps_noise = False
    for record in records:
        event = record.get("event", {})
        kind = event.get("kind")
        if kind == "gps_sample":
            if event.get("dropped") is True:
                gps_dropout = True
            truth = event.get("truth_at_sample")
            observation = event.get("observation")
            if (
                event.get("dropped") is False
                and isinstance(truth, dict)
                and isinstance(observation, dict)
                and truth != observation
            ):
                gps_noise = True
        elif kind == "packet_outcome":
            if event.get("delivered") is False:
                packet_loss = True
            if (
                event.get("delivered") is True
                and type(event.get("delay_ticks")) is int
                and event["delay_ticks"] > 0
            ):
                delayed_sample = True
    return {
        "gps_dropout": gps_dropout,
        "packet_loss": packet_loss,
        "delayed_sample": delayed_sample,
        "gps_noise": gps_noise,
    }


def point_on_segment(point: dict[str, int], start: dict[str, int], end: dict[str, int]) -> bool:
    cross = (point["x_mm"] - start["x_mm"]) * (end["y_mm"] - start["y_mm"]) - (
        point["y_mm"] - start["y_mm"]
    ) * (end["x_mm"] - start["x_mm"])
    return (
        cross == 0
        and min(start["x_mm"], end["x_mm"]) <= point["x_mm"] <= max(start["x_mm"], end["x_mm"])
        and min(start["y_mm"], end["y_mm"]) <= point["y_mm"] <= max(start["y_mm"], end["y_mm"])
    )


def point_in_or_on_polygon(point: dict[str, int], vertices: list[dict[str, int]]) -> bool:
    inside = False
    previous = vertices[-1]
    for current in vertices:
        if point_on_segment(point, previous, current):
            return True
        crosses = (current["y_mm"] > point["y_mm"]) != (previous["y_mm"] > point["y_mm"])
        if crosses:
            numerator = (
                current["x_mm"] * (previous["y_mm"] - point["y_mm"])
                + previous["x_mm"] * (point["y_mm"] - current["y_mm"])
            )
            denominator = previous["y_mm"] - current["y_mm"]
            left = point["x_mm"] * denominator
            crosses_right = left < numerator if denominator > 0 else left > numerator
            if crosses_right:
                inside = not inside
        previous = current
    return inside


def condition_category(
    invariant_name: str,
    evidence: dict[str, Any],
    artifact: dict[str, Any],
) -> tuple[str, dict[str, Any] | None]:
    trigger = find_event(artifact, evidence.get("trigger_event_sequence", -1))
    event = trigger.get("event", {}) if trigger else {}
    if invariant_name == "restricted_zone" and event.get("kind") == "tick_result":
        start = event.get("truth_from")
        end = event.get("truth_position")
        vertices = artifact.get("scenario", {}).get("restricted_zone", {}).get("vertices", [])
        if isinstance(start, dict) and isinstance(end, dict) and vertices:
            start_relation = point_relation(start, vertices)
            end_relation = point_relation(end, vertices)
            edge = {"from": start_relation, "to": end_relation}
            if start_relation != "outside":
                return "started_inside_or_on_zone", edge
            if end_relation != "outside":
                return "ended_inside_or_on_zone", edge
            return "segment_touched_or_crossed_zone", edge
    if invariant_name == "world_bounds" and event.get("kind") == "tick_result":
        start = event.get("truth_from")
        end = event.get("truth_position")
        bounds = artifact.get("scenario", {}).get("bounds", {})
        if isinstance(start, dict) and isinstance(end, dict) and bounds:
            def in_bounds(point: dict[str, int]) -> bool:
                return (
                    bounds["min_x_mm"] <= point["x_mm"] <= bounds["max_x_mm"]
                    and bounds["min_y_mm"] <= point["y_mm"] <= bounds["max_y_mm"]
                )

            state = (in_bounds(start), in_bounds(end))
            category = "started_outside_bounds" if not state[0] else "ended_outside_bounds"
            return category, {"from": "inside" if state[0] else "outside", "to": "inside" if state[1] else "outside"}
    if invariant_name == "valid_state_transitions":
        if event.get("kind") == "transition":
            edge = {
                "subsystem": event.get("subsystem"),
                "from": event.get("from"),
                "to": event.get("to"),
            }
            return "invalid_transition_edge", edge
        if event.get("kind") == "tick_result":
            transitions = [
                record["event"]
                for record in artifact.get("events", [])
                if record.get("tick") == evidence.get("tick")
                and record.get("event", {}).get("kind") == "transition"
            ]
            edges = [
                {"subsystem": item.get("subsystem"), "from": item.get("from"), "to": item.get("to")}
                for item in transitions
            ]
            return "transition_chain_mismatch", {
                "transitions": edges,
                "mission_state": event.get("mission_state"),
                "safety_state": event.get("safety_state"),
            }
        return "transition_chain_mismatch", None
    if invariant_name == "safe_fallback":
        if event.get("kind") == "tick_result":
            telemetry = next(
                (
                    item
                    for item in artifact.get("expected_report", {}).get("telemetry", [])
                    if item.get("tick") == evidence.get("tick")
                ),
                {},
            )
            observed = event.get("observed")
            threshold = artifact.get("scenario", {}).get("safety", {}).get(
                "confidence_threshold_permille", 500
            )
            low_confidence = not isinstance(observed, dict) or not observed.get("fresh") or (
                observed.get("confidence_permille", 0) < threshold
            )
            watchdog_required = telemetry.get("low_confidence_watchdog_required", False)
            safety_state = event.get("safety_state")
            if watchdog_required and safety_state != "fallback":
                condition = "fallback_not_entered"
            else:
                condition = "movement_during_low_confidence_or_fallback"
            previous_safety = "nominal"
            previous_results = [
                record
                for record in artifact.get("events", [])
                if record.get("event", {}).get("kind") == "tick_result"
                and record.get("tick", 0) < evidence.get("tick", 0)
            ]
            if previous_results:
                previous = max(previous_results, key=lambda item: item.get("tick", 0))
                previous_safety = previous.get("event", {}).get("safety_state", "nominal")
            start = event.get("truth_from")
            end = event.get("truth_position")
            moved = start != end
            return condition, {
                "safety": {"from": previous_safety, "to": safety_state},
                "motion": "moved" if moved else "held",
                "low_confidence": low_confidence,
            }
        return "fallback_or_hold_contract_violation", None
    return "invariant_condition", None


def invariant_failure_signature(
    invariant_name: str, evidence: dict[str, Any], artifact: dict[str, Any]
) -> dict[str, Any]:
    condition, state_edge = condition_category(invariant_name, evidence, artifact)
    return {
        "invariant": invariant_name,
        "expected": evidence.get("expected"),
        "condition": condition,
        "trigger_event_kind": evidence.get("trigger_event_kind"),
        "mechanisms": sample_mechanisms(artifact, evidence),
        "state_edge": state_edge,
    }


def point_relation(point: dict[str, int], vertices: list[dict[str, int]]) -> str:
    if any(point_on_segment(point, vertices[index - 1], vertex) for index, vertex in enumerate(vertices)):
        return "boundary"
    return "inside" if point_in_or_on_polygon(point, vertices) else "outside"


def target_signature(property_name: str, evaluation: Evaluation) -> dict[str, Any]:
    failed = failed_invariant_names(evaluation.report)
    if property_name.startswith("invariant:"):
        name = property_name.split(":", 1)[1]
        result = next(item for item in evaluation.report["invariants"] if item["name"] == name)
        return {
            "kind": "invariant",
            "name": name,
            "failed_invariants": list(failed),
            "evidence": invariant_failure_signature(name, result["failures"][0], evaluation.artifact),
        }

    mission = evaluation.mission
    outcome = mission["outcome"]
    criteria: dict[str, Any] = {
        "waypoint_count": mission["waypoint_count"],
        "waypoints_completed": mission["waypoints_completed"],
        "waypoints_remaining": mission["waypoints_remaining"],
        "controller_waypoint_index": mission["controller_waypoint_index"],
    }
    if outcome == "stalled":
        criteria["fault_hold_class"] = hold_reason_class(mission)
        criteria["fallback_exposure"] = mission["fallback_ticks"] > 0
        end_tick = evaluation.report.get("ticks", mission["stall_detected_tick"])
        criteria["stall_window_mechanisms"] = stall_window_mechanisms(
            evaluation.artifact, end_tick, mission["policy"]["stall_window_ticks"]
        )
        criteria.update(
            {
                "stall_window_ticks": mission["policy"]["stall_window_ticks"],
                "stall_threshold_reached": mission["ticks_since_meaningful_progress"]
                >= mission["policy"]["stall_window_ticks"],
                "stall_detected": mission["stall_detected_tick"] is not None,
            }
        )
    elif outcome == "incomplete":
        criteria["incomplete_class"] = (
            "physical_waypoints_reached_unconfirmed"
            if mission["waypoints_remaining"] == 0
            else "horizon_before_stall"
            if mission["ticks_since_meaningful_progress"] < mission["policy"]["stall_window_ticks"]
            else "incomplete_with_stall_threshold"
        )
        criteria["fault_hold_class"] = hold_reason_class(mission)
        criteria["fallback_exposure"] = mission["fallback_ticks"] > 0
        criteria["meaningful_progress_observed"] = (
            mission["last_meaningful_progress_tick"] is not None
        )
        criteria["ticks_since_meaningful_progress"] = mission[
            "ticks_since_meaningful_progress"
        ]
        criteria["distance_to_next_waypoint_mm"] = mission[
            "distance_to_next_waypoint_mm"
        ]
        criteria["best_distance_to_next_waypoint_mm"] = mission[
            "best_distance_to_next_waypoint_mm"
        ]
    elif outcome == "invalid_terminated":
        criteria["completion_unconfirmed"] = mission["completion_tick"] is not None
    return {
        "kind": "mission",
        "outcome": outcome,
        "reason": mission["reason"],
        "policy": mission["policy"],
        "criteria": criteria,
        "failed_invariants": list(failed),
    }


def hold_reason_class(mission: dict[str, Any]) -> str:
    holds = mission["hold_reason_counts"]
    low_confidence = holds["low_confidence"] > 0
    fallback = holds["fallback"] > 0
    if low_confidence and fallback:
        return "low_confidence_and_fallback"
    if low_confidence:
        return "low_confidence"
    if fallback:
        return "fallback"
    return "other_hold" if holds["other"] > 0 else "no_hold"


def same_property(
    property_name: str,
    original_signature: dict[str, Any],
    candidate: Evaluation,
) -> bool:
    candidate_failed = failed_invariant_names(candidate.report)
    if candidate_failed != tuple(original_signature["failed_invariants"]):
        return False
    if property_name.startswith("invariant:"):
        name = property_name.split(":", 1)[1]
        result = next(
            (item for item in candidate.report.get("invariants", []) if item.get("name") == name),
            None,
        )
        if not result or result.get("passed") is not False:
            return False
        failures = result.get("failures", [])
        if not failures:
            return False
        wanted = original_signature["evidence"]
        return invariant_failure_signature(name, failures[0], candidate.artifact) == wanted
    candidate_signature = target_signature(property_name, candidate)
    return candidate_signature == original_signature
