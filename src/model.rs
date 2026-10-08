use serde::{Deserialize, Serialize};

use crate::geometry::{GeometryError, validate_simple_polygon};

pub const SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_TICK_MS: u32 = 100;
pub const DEFAULT_CONFIDENCE_THRESHOLD_PERMILLE: u16 = 500;
pub const DEFAULT_FALLBACK_AFTER_TICKS: u32 = 3;
pub const DEFAULT_RECOVERY_AFTER_TICKS: u32 = 2;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x_mm: i32,
    pub y_mm: i32,
}

impl Point {
    pub fn translated(self, heading: Heading, distance_mm: i32) -> Self {
        let (dx, dy) = heading.delta();
        Self {
            x_mm: self.x_mm + dx * distance_mm,
            y_mm: self.y_mm + dy * distance_mm,
        }
    }

    pub fn manhattan_distance(self, other: Self) -> i64 {
        i64::from((self.x_mm - other.x_mm).abs()) + i64::from((self.y_mm - other.y_mm).abs())
    }

    pub fn within_radius(self, other: Self, radius_mm: i32) -> bool {
        self.manhattan_distance(other) <= i64::from(radius_mm)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Heading {
    North,
    East,
    South,
    West,
}

impl Heading {
    pub fn delta(self) -> (i32, i32) {
        match self {
            Self::North => (0, 1),
            Self::East => (1, 0),
            Self::South => (0, -1),
            Self::West => (-1, 0),
        }
    }

    pub fn all() -> [Self; 4] {
        [Self::North, Self::East, Self::South, Self::West]
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionState {
    Pending,
    Running,
    Completed,
}

impl std::fmt::Display for MissionState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyState {
    Nominal,
    Fallback,
}

impl std::fmt::Display for SafetyState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Nominal => "nominal",
            Self::Fallback => "fallback",
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationMutant {
    DisableSafetyFallback,
    InvalidMissionTransition,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    pub min_x_mm: i32,
    pub max_x_mm: i32,
    pub min_y_mm: i32,
    pub max_y_mm: i32,
}

impl Bounds {
    pub fn contains(&self, point: Point) -> bool {
        point.x_mm >= self.min_x_mm
            && point.x_mm <= self.max_x_mm
            && point.y_mm >= self.min_y_mm
            && point.y_mm <= self.max_y_mm
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RestrictedZone {
    pub vertices: Vec<Point>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VehicleConfig {
    pub start: Point,
    pub heading: Heading,
    pub step_mm: i32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MissionConfig {
    pub waypoints: Vec<Point>,
    pub arrival_radius_mm: i32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TickWindow {
    pub start_tick: u32,
    pub end_tick: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GpsConfig {
    #[serde(default)]
    pub dropout_windows: Vec<TickWindow>,
    pub dropout_permille: u16,
    pub noise_max_mm: i32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommunicationConfig {
    #[serde(default)]
    pub packet_loss_windows: Vec<TickWindow>,
    pub packet_loss_permille: u16,
    pub delay_min_ticks: u32,
    pub delay_max_ticks: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SensorConfig {
    pub gps: GpsConfig,
    pub communication: CommunicationConfig,
    pub max_observation_age_ticks: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyConfig {
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold_permille: u16,
    #[serde(default = "default_fallback_after")]
    pub fallback_after_ticks: u32,
    #[serde(default = "default_recovery_after")]
    pub recovery_after_ticks: u32,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            confidence_threshold_permille: DEFAULT_CONFIDENCE_THRESHOLD_PERMILLE,
            fallback_after_ticks: DEFAULT_FALLBACK_AFTER_TICKS,
            recovery_after_ticks: DEFAULT_RECOVERY_AFTER_TICKS,
        }
    }
}

fn default_confidence_threshold() -> u16 {
    DEFAULT_CONFIDENCE_THRESHOLD_PERMILLE
}

fn default_fallback_after() -> u32 {
    DEFAULT_FALLBACK_AFTER_TICKS
}

fn default_recovery_after() -> u32 {
    DEFAULT_RECOVERY_AFTER_TICKS
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub schema_version: u32,
    pub name: String,
    pub steps: u32,
    #[serde(default = "default_tick_ms")]
    pub tick_ms: u32,
    pub bounds: Bounds,
    pub restricted_zone: RestrictedZone,
    pub vehicle: VehicleConfig,
    pub mission: MissionConfig,
    pub sensors: SensorConfig,
    #[serde(default)]
    pub safety: SafetyConfig,
    #[serde(default)]
    pub validation_mutants: Vec<ValidationMutant>,
}

fn default_tick_ms() -> u32 {
    DEFAULT_TICK_MS
}

impl Scenario {
    pub fn from_json(text: &str) -> Result<Self, String> {
        let scenario: Self = serde_json::from_str(text).map_err(|error| error.to_string())?;
        scenario.validate().map_err(|error| error.to_string())?;
        Ok(scenario)
    }

    pub fn validate(&self) -> Result<(), ScenarioError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ScenarioError::Invalid("unsupported schema_version".into()));
        }
        if self.name.trim().is_empty() || self.name.len() > 100 {
            return Err(ScenarioError::Invalid(
                "name must contain 1 to 100 bytes".into(),
            ));
        }
        if !(1..=10_000).contains(&self.steps) {
            return Err(ScenarioError::Invalid(
                "steps must be between 1 and 10000".into(),
            ));
        }
        if !(1..=60_000).contains(&self.tick_ms) {
            return Err(ScenarioError::Invalid(
                "tick_ms must be between 1 and 60000".into(),
            ));
        }
        validate_bounds(&self.bounds)?;
        if !self.bounds.contains(self.vehicle.start) {
            return Err(ScenarioError::Invalid(
                "vehicle.start must be inside bounds".into(),
            ));
        }
        if !(1..=100_000).contains(&self.vehicle.step_mm) {
            return Err(ScenarioError::Invalid(
                "vehicle.step_mm must be between 1 and 100000".into(),
            ));
        }
        if self.mission.waypoints.is_empty() || self.mission.waypoints.len() > 128 {
            return Err(ScenarioError::Invalid(
                "mission must contain 1 to 128 waypoints".into(),
            ));
        }
        if !(0..=100_000).contains(&self.mission.arrival_radius_mm) {
            return Err(ScenarioError::Invalid(
                "arrival_radius_mm must be between 0 and 100000".into(),
            ));
        }
        for waypoint in &self.mission.waypoints {
            if !self.bounds.contains(*waypoint) {
                return Err(ScenarioError::Invalid(
                    "all waypoints must be inside bounds".into(),
                ));
            }
        }
        if self.restricted_zone.vertices.len() > 128 {
            return Err(ScenarioError::Invalid(
                "restricted_zone supports at most 128 vertices".into(),
            ));
        }
        for vertex in &self.restricted_zone.vertices {
            if !self.bounds.contains(*vertex) {
                return Err(ScenarioError::Invalid(
                    "restricted zone vertices must be inside bounds".into(),
                ));
            }
        }
        validate_simple_polygon(&self.restricted_zone.vertices).map_err(ScenarioError::Geometry)?;
        self.validate_sensors()?;
        self.validate_safety()?;
        if self.validation_mutants.len() > 2 {
            return Err(ScenarioError::Invalid(
                "at most two validation_mutants are supported".into(),
            ));
        }
        Ok(())
    }

    fn validate_sensors(&self) -> Result<(), ScenarioError> {
        if self.sensors.gps.dropout_permille > 1000 {
            return Err(ScenarioError::Invalid(
                "gps.dropout_permille cannot exceed 1000".into(),
            ));
        }
        if !(0..=100_000).contains(&self.sensors.gps.noise_max_mm) {
            return Err(ScenarioError::Invalid(
                "gps.noise_max_mm must be between 0 and 100000".into(),
            ));
        }
        if self.sensors.communication.packet_loss_permille > 1000 {
            return Err(ScenarioError::Invalid(
                "packet_loss_permille cannot exceed 1000".into(),
            ));
        }
        if self.sensors.communication.delay_min_ticks > self.sensors.communication.delay_max_ticks
            || self.sensors.communication.delay_max_ticks > 10_000
        {
            return Err(ScenarioError::Invalid(
                "communication delay range is invalid".into(),
            ));
        }
        if self.sensors.max_observation_age_ticks > 10_000 {
            return Err(ScenarioError::Invalid(
                "max_observation_age_ticks cannot exceed 10000".into(),
            ));
        }
        validate_windows(&self.sensors.gps.dropout_windows, self.steps)?;
        validate_windows(&self.sensors.communication.packet_loss_windows, self.steps)?;
        Ok(())
    }

    fn validate_safety(&self) -> Result<(), ScenarioError> {
        if self.safety.confidence_threshold_permille > 1000 {
            return Err(ScenarioError::Invalid(
                "confidence_threshold_permille cannot exceed 1000".into(),
            ));
        }
        if !(1..=10_000).contains(&self.safety.fallback_after_ticks) {
            return Err(ScenarioError::Invalid(
                "fallback_after_ticks must be between 1 and 10000".into(),
            ));
        }
        if !(1..=10_000).contains(&self.safety.recovery_after_ticks) {
            return Err(ScenarioError::Invalid(
                "recovery_after_ticks must be between 1 and 10000".into(),
            ));
        }
        Ok(())
    }
}

fn validate_bounds(bounds: &Bounds) -> Result<(), ScenarioError> {
    let coordinates = [
        bounds.min_x_mm,
        bounds.max_x_mm,
        bounds.min_y_mm,
        bounds.max_y_mm,
    ];
    if coordinates
        .iter()
        .any(|coordinate| coordinate.unsigned_abs() > 10_000_000)
    {
        return Err(ScenarioError::Invalid(
            "bounds coordinates must be within 10000000 mm of zero".into(),
        ));
    }
    if bounds.min_x_mm >= bounds.max_x_mm || bounds.min_y_mm >= bounds.max_y_mm {
        return Err(ScenarioError::Invalid(
            "bounds minimums must be less than maximums".into(),
        ));
    }
    Ok(())
}

fn validate_windows(windows: &[TickWindow], steps: u32) -> Result<(), ScenarioError> {
    if windows.len() > 256 {
        return Err(ScenarioError::Invalid(
            "at most 256 fault windows are supported per sensor".into(),
        ));
    }
    for window in windows {
        if window.start_tick == 0 || window.start_tick > window.end_tick || window.end_tick > steps
        {
            return Err(ScenarioError::Invalid(
                "fault windows must be inclusive ranges inside 1..=steps".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScenarioError {
    Invalid(String),
    Geometry(GeometryError),
}

impl std::fmt::Display for ScenarioError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::Geometry(error) => write!(formatter, "restricted_zone: {error}"),
        }
    }
}

impl std::error::Error for ScenarioError {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedTelemetry {
    pub position: Point,
    pub sample_tick: u32,
    pub age_ticks: u32,
    pub base_confidence_permille: u16,
    pub confidence_permille: u16,
    pub fresh: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub heading: Option<Heading>,
    pub distance_mm: i32,
}

impl Action {
    pub fn hold() -> Self {
        Self {
            heading: None,
            distance_mm: 0,
        }
    }

    pub fn step(heading: Heading, distance_mm: i32) -> Self {
        Self {
            heading: Some(heading),
            distance_mm,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TickTelemetry {
    pub tick: u32,
    pub sim_time_ms: u64,
    pub truth_position: Point,
    pub observed: Option<ObservedTelemetry>,
    pub observation_age_ticks: Option<u32>,
    pub confidence_permille: u16,
    pub low_confidence_streak_ticks: u32,
    pub low_confidence_watchdog_required: bool,
    pub heading: Heading,
    pub mission_state: MissionState,
    pub safety_state: SafetyState,
    pub waypoint_index: usize,
    pub action: Action,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionReport {
    pub tick: u32,
    pub subsystem: String,
    pub from: String,
    pub to: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FailureEvidence {
    pub tick: u32,
    pub truth_position: Point,
    pub observed: Option<ObservedTelemetry>,
    pub trigger_event_sequence: u64,
    pub trigger_event_kind: String,
    pub expected: String,
    pub observed_result: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvariantResult {
    pub name: String,
    pub passed: bool,
    pub failures: Vec<FailureEvidence>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Passed,
    InvariantFailed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunReport {
    pub schema_version: u32,
    pub scenario_name: String,
    pub seed: u64,
    pub tick_ms: u32,
    pub status: RunStatus,
    pub ticks: u32,
    pub event_count: u64,
    pub final_hash: String,
    pub final_truth: Point,
    pub final_observed: Option<ObservedTelemetry>,
    pub invariants: Vec<InvariantResult>,
    pub transitions: Vec<TransitionReport>,
    pub telemetry: Vec<TickTelemetry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventRecord {
    pub sequence: u64,
    pub tick: u32,
    pub event: EventPayload,
    pub previous_hash: String,
    pub hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventPayload {
    GpsSample {
        packet_sequence: u64,
        truth_at_sample: Point,
        observation: Option<Point>,
        dropped: bool,
        noise_x_mm: i32,
        noise_y_mm: i32,
        confidence_permille: u16,
    },
    PacketOutcome {
        packet_sequence: u64,
        delivered: bool,
        delay_ticks: u32,
        due_tick: Option<u32>,
    },
    SensorDelivery {
        packet_sequence: u64,
        sample_tick: u32,
        position: Point,
        confidence_permille: u16,
        accepted: bool,
        rejection_reason: Option<String>,
    },
    Transition {
        subsystem: String,
        from: String,
        to: String,
        reason: String,
    },
    TickResult {
        truth_from: Point,
        truth_position: Point,
        observed: Option<ObservedTelemetry>,
        heading: Heading,
        mission_state: MissionState,
        safety_state: SafetyState,
        waypoint_index: usize,
        action: Action,
    },
    InvariantCheck {
        name: String,
        passed: bool,
        detail: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventsArtifact {
    pub schema_version: u32,
    pub scenario: Scenario,
    pub seed: u64,
    pub events: Vec<EventRecord>,
    pub event_count: u64,
    pub final_hash: String,
    pub expected_report: RunReport,
    pub artifact_sha256: String,
}

pub fn transition_is_valid(subsystem: &str, from: &str, to: &str) -> bool {
    match subsystem {
        "mission" => matches!(
            (from, to),
            ("pending", "running") | ("running", "completed")
        ),
        "safety" => matches!(
            (from, to),
            ("nominal", "fallback") | ("fallback", "nominal")
        ),
        _ => false,
    }
}

pub const INVARIANT_NAMES: [&str; 4] = [
    "restricted_zone",
    "world_bounds",
    "safe_fallback",
    "valid_state_transitions",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_graph_contains_only_documented_edges() {
        assert!(transition_is_valid("mission", "pending", "running"));
        assert!(transition_is_valid("mission", "running", "completed"));
        assert!(!transition_is_valid("mission", "pending", "completed"));
        assert!(transition_is_valid("safety", "nominal", "fallback"));
        assert!(transition_is_valid("safety", "fallback", "nominal"));
        assert!(!transition_is_valid("safety", "nominal", "nominal"));
    }
}
