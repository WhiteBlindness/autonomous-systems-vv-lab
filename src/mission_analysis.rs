use serde::{Deserialize, Serialize};

use crate::error::LabError;
use crate::model::{MissionState, Point, RunReport, RunStatus, SCHEMA_VERSION, Scenario};

pub const MISSION_ANALYSIS_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_STALL_WINDOW_TICKS: u32 = 8;
pub const DEFAULT_MIN_PROGRESS_MM: u32 = 1;
const MAX_MISSION_ANALYSIS_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MissionPolicy {
    pub stall_window_ticks: u32,
    pub min_progress_mm: u32,
}

impl Default for MissionPolicy {
    fn default() -> Self {
        Self {
            stall_window_ticks: DEFAULT_STALL_WINDOW_TICKS,
            min_progress_mm: DEFAULT_MIN_PROGRESS_MM,
        }
    }
}

impl MissionPolicy {
    pub fn validate(self) -> Result<Self, LabError> {
        if !(1..=10_000).contains(&self.stall_window_ticks) {
            return Err(LabError(
                "stall_window_ticks must be between 1 and 10000".into(),
            ));
        }
        if !(1..=40_000_000).contains(&self.min_progress_mm) {
            return Err(LabError(
                "min_progress_mm must be between 1 and 40000000".into(),
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionOutcome {
    Completed,
    Incomplete,
    Stalled,
    InvalidTerminated,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionReason {
    ExpectedFaultHold,
    UnexpectedNoProgress,
    ObservationCompletionNotConfirmed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HoldReasonCounts {
    pub low_confidence: u32,
    pub fallback: u32,
    pub other: u32,
}

impl HoldReasonCounts {
    fn new() -> Self {
        Self {
            low_confidence: 0,
            fallback: 0,
            other: 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MissionAnalysis {
    pub schema_version: u32,
    pub source_final_hash: String,
    pub policy: MissionPolicy,
    pub outcome: MissionOutcome,
    pub reason: Option<MissionReason>,
    pub waypoint_count: usize,
    pub waypoints_completed: usize,
    pub waypoints_remaining: usize,
    pub controller_waypoint_index: usize,
    pub distance_to_next_waypoint_mm: Option<u64>,
    pub best_distance_to_next_waypoint_mm: Option<u64>,
    pub last_meaningful_progress_tick: Option<u32>,
    pub ticks_since_meaningful_progress: u32,
    pub stall_detected_tick: Option<u32>,
    pub fallback_ticks: u32,
    pub hold_reason_counts: HoldReasonCounts,
    pub completion_tick: Option<u32>,
    pub safety_status: RunStatus,
}

impl MissionAnalysis {
    pub fn validate(&self) -> Result<(), LabError> {
        if self.schema_version != MISSION_ANALYSIS_SCHEMA_VERSION {
            return Err(LabError(
                "unsupported mission analysis schema_version".into(),
            ));
        }
        self.policy.validate()?;
        if self.source_final_hash.len() != 64
            || !self
                .source_final_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(LabError(
                "source_final_hash must be a lowercase SHA-256 digest".into(),
            ));
        }
        if self.waypoints_completed > self.waypoint_count
            || self
                .waypoints_completed
                .checked_add(self.waypoints_remaining)
                != Some(self.waypoint_count)
            || self.controller_waypoint_index > self.waypoint_count
        {
            return Err(LabError("mission waypoint counts are inconsistent".into()));
        }
        let has_remaining_waypoints = self.waypoints_remaining > 0;
        if self.distance_to_next_waypoint_mm.is_some() != has_remaining_waypoints
            || self.best_distance_to_next_waypoint_mm.is_some() != has_remaining_waypoints
            || self
                .best_distance_to_next_waypoint_mm
                .zip(self.distance_to_next_waypoint_mm)
                .is_some_and(|(best, current)| best > current)
        {
            return Err(LabError(
                "mission waypoint distances are inconsistent".into(),
            ));
        }
        match self.outcome {
            MissionOutcome::Completed
                if self.completion_tick.is_none()
                    || self.reason.is_some()
                    || self.waypoints_remaining != 0 =>
            {
                Err(LabError(
                    "completed mission metadata is inconsistent".into(),
                ))
            }
            MissionOutcome::Incomplete
                if self.completion_tick.is_some() || self.reason.is_some() =>
            {
                Err(LabError(
                    "incomplete mission metadata is inconsistent".into(),
                ))
            }
            MissionOutcome::Stalled
                if self.completion_tick.is_some()
                    || self.waypoints_remaining == 0
                    || self.stall_detected_tick.is_none()
                    || !matches!(
                        self.reason,
                        Some(
                            MissionReason::ExpectedFaultHold | MissionReason::UnexpectedNoProgress
                        )
                    ) =>
            {
                Err(LabError("stalled mission metadata is inconsistent".into()))
            }
            MissionOutcome::InvalidTerminated
                if self.completion_tick.is_none()
                    || self.waypoints_remaining == 0
                    || self.reason != Some(MissionReason::ObservationCompletionNotConfirmed) =>
            {
                Err(LabError(
                    "invalid termination metadata is inconsistent".into(),
                ))
            }
            _ => Ok(()),
        }
    }
}

pub fn parse_mission_analysis(text: &str) -> Result<MissionAnalysis, LabError> {
    if text.len() > MAX_MISSION_ANALYSIS_BYTES {
        return Err(LabError(
            "mission analysis file exceeds the 1 MiB limit".into(),
        ));
    }
    let analysis: MissionAnalysis =
        serde_json::from_str(text).map_err(|error| LabError(error.to_string()))?;
    analysis.validate()?;
    Ok(analysis)
}

#[derive(Clone, Debug)]
struct ProgressTracker {
    waypoint_index: usize,
    best_distances: Vec<Option<i64>>,
    significance_anchors: Vec<Option<i64>>,
    last_meaningful_progress_tick: Option<u32>,
}

impl ProgressTracker {
    fn new(waypoint_count: usize) -> Self {
        Self {
            waypoint_index: 0,
            best_distances: vec![None; waypoint_count],
            significance_anchors: vec![None; waypoint_count],
            last_meaningful_progress_tick: None,
        }
    }

    fn sample_truth(
        &mut self,
        position: Point,
        tick: u32,
        scenario: &Scenario,
        min_progress_mm: u32,
    ) {
        while self.waypoint_index < scenario.mission.waypoints.len()
            && position.within_radius(
                scenario.mission.waypoints[self.waypoint_index],
                scenario.mission.arrival_radius_mm,
            )
        {
            self.waypoint_index += 1;
            self.last_meaningful_progress_tick = Some(tick);
        }
        let Some(target) = scenario.mission.waypoints.get(self.waypoint_index) else {
            return;
        };
        let distance = position.manhattan_distance(*target);
        let best_distance = &mut self.best_distances[self.waypoint_index];
        let anchor = &mut self.significance_anchors[self.waypoint_index];
        match *best_distance {
            None => {
                *best_distance = Some(distance);
                *anchor = Some(distance);
            }
            Some(best) if distance < best => {
                *best_distance = Some(distance);
                if anchor.is_some_and(|baseline| {
                    baseline.saturating_sub(distance) >= i64::from(min_progress_mm)
                }) {
                    *anchor = Some(distance);
                    self.last_meaningful_progress_tick = Some(tick);
                }
            }
            Some(_) => {}
        }
    }
}

/// Recalcula o progresso da missão a partir da trajetória real do relatório reproduzido.
pub fn analyze_mission(
    scenario: &Scenario,
    report: &RunReport,
    policy: MissionPolicy,
) -> Result<MissionAnalysis, LabError> {
    scenario
        .validate()
        .map_err(|error| LabError(error.to_string()))?;
    let policy = policy.validate()?;
    validate_report_shape(scenario, report)?;

    let mut progress = ProgressTracker::new(scenario.mission.waypoints.len());
    let mut hold_reason_counts = HoldReasonCounts::new();
    let mut expected_fault_hold_by_tick = Vec::with_capacity(report.telemetry.len());
    let mut fallback_ticks = 0_u32;
    let mut stall_detected_tick = None;
    let mut final_stall_reason = None;
    let mut completion_tick = None;
    let mut completion_confirmed = false;
    let mut was_controller_completed = false;
    let mut previous_truth = scenario.vehicle.start;

    progress.sample_truth(previous_truth, 0, scenario, policy.min_progress_mm);
    for telemetry in &report.telemetry {
        progress.sample_truth(
            previous_truth,
            telemetry.tick,
            scenario,
            policy.min_progress_mm,
        );
        progress.sample_truth(
            telemetry.truth_position,
            telemetry.tick,
            scenario,
            policy.min_progress_mm,
        );
        previous_truth = telemetry.truth_position;

        let low_confidence = telemetry.observed.as_ref().is_none_or(|sample| {
            !sample.fresh
                || sample.confidence_permille < scenario.safety.confidence_threshold_permille
        });
        let holding = telemetry.action.distance_mm == 0;
        let expected_fault_hold = holding
            && telemetry.mission_state != MissionState::Completed
            && (low_confidence || telemetry.safety_state == crate::model::SafetyState::Fallback);
        expected_fault_hold_by_tick.push(expected_fault_hold);
        if holding && telemetry.mission_state != MissionState::Completed {
            if low_confidence {
                hold_reason_counts.low_confidence =
                    hold_reason_counts.low_confidence.saturating_add(1);
            }
            if telemetry.safety_state == crate::model::SafetyState::Fallback {
                hold_reason_counts.fallback = hold_reason_counts.fallback.saturating_add(1);
            }
            if !low_confidence && telemetry.safety_state != crate::model::SafetyState::Fallback {
                hold_reason_counts.other = hold_reason_counts.other.saturating_add(1);
            }
        }
        if telemetry.safety_state == crate::model::SafetyState::Fallback {
            fallback_ticks = fallback_ticks.saturating_add(1);
        }

        if telemetry.mission_state == MissionState::Completed && !was_controller_completed {
            completion_tick = Some(telemetry.tick);
            completion_confirmed = progress.waypoint_index == scenario.mission.waypoints.len();
            was_controller_completed = true;
        }

        if !was_controller_completed && progress.waypoint_index < scenario.mission.waypoints.len() {
            let ticks_since = telemetry
                .tick
                .saturating_sub(progress.last_meaningful_progress_tick.unwrap_or(0));
            if ticks_since >= policy.stall_window_ticks {
                stall_detected_tick.get_or_insert(telemetry.tick);
                final_stall_reason = Some(classify_stall(
                    &expected_fault_hold_by_tick,
                    telemetry.tick,
                    policy.stall_window_ticks,
                ));
            } else {
                final_stall_reason = None;
            }
        } else {
            final_stall_reason = None;
        }
    }

    let last_telemetry = report
        .telemetry
        .last()
        .expect("report shape validation requires telemetry");
    let waypoints_completed = progress.waypoint_index;
    let waypoints_remaining = scenario.mission.waypoints.len() - waypoints_completed;
    let analysis_tick = completion_tick.unwrap_or(report.ticks);
    let ticks_since_meaningful_progress =
        analysis_tick.saturating_sub(progress.last_meaningful_progress_tick.unwrap_or(0));
    let (outcome, reason) = if completion_tick.is_some() {
        if completion_confirmed {
            (MissionOutcome::Completed, None)
        } else {
            (
                MissionOutcome::InvalidTerminated,
                Some(MissionReason::ObservationCompletionNotConfirmed),
            )
        }
    } else if waypoints_remaining == 0 {
        (MissionOutcome::Incomplete, None)
    } else if ticks_since_meaningful_progress >= policy.stall_window_ticks {
        (
            MissionOutcome::Stalled,
            Some(final_stall_reason.unwrap_or_else(|| {
                classify_stall(
                    &expected_fault_hold_by_tick,
                    report.ticks,
                    policy.stall_window_ticks,
                )
            })),
        )
    } else {
        (MissionOutcome::Incomplete, None)
    };
    let distance_to_next_waypoint_mm = scenario
        .mission
        .waypoints
        .get(waypoints_completed)
        .map(|target| last_telemetry.truth_position.manhattan_distance(*target) as u64);
    let best_distance_to_next_waypoint_mm = scenario
        .mission
        .waypoints
        .get(waypoints_completed)
        .and_then(|_| progress.best_distances[waypoints_completed])
        .map(|distance| distance as u64);

    Ok(MissionAnalysis {
        schema_version: MISSION_ANALYSIS_SCHEMA_VERSION,
        source_final_hash: report.final_hash.clone(),
        policy,
        outcome,
        reason,
        waypoint_count: scenario.mission.waypoints.len(),
        waypoints_completed,
        waypoints_remaining,
        controller_waypoint_index: last_telemetry.waypoint_index,
        distance_to_next_waypoint_mm,
        best_distance_to_next_waypoint_mm,
        last_meaningful_progress_tick: progress.last_meaningful_progress_tick,
        ticks_since_meaningful_progress,
        stall_detected_tick,
        fallback_ticks,
        hold_reason_counts,
        completion_tick,
        safety_status: report.status,
    })
}

fn validate_report_shape(scenario: &Scenario, report: &RunReport) -> Result<(), LabError> {
    if report.schema_version != SCHEMA_VERSION
        || report.scenario_name != scenario.name
        || report.tick_ms != scenario.tick_ms
        || report.ticks != scenario.steps
        || report.telemetry.len() != scenario.steps as usize
    {
        return Err(LabError(
            "run report does not match the mission scenario shape".into(),
        ));
    }
    for (index, telemetry) in report.telemetry.iter().enumerate() {
        if telemetry.tick != index as u32 + 1 {
            return Err(LabError(
                "run report telemetry ticks are not consecutive".into(),
            ));
        }
    }
    if report
        .telemetry
        .last()
        .is_none_or(|telemetry| telemetry.truth_position != report.final_truth)
    {
        return Err(LabError(
            "run report final truth does not match final telemetry".into(),
        ));
    }
    Ok(())
}

fn classify_stall(
    expected_fault_hold_by_tick: &[bool],
    tick: u32,
    window_ticks: u32,
) -> MissionReason {
    if tick < window_ticks {
        return MissionReason::UnexpectedNoProgress;
    }
    let first_tick = tick - window_ticks + 1;
    let start_index = first_tick.saturating_sub(1) as usize;
    let end_index = tick as usize;
    let window = expected_fault_hold_by_tick
        .get(start_index..end_index)
        .unwrap_or_default();
    if window.len() == window_ticks as usize
        && window.iter().all(|is_expected_hold| *is_expected_hold)
    {
        MissionReason::ExpectedFaultHold
    } else {
        MissionReason::UnexpectedNoProgress
    }
}
