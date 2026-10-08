use crate::model::{
    Action, Bounds, Heading, MissionConfig, MissionState, ObservedTelemetry, Point, SafetyState,
};

/// Entrada limitada do controlador, composta por observações e configuração.
///
/// O tipo não transporta a posição real do veículo nem o estado interno da execução.
///
/// ```compile_fail
/// use autonomous_systems_vv_lab::controller::ControllerInput;
/// fn read_truth(input: ControllerInput<'_>) -> i32 {
///     input.truth.x_mm
/// }
/// ```
pub struct ControllerInput<'a> {
    pub mission_state: MissionState,
    pub safety_state: SafetyState,
    pub observed: Option<&'a ObservedTelemetry>,
    pub target: Option<Point>,
    pub step_mm: i32,
    pub bounds: &'a Bounds,
    pub confidence_threshold_permille: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct ControllerState {
    pub(crate) mission_state: MissionState,
    pub(crate) safety_state: SafetyState,
    pub(crate) waypoint_index: usize,
    pub(crate) recovery_streak: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TransitionIntent {
    pub(crate) subsystem: String,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) reason: String,
}

impl ControllerState {
    pub(crate) fn new() -> Self {
        Self {
            mission_state: MissionState::Pending,
            safety_state: SafetyState::Nominal,
            waypoint_index: 0,
            recovery_streak: 0,
        }
    }
}

pub(crate) fn update_mission(
    mission: &MissionConfig,
    confidence_threshold_permille: u16,
    invalid_mutant: bool,
    observed: Option<&ObservedTelemetry>,
    state: &mut ControllerState,
) -> Vec<TransitionIntent> {
    let mut transitions = Vec::new();
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
        transitions.push(transition_intent(
            "mission",
            state.mission_state,
            next_state,
            reason,
        ));
        state.mission_state = next_state;
    }
    if state.mission_state != MissionState::Running {
        return transitions;
    }
    let Some(observation) = observed.filter(|sample| {
        sample.fresh && sample.confidence_permille >= confidence_threshold_permille
    }) else {
        return transitions;
    };
    let position = observation.position;
    while state.waypoint_index < mission.waypoints.len()
        && position.within_radius(
            mission.waypoints[state.waypoint_index],
            mission.arrival_radius_mm,
        )
    {
        state.waypoint_index += 1;
    }
    if state.waypoint_index == mission.waypoints.len() {
        transitions.push(transition_intent(
            "mission",
            MissionState::Running,
            MissionState::Completed,
            "all_waypoints_observed_within_arrival_radius",
        ));
        state.mission_state = MissionState::Completed;
    }
    transitions
}

pub(crate) fn update_safety(
    recovery_after_ticks: u32,
    low_confidence: bool,
    watchdog_required: bool,
    state: &mut ControllerState,
) -> Vec<TransitionIntent> {
    let mut transitions = Vec::new();
    state.recovery_streak = if low_confidence {
        0
    } else {
        state.recovery_streak.saturating_add(1)
    };
    match state.safety_state {
        SafetyState::Nominal if watchdog_required => {
            transitions.push(transition_intent(
                "safety",
                SafetyState::Nominal,
                SafetyState::Fallback,
                "low_confidence_watchdog",
            ));
            state.safety_state = SafetyState::Fallback;
        }
        SafetyState::Fallback
            if !low_confidence && state.recovery_streak >= recovery_after_ticks =>
        {
            transitions.push(transition_intent(
                "safety",
                SafetyState::Fallback,
                SafetyState::Nominal,
                "confidence_recovered_for_configured_interval",
            ));
            state.safety_state = SafetyState::Nominal;
        }
        _ => {}
    }
    transitions
}

/// Escolhe uma ação apenas com base na entrada observável do controlador.
pub fn choose_action(input: &ControllerInput<'_>) -> Action {
    if input.mission_state == MissionState::Completed || input.safety_state == SafetyState::Fallback
    {
        return Action::hold();
    }
    let Some(observed) = input.observed else {
        return Action::hold();
    };
    if !observed.fresh || observed.confidence_permille < input.confidence_threshold_permille {
        return Action::hold();
    }
    let Some(target) = input.target else {
        return Action::hold();
    };
    let Some(heading) = desired_heading(observed.position, target) else {
        return Action::hold();
    };
    let destination = observed.position.translated(heading, input.step_mm);
    if !crate::geometry::segment_stays_in_bounds(observed.position, destination, input.bounds) {
        return Action::hold();
    }
    Action::step(heading, input.step_mm)
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

fn transition_intent(
    subsystem: &str,
    from: impl ToString,
    to: impl ToString,
    reason: &str,
) -> TransitionIntent {
    TransitionIntent {
        subsystem: subsystem.to_owned(),
        from: from.to_string(),
        to: to.to_string(),
        reason: reason.to_owned(),
    }
}
