use std::collections::BTreeMap;

use crate::artifact::Recorder;
use crate::geometry::{point_in_or_on_polygon, segment_hits_polygon, segment_stays_in_bounds};
use crate::model::{
    Action, EventPayload, FailureEvidence, INVARIANT_NAMES, InvariantResult, MissionState,
    ObservedTelemetry, Point, RunStatus, SafetyState, Scenario, TickTelemetry, TransitionReport,
    transition_is_valid,
};

#[derive(Clone, Debug)]
pub(crate) struct InvariantTracker {
    pub(crate) failures: BTreeMap<String, Vec<FailureEvidence>>,
    pub(crate) transitions: Vec<TransitionReport>,
    pub(crate) telemetry: Vec<TickTelemetry>,
    pub(crate) verifier_low_streak: u32,
    previous_mission_state: MissionState,
    previous_safety_state: SafetyState,
}

impl InvariantTracker {
    pub(crate) fn new(steps: u32) -> Self {
        Self {
            failures: INVARIANT_NAMES
                .iter()
                .map(|name| ((*name).to_owned(), Vec::new()))
                .collect(),
            transitions: Vec::new(),
            telemetry: Vec::with_capacity(steps as usize),
            verifier_low_streak: 0,
            previous_mission_state: MissionState::Pending,
            previous_safety_state: SafetyState::Nominal,
        }
    }

    pub(crate) fn report_results(&self) -> Vec<InvariantResult> {
        INVARIANT_NAMES
            .iter()
            .map(|name| {
                let failures = self.failures.get(*name).cloned().unwrap_or_default();
                InvariantResult {
                    name: (*name).to_owned(),
                    passed: failures.is_empty(),
                    failures,
                }
            })
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn verify_tick(
    scenario: &Scenario,
    tick: u32,
    truth_from: Point,
    truth_position: Point,
    observed: Option<&ObservedTelemetry>,
    confidence: u16,
    watchdog_required: bool,
    mission_state: MissionState,
    safety_state: SafetyState,
    action: Action,
    tick_result_sequence: u64,
    transition_sequences: &[u64],
    tracker: &mut InvariantTracker,
    recorder: &mut Recorder,
) {
    verify_restricted_zone(
        scenario,
        tick,
        truth_from,
        truth_position,
        observed,
        tick_result_sequence,
        tracker,
        recorder,
    );
    verify_world_bounds(
        scenario,
        tick,
        truth_from,
        truth_position,
        observed,
        tick_result_sequence,
        tracker,
        recorder,
    );
    verify_safe_fallback(
        scenario,
        tick,
        truth_from,
        truth_position,
        observed,
        confidence,
        watchdog_required,
        safety_state,
        action,
        tick_result_sequence,
        tracker,
        recorder,
    );
    verify_state_transitions(
        StateTransitionContext {
            tick,
            truth_position,
            observed,
            tick_result_sequence,
            mission_state,
            safety_state,
            transition_sequences,
        },
        tracker,
        recorder,
    );
}

#[allow(clippy::too_many_arguments)]
fn verify_restricted_zone(
    scenario: &Scenario,
    tick: u32,
    truth_from: Point,
    truth_position: Point,
    observed: Option<&ObservedTelemetry>,
    trigger_sequence: u64,
    tracker: &mut InvariantTracker,
    recorder: &mut Recorder,
) {
    let was_inside = point_in_or_on_polygon(truth_from, &scenario.restricted_zone.vertices);
    let crossing = segment_hits_polygon(
        truth_from,
        truth_position,
        &scenario.restricted_zone.vertices,
    );
    let passed = !was_inside && !crossing;
    let detail = if passed {
        "truth stayed outside the restricted polygon"
    } else if was_inside {
        "truth started inside or on the restricted polygon"
    } else {
        "truth segment entered, crossed, or touched the restricted polygon"
    };
    record_invariant(
        "restricted_zone",
        passed,
        detail,
        tick,
        truth_position,
        observed,
        trigger_sequence,
        "tick_result",
        "truth position and swept segment outside restricted polygon",
        format!(
            "from=({},{}); to=({},{})",
            truth_from.x_mm, truth_from.y_mm, truth_position.x_mm, truth_position.y_mm
        ),
        tracker,
        recorder,
    );
}

#[allow(clippy::too_many_arguments)]
fn verify_world_bounds(
    scenario: &Scenario,
    tick: u32,
    truth_from: Point,
    truth_position: Point,
    observed: Option<&ObservedTelemetry>,
    trigger_sequence: u64,
    tracker: &mut InvariantTracker,
    recorder: &mut Recorder,
) {
    let passed = segment_stays_in_bounds(truth_from, truth_position, &scenario.bounds);
    record_invariant(
        "world_bounds",
        passed,
        if passed {
            "truth remained inside world bounds"
        } else {
            "truth left world bounds"
        },
        tick,
        truth_position,
        observed,
        trigger_sequence,
        "tick_result",
        "truth segment inside configured world bounds",
        format!(
            "from=({},{}); to=({},{})",
            truth_from.x_mm, truth_from.y_mm, truth_position.x_mm, truth_position.y_mm
        ),
        tracker,
        recorder,
    );
}

#[allow(clippy::too_many_arguments)]
fn verify_safe_fallback(
    scenario: &Scenario,
    tick: u32,
    truth_from: Point,
    truth_position: Point,
    observed: Option<&ObservedTelemetry>,
    confidence: u16,
    controller_watchdog_required: bool,
    safety_state: SafetyState,
    action: Action,
    trigger_sequence: u64,
    tracker: &mut InvariantTracker,
    recorder: &mut Recorder,
) {
    let low_confidence = observed.is_none_or(|sample| {
        !sample.fresh || sample.confidence_permille < scenario.safety.confidence_threshold_permille
    });
    tracker.verifier_low_streak = if low_confidence {
        tracker.verifier_low_streak.saturating_add(1)
    } else {
        0
    };
    let required = tracker.verifier_low_streak >= scenario.safety.fallback_after_ticks;
    let passed = safe_fallback_holds(
        required,
        low_confidence,
        safety_state,
        action,
        truth_from,
        truth_position,
    );
    record_invariant(
        "safe_fallback",
        passed,
        if passed {
            "fallback requirement satisfied"
        } else {
            "fallback requirement was missed"
        },
        tick,
        truth_position,
        observed,
        trigger_sequence,
        "tick_result",
        "Fallback after the configured low-confidence interval; hold without truth motion during low confidence or while in Fallback",
        format!(
            "state={}; action_heading={:?}; action_distance_mm={}; confidence={}; streak={}; watchdog={}",
            safety_state,
            action.heading,
            action.distance_mm,
            confidence,
            tracker.verifier_low_streak,
            controller_watchdog_required,
        ),
        tracker,
        recorder,
    );
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TransitionIssue {
    invalid_graph_sequence: Option<u64>,
    detail: String,
}

type TransitionStep<'a> = (u64, &'a str, &'a str, &'a str);

struct StateTransitionContext<'a> {
    tick: u32,
    truth_position: Point,
    observed: Option<&'a ObservedTelemetry>,
    tick_result_sequence: u64,
    mission_state: MissionState,
    safety_state: SafetyState,
    transition_sequences: &'a [u64],
}

fn validate_transition_chain(
    previous_mission_state: MissionState,
    previous_safety_state: SafetyState,
    actual_mission_state: MissionState,
    actual_safety_state: SafetyState,
    transitions: &[TransitionStep<'_>],
) -> Option<TransitionIssue> {
    let mut mission_state = previous_mission_state.to_string();
    let mut safety_state = previous_safety_state.to_string();
    let mut semantic_detail = None;
    let mut invalid_graph = None;

    for (sequence, subsystem, from, to) in transitions {
        if !transition_is_valid(subsystem, from, to) && invalid_graph.is_none() {
            invalid_graph = Some((
                *sequence,
                format!("{subsystem} state edge {from}->{to} is not allowed"),
            ));
        }

        let current_state = match *subsystem {
            "mission" => &mut mission_state,
            "safety" => &mut safety_state,
            _ => continue,
        };
        if *from != current_state.as_str() {
            if semantic_detail.is_none() {
                semantic_detail = Some(format!(
                    "{subsystem} transition starts at {from}, expected {}",
                    current_state
                ));
            }
            continue;
        }
        *current_state = (*to).to_owned();
    }

    if mission_state != actual_mission_state.to_string() && semantic_detail.is_none() {
        semantic_detail = Some(format!(
            "mission transitions end at {mission_state}, observed {actual_mission_state}"
        ));
    }
    if safety_state != actual_safety_state.to_string() && semantic_detail.is_none() {
        semantic_detail = Some(format!(
            "safety transitions end at {safety_state}, observed {actual_safety_state}"
        ));
    }

    if let Some((sequence, detail)) = invalid_graph {
        Some(TransitionIssue {
            invalid_graph_sequence: Some(sequence),
            detail,
        })
    } else {
        semantic_detail.map(|detail| TransitionIssue {
            invalid_graph_sequence: None,
            detail,
        })
    }
}

fn safe_fallback_holds(
    required: bool,
    low_confidence: bool,
    safety_state: SafetyState,
    action: Action,
    truth_from: Point,
    truth_position: Point,
) -> bool {
    (!required || safety_state == SafetyState::Fallback)
        && (!(low_confidence || safety_state == SafetyState::Fallback)
            || (action.distance_mm == 0 && truth_from == truth_position))
}

fn verify_state_transitions(
    context: StateTransitionContext<'_>,
    tracker: &mut InvariantTracker,
    recorder: &mut Recorder,
) {
    let StateTransitionContext {
        tick,
        truth_position,
        observed,
        tick_result_sequence,
        mission_state,
        safety_state,
        transition_sequences,
    } = context;
    let mut steps = Vec::with_capacity(transition_sequences.len());
    let mut missing_transition_event = false;
    for sequence in transition_sequences {
        let Some(record) = recorder
            .events
            .iter()
            .find(|event| event.sequence == *sequence)
        else {
            missing_transition_event = true;
            continue;
        };
        if let EventPayload::Transition {
            subsystem,
            from,
            to,
            ..
        } = &record.event
        {
            steps.push((*sequence, subsystem.as_str(), from.as_str(), to.as_str()));
        } else {
            missing_transition_event = true;
        }
    }
    let issue = validate_transition_chain(
        tracker.previous_mission_state,
        tracker.previous_safety_state,
        mission_state,
        safety_state,
        &steps,
    )
    .or_else(|| {
        missing_transition_event.then(|| TransitionIssue {
            invalid_graph_sequence: None,
            detail: "a referenced transition event is missing or has the wrong kind".to_owned(),
        })
    });
    tracker.previous_mission_state = mission_state;
    tracker.previous_safety_state = safety_state;
    let passed = issue.is_none();
    let detail = issue.as_ref().map_or_else(
        || "mission and safety transitions match the state graphs and observed states".to_owned(),
        |value| value.detail.clone(),
    );
    let trigger_event_sequence = issue
        .as_ref()
        .and_then(|value| value.invalid_graph_sequence)
        .unwrap_or(tick_result_sequence);
    let trigger_event_kind = if issue
        .as_ref()
        .is_some_and(|value| value.invalid_graph_sequence.is_some())
    {
        "transition"
    } else {
        "tick_result"
    };
    record_invariant(
        "valid_state_transitions",
        passed,
        &detail,
        tick,
        truth_position,
        observed,
        trigger_event_sequence,
        trigger_event_kind,
        "mission Pending->Running->Completed; safety Nominal<->Fallback",
        format!(
            "transition_sequences={transition_sequences:?}; issue={}",
            issue.as_ref().map_or("none", |value| value.detail.as_str())
        ),
        tracker,
        recorder,
    );
}

#[allow(clippy::too_many_arguments)]
fn record_invariant(
    name: &str,
    passed: bool,
    detail: &str,
    tick: u32,
    truth_position: Point,
    observed: Option<&ObservedTelemetry>,
    trigger_event_sequence: u64,
    trigger_event_kind: &str,
    expected: &str,
    observed_result: String,
    tracker: &mut InvariantTracker,
    recorder: &mut Recorder,
) {
    recorder.push(
        tick,
        EventPayload::InvariantCheck {
            name: name.to_owned(),
            passed,
            detail: detail.to_owned(),
        },
    );
    if passed {
        return;
    }
    if let Some(failures) = tracker.failures.get_mut(name) {
        failures.push(FailureEvidence {
            tick,
            truth_position,
            observed: observed.cloned(),
            trigger_event_sequence,
            trigger_event_kind: trigger_event_kind.to_owned(),
            expected: expected.to_owned(),
            observed_result,
        });
    }
}

pub(crate) fn finalize_results(tracker: &InvariantTracker) -> (Vec<InvariantResult>, bool) {
    let results = tracker.report_results();
    let failed = results.iter().any(|result| !result.passed);
    (results, failed)
}

pub(crate) fn run_status(failed: bool) -> RunStatus {
    if failed {
        RunStatus::InvariantFailed
    } else {
        RunStatus::Passed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_transition_for_mission_state_change_is_detected() {
        let issue = validate_transition_chain(
            MissionState::Pending,
            SafetyState::Nominal,
            MissionState::Running,
            SafetyState::Nominal,
            &[],
        )
        .expect("missing state transition should fail");

        assert_eq!(issue.invalid_graph_sequence, None);
        assert!(issue.detail.contains("end at pending, observed running"));
    }

    #[test]
    fn allowed_graph_edge_with_wrong_previous_state_is_detected() {
        let transitions = [(7, "mission", "pending", "running")];
        let issue = validate_transition_chain(
            MissionState::Running,
            SafetyState::Nominal,
            MissionState::Running,
            SafetyState::Nominal,
            &transitions,
        )
        .expect("edge must start at the reconstructed previous state");

        assert_eq!(issue.invalid_graph_sequence, None);
        assert!(issue.detail.contains("starts at pending, expected running"));
    }

    #[test]
    fn transition_chain_must_end_at_observed_state() {
        let transitions = [(7, "mission", "pending", "running")];
        let issue = validate_transition_chain(
            MissionState::Pending,
            SafetyState::Nominal,
            MissionState::Completed,
            SafetyState::Nominal,
            &transitions,
        )
        .expect("end state must match the tick result");

        assert_eq!(issue.invalid_graph_sequence, None);
        assert!(issue.detail.contains("end at running, observed completed"));
    }

    #[test]
    fn mission_can_advance_through_two_allowed_edges_in_one_tick() {
        let transitions = [
            (7, "mission", "pending", "running"),
            (8, "mission", "running", "completed"),
        ];

        assert_eq!(
            validate_transition_chain(
                MissionState::Pending,
                SafetyState::Nominal,
                MissionState::Completed,
                SafetyState::Nominal,
                &transitions,
            ),
            None
        );
    }

    #[test]
    fn fallback_hold_cannot_hide_truth_motion() {
        let moved = safe_fallback_holds(
            true,
            false,
            SafetyState::Fallback,
            Action::hold(),
            Point { x_mm: 0, y_mm: 0 },
            Point { x_mm: 1, y_mm: 0 },
        );

        assert!(!moved);
    }

    #[test]
    fn low_confidence_hold_cannot_hide_truth_motion_before_fallback_deadline() {
        let moved = safe_fallback_holds(
            false,
            true,
            SafetyState::Nominal,
            Action::hold(),
            Point { x_mm: 0, y_mm: 0 },
            Point { x_mm: 1, y_mm: 0 },
        );

        assert!(!moved);
    }
}
