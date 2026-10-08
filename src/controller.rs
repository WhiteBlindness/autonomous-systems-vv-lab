use crate::artifact::Recorder;
use crate::engine::RunState;
use crate::model::{
    Action, EventPayload, Heading, MissionState, ObservedTelemetry, Point, SafetyState, Scenario,
    TransitionReport, ValidationMutant,
};
use crate::verifier::InvariantTracker;

#[derive(Clone, Debug)]
pub(crate) struct ControlContext {
    pub(crate) desired_heading: Option<Heading>,
    pub(crate) next_within_bounds: bool,
}

pub(crate) fn update_mission(
    scenario: &Scenario,
    tick: u32,
    observed: Option<&ObservedTelemetry>,
    state: &mut RunState,
    recorder: &mut Recorder,
    tracker: &mut InvariantTracker,
    transition_sequences: &mut Vec<u64>,
) {
    let invalid_mutant = scenario
        .validation_mutants
        .contains(&ValidationMutant::InvalidMissionTransition);
    if state.mission_state == MissionState::Pending {
        let next_state = if invalid_mutant {
            MissionState::Completed
        } else {
            MissionState::Running
        };
        let reason = if invalid_mutant {
            "validation_mutant_invalid_mission_transition"
        } else {
            "mission_started"
        };
        transition_sequences.push(record_transition(
            recorder,
            tracker,
            tick,
            "mission",
            state.mission_state,
            next_state,
            reason,
        ));
        state.mission_state = next_state;
    }
    if state.mission_state != MissionState::Running {
        return;
    }
    let Some(observation) = observed.filter(|sample| {
        sample.fresh && sample.confidence_permille >= scenario.safety.confidence_threshold_permille
    }) else {
        return;
    };
    let position = observation.position;
    while state.waypoint_index < scenario.mission.waypoints.len()
        && position.within_radius(
            scenario.mission.waypoints[state.waypoint_index],
            scenario.mission.arrival_radius_mm,
        )
    {
        state.waypoint_index += 1;
    }
    if state.waypoint_index == scenario.mission.waypoints.len() {
        transition_sequences.push(record_transition(
            recorder,
            tracker,
            tick,
            "mission",
            MissionState::Running,
            MissionState::Completed,
            "all_waypoints_observed_within_arrival_radius",
        ));
        state.mission_state = MissionState::Completed;
    }
}

pub(crate) fn control_context(
    scenario: &Scenario,
    mission_state: MissionState,
    waypoint_index: usize,
    observed: Option<&ObservedTelemetry>,
) -> ControlContext {
    if mission_state == MissionState::Completed
        || waypoint_index >= scenario.mission.waypoints.len()
    {
        return ControlContext {
            desired_heading: None,
            next_within_bounds: true,
        };
    }
    let Some(observed) = observed else {
        return ControlContext {
            desired_heading: None,
            next_within_bounds: false,
        };
    };
    let desired = desired_heading(
        observed.position,
        scenario.mission.waypoints[waypoint_index],
    );
    let Some(heading) = desired else {
        return ControlContext {
            desired_heading: None,
            next_within_bounds: true,
        };
    };
    let destination = observed
        .position
        .translated(heading, scenario.vehicle.step_mm);
    ControlContext {
        desired_heading: Some(heading),
        next_within_bounds: crate::geometry::segment_stays_in_bounds(
            observed.position,
            destination,
            &scenario.bounds,
        ),
    }
}

fn desired_heading(position: Point, target: Point) -> Option<Heading> {
    let delta_x = target.x_mm - position.x_mm;
    let delta_y = target.y_mm - position.y_mm;
    if delta_x == 0 && delta_y == 0 {
        return None;
    }
    if delta_x.abs() >= delta_y.abs() && delta_x != 0 {
        Some(if delta_x > 0 {
            Heading::East
        } else {
            Heading::West
        })
    } else if delta_y > 0 {
        Some(Heading::North)
    } else {
        Some(Heading::South)
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn update_safety(
    scenario: &Scenario,
    tick: u32,
    low_confidence: bool,
    watchdog_required: bool,
    state: &mut RunState,
    recorder: &mut Recorder,
    tracker: &mut InvariantTracker,
    transition_sequences: &mut Vec<u64>,
) {
    state.recovery_streak = if low_confidence {
        0
    } else {
        state.recovery_streak.saturating_add(1)
    };
    match state.safety_state {
        SafetyState::Nominal if watchdog_required => {
            transition_sequences.push(record_transition(
                recorder,
                tracker,
                tick,
                "safety",
                SafetyState::Nominal,
                SafetyState::Fallback,
                "low_confidence_watchdog",
            ));
            state.safety_state = SafetyState::Fallback;
        }
        SafetyState::Fallback
            if !low_confidence && state.recovery_streak >= scenario.safety.recovery_after_ticks =>
        {
            transition_sequences.push(record_transition(
                recorder,
                tracker,
                tick,
                "safety",
                SafetyState::Fallback,
                SafetyState::Nominal,
                "confidence_recovered_for_configured_interval",
            ));
            state.safety_state = SafetyState::Nominal;
        }
        _ => {}
    }
}

pub(crate) fn choose_action(
    scenario: &Scenario,
    state: &RunState,
    observed: Option<&ObservedTelemetry>,
    control: &ControlContext,
) -> Action {
    if state.mission_state == MissionState::Completed || state.safety_state == SafetyState::Fallback
    {
        return Action::hold();
    }
    let Some(observed) = observed else {
        return Action::hold();
    };
    if !observed.fresh
        || observed.confidence_permille < scenario.safety.confidence_threshold_permille
    {
        return Action::hold();
    }
    let Some(heading) = control.desired_heading else {
        return Action::hold();
    };
    if !control.next_within_bounds {
        return Action::hold();
    }
    Action::step(heading, scenario.vehicle.step_mm)
}

fn record_transition(
    recorder: &mut Recorder,
    tracker: &mut InvariantTracker,
    tick: u32,
    subsystem: &str,
    from: impl ToString,
    to: impl ToString,
    reason: &str,
) -> u64 {
    let from = from.to_string();
    let to = to.to_string();
    let reason = reason.to_owned();
    let sequence = recorder.push(
        tick,
        EventPayload::Transition {
            subsystem: subsystem.to_owned(),
            from: from.clone(),
            to: to.clone(),
            reason: reason.clone(),
        },
    );
    tracker.transitions.push(TransitionReport {
        tick,
        subsystem: subsystem.to_owned(),
        from,
        to,
        reason,
    });
    sequence
}
