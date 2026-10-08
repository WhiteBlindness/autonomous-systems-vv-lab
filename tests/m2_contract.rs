use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use autonomous_systems_vv_lab::analyze_artifact;
use autonomous_systems_vv_lab::controller::{ControllerInput, choose_action};
use autonomous_systems_vv_lab::mission_analysis::{
    MissionOutcome, MissionPolicy, MissionReason, analyze_mission,
};
use autonomous_systems_vv_lab::model::{
    Action, Bounds, Heading, MissionState, ObservedTelemetry, Point, RunReport, RunStatus,
    SafetyState, Scenario, TickTelemetry, TickWindow, ValidationMutant,
};
use autonomous_systems_vv_lab::{parse_mission_analysis, parse_scenario, run_scenario};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let unique = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("vv-lab-m2-{label}-{}-{unique}", std::process::id()));
        fs::create_dir_all(&path).expect("create temporary directory");
        Self(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn basic_scenario() -> Scenario {
    parse_scenario(include_str!("../scenarios/basic-mission.json")).expect("valid fixture")
}

fn analyze(
    scenario: Scenario,
    policy: MissionPolicy,
) -> autonomous_systems_vv_lab::mission_analysis::MissionAnalysis {
    let artifact = run_scenario(scenario.clone(), 42).expect("run scenario");
    analyze_mission(&scenario, &artifact.expected_report, policy).expect("analyze mission")
}

#[test]
fn completed_requires_controller_completion_and_truth_reach() {
    let mut scenario = basic_scenario();
    scenario.steps = 2;
    scenario.mission.waypoints = vec![scenario.vehicle.start, scenario.vehicle.start];
    scenario.mission.arrival_radius_mm = 0;

    let analysis = analyze(scenario, MissionPolicy::default());

    assert_eq!(analysis.outcome, MissionOutcome::Completed);
    assert_eq!(analysis.waypoint_count, 2);
    assert_eq!(analysis.waypoints_completed, 2);
    assert_eq!(analysis.waypoints_remaining, 0);
    assert_eq!(analysis.controller_waypoint_index, 2);
    assert_eq!(analysis.completion_tick, Some(1));
    assert_eq!(analysis.safety_status, RunStatus::Passed);
}

#[test]
fn short_horizon_is_incomplete_before_stall_window() {
    let mut scenario = basic_scenario();
    scenario.steps = 1;
    scenario.sensors.gps.dropout_windows = vec![TickWindow {
        start_tick: 1,
        end_tick: 1,
    }];

    let analysis = analyze(scenario, MissionPolicy::default());

    assert_eq!(analysis.outcome, MissionOutcome::Incomplete);
    assert_eq!(analysis.waypoints_completed, 0);
    assert_eq!(analysis.waypoints_remaining, 2);
    assert_eq!(analysis.stall_detected_tick, None);
    assert_eq!(analysis.last_meaningful_progress_tick, None);
    assert_eq!(analysis.ticks_since_meaningful_progress, 1);
}

#[test]
fn sustained_low_confidence_holds_are_a_stall_with_separate_counts() {
    let mut scenario = basic_scenario();
    scenario.steps = 4;
    scenario.sensors.gps.dropout_windows = vec![TickWindow {
        start_tick: 1,
        end_tick: 4,
    }];
    let policy = MissionPolicy {
        stall_window_ticks: 2,
        min_progress_mm: 1,
    };

    let analysis = analyze(scenario, policy);

    assert_eq!(analysis.outcome, MissionOutcome::Stalled);
    assert_eq!(analysis.reason, Some(MissionReason::ExpectedFaultHold));
    assert_eq!(analysis.stall_detected_tick, Some(2));
    assert_eq!(analysis.hold_reason_counts.low_confidence, 4);
    assert_eq!(analysis.hold_reason_counts.fallback, 2);
    assert_eq!(analysis.fallback_ticks, 2);
}

#[test]
fn premature_observation_completion_is_invalid_termination() {
    let mut scenario = basic_scenario();
    scenario.steps = 1;
    scenario.mission.waypoints = vec![Point {
        x_mm: 9000,
        y_mm: 9000,
    }];
    let report = report_with_telemetry(
        &scenario,
        Point {
            x_mm: 1000,
            y_mm: 1000,
        },
        MissionState::Completed,
        1,
        Action::hold(),
    );

    let analysis = analyze_mission(&scenario, &report, MissionPolicy::default())
        .expect("analyze synthetic early completion");

    assert_eq!(analysis.outcome, MissionOutcome::InvalidTerminated);
    assert_eq!(
        analysis.reason,
        Some(MissionReason::ObservationCompletionNotConfirmed)
    );
    assert_eq!(analysis.waypoints_completed, 0);
    assert_eq!(analysis.controller_waypoint_index, 1);
    assert_eq!(analysis.completion_tick, Some(1));
}

#[test]
fn sub_threshold_distance_improvements_accumulate_per_waypoint() {
    let mut scenario = basic_scenario();
    scenario.steps = 4;
    scenario.vehicle.start = Point {
        x_mm: 1000,
        y_mm: 1000,
    };
    scenario.vehicle.step_mm = 1;
    scenario.mission.waypoints = vec![Point {
        x_mm: 1010,
        y_mm: 1000,
    }];
    scenario.mission.arrival_radius_mm = 0;
    let policy = MissionPolicy {
        stall_window_ticks: 8,
        min_progress_mm: 3,
    };

    let analysis = analyze(scenario, policy);

    assert_eq!(analysis.last_meaningful_progress_tick, Some(3));
    assert_eq!(analysis.ticks_since_meaningful_progress, 1);
    assert_eq!(analysis.distance_to_next_waypoint_mm, Some(6));
    assert_eq!(analysis.best_distance_to_next_waypoint_mm, Some(6));
}

#[test]
fn waypoint_arrival_uses_truth_samples_without_swept_segment_shortcuts() {
    let mut scenario = basic_scenario();
    scenario.steps = 1;
    scenario.vehicle.start = Point {
        x_mm: 1000,
        y_mm: 1000,
    };
    scenario.vehicle.step_mm = 2;
    scenario.mission.waypoints = vec![Point {
        x_mm: 1001,
        y_mm: 1000,
    }];
    scenario.mission.arrival_radius_mm = 0;

    let analysis = analyze(scenario, MissionPolicy::default());

    assert_eq!(analysis.waypoints_completed, 0);
    assert_eq!(analysis.waypoints_remaining, 1);
}

#[test]
fn safety_invariant_failure_is_preserved_in_mission_analysis() {
    let mut scenario = basic_scenario();
    scenario.steps = 4;
    scenario.sensors.gps.dropout_windows = vec![TickWindow {
        start_tick: 1,
        end_tick: 4,
    }];
    scenario
        .validation_mutants
        .push(ValidationMutant::DisableSafetyFallback);

    let analysis = analyze(
        scenario,
        MissionPolicy {
            stall_window_ticks: 2,
            min_progress_mm: 1,
        },
    );

    assert_eq!(analysis.safety_status, RunStatus::InvariantFailed);
}

#[test]
fn completed_mission_retains_unrelated_safety_invariant_failure() {
    let mut scenario = basic_scenario();
    scenario.restricted_zone.vertices = vec![
        Point {
            x_mm: 2000,
            y_mm: 900,
        },
        Point {
            x_mm: 2500,
            y_mm: 900,
        },
        Point {
            x_mm: 2500,
            y_mm: 1100,
        },
        Point {
            x_mm: 2000,
            y_mm: 1100,
        },
    ];

    let artifact = run_scenario(scenario.clone(), 42).expect("run crossing mission");
    let analysis = analyze_mission(
        &scenario,
        &artifact.expected_report,
        MissionPolicy::default(),
    )
    .expect("analyze crossing mission");

    assert_eq!(analysis.outcome, MissionOutcome::Completed);
    assert_eq!(analysis.waypoints_completed, 2);
    assert_eq!(analysis.safety_status, RunStatus::InvariantFailed);
}

#[test]
fn physically_reached_waypoints_with_controller_pending_are_incomplete() {
    let mut scenario = basic_scenario();
    scenario.steps = 1;
    scenario.mission.waypoints = vec![scenario.vehicle.start];
    scenario.mission.arrival_radius_mm = 0;
    scenario.sensors.gps.dropout_windows = vec![TickWindow {
        start_tick: 1,
        end_tick: 1,
    }];

    let analysis = analyze(scenario, MissionPolicy::default());

    assert_eq!(analysis.outcome, MissionOutcome::Incomplete);
    assert_eq!(analysis.waypoints_completed, 1);
    assert_eq!(analysis.waypoints_remaining, 0);
    assert_eq!(analysis.controller_waypoint_index, 0);
    assert_eq!(analysis.completion_tick, None);
}

#[test]
fn controller_input_carries_only_observation_and_configured_limits() {
    let observed = ObservedTelemetry {
        position: Point {
            x_mm: 100,
            y_mm: 100,
        },
        sample_tick: 1,
        age_ticks: 0,
        base_confidence_permille: 1000,
        confidence_permille: 1000,
        fresh: true,
    };
    let bounds = Bounds {
        min_x_mm: 0,
        max_x_mm: 1000,
        min_y_mm: 0,
        max_y_mm: 1000,
    };
    let input = ControllerInput {
        mission_state: MissionState::Running,
        safety_state: SafetyState::Nominal,
        observed: Some(&observed),
        target: Some(Point {
            x_mm: 200,
            y_mm: 100,
        }),
        step_mm: 10,
        bounds: &bounds,
        confidence_threshold_permille: 500,
    };

    assert_eq!(choose_action(&input), Action::step(Heading::East, 10));
}

#[test]
fn controller_module_has_no_engine_truth_or_recorder_dependencies() {
    let controller = include_str!("../src/controller.rs");

    for forbidden in ["crate::engine", "crate::artifact", "crate::verifier"] {
        assert!(
            !controller.contains(forbidden),
            "controller module must not depend on {forbidden}"
        );
    }
}

#[test]
fn analyze_artifact_replays_before_recomputing_the_sidecar() {
    let scenario = basic_scenario();
    let artifact = run_scenario(scenario.clone(), 42).expect("run scenario");
    let policy = MissionPolicy::default();
    let expected = analyze_mission(&scenario, &artifact.expected_report, policy)
        .expect("analyze original report");

    let recomputed = analyze_artifact(artifact, policy).expect("replay and analyze");

    assert_eq!(recomputed, expected);
}

#[test]
fn mission_policy_rejects_zero_and_out_of_range_values() {
    assert_eq!(MissionPolicy::default().stall_window_ticks, 8);
    assert_eq!(MissionPolicy::default().min_progress_mm, 1);
    for policy in [
        MissionPolicy {
            stall_window_ticks: 0,
            min_progress_mm: 1,
        },
        MissionPolicy {
            stall_window_ticks: 10_001,
            min_progress_mm: 1,
        },
        MissionPolicy {
            stall_window_ticks: 1,
            min_progress_mm: 0,
        },
        MissionPolicy {
            stall_window_ticks: 1,
            min_progress_mm: 40_000_001,
        },
    ] {
        assert!(policy.validate().is_err());
    }
}

#[test]
fn mission_metadata_rejects_unsupported_versions_unknown_fields_and_inconsistent_values() {
    let mut scenario = basic_scenario();
    scenario.steps = 2;
    scenario.mission.waypoints = vec![scenario.vehicle.start];
    let analysis = analyze(scenario, MissionPolicy::default());
    let serialized = serde_json::to_value(&analysis).expect("serialize analysis");

    let mut unsupported = serialized.clone();
    unsupported["schema_version"] = serde_json::json!(2);
    assert!(parse_mission_analysis(&unsupported.to_string()).is_err());

    let mut unknown = serialized.clone();
    unknown["unknown_metadata"] = serde_json::json!(true);
    assert!(parse_mission_analysis(&unknown.to_string()).is_err());

    let mut invalid_counts = serialized;
    invalid_counts["waypoints_remaining"] = serde_json::json!(1);
    assert!(parse_mission_analysis(&invalid_counts.to_string()).is_err());
    assert!(parse_mission_analysis("{").is_err());
}

#[test]
fn exact_output_limit_allows_run_and_one_byte_less_fails_before_writing() {
    let scratch = TempDirectory::new("output-limit");
    let scenario_path = scratch.0.join("scenario.json");
    fs::write(
        &scenario_path,
        include_str!("../scenarios/basic-mission.json"),
    )
    .expect("write scenario fixture");
    let baseline_dir = scratch.0.join("baseline");
    let baseline = Command::new(env!("CARGO_BIN_EXE_vv-lab"))
        .args([
            "run",
            scenario_path.to_str().unwrap(),
            "--seed",
            "42",
            "--output",
        ])
        .arg(&baseline_dir)
        .output()
        .expect("run baseline command");
    assert_eq!(baseline.status.code(), Some(0));
    let total_bytes = ["events.json", "report.json", "mission.json"]
        .iter()
        .map(|name| fs::metadata(baseline_dir.join(name)).unwrap().len())
        .sum::<u64>();

    let exact_dir = scratch.0.join("exact-limit");
    let exact_limit = total_bytes.to_string();
    let exact = Command::new(env!("CARGO_BIN_EXE_vv-lab"))
        .args([
            "run",
            scenario_path.to_str().unwrap(),
            "--seed",
            "42",
            "--output",
        ])
        .arg(&exact_dir)
        .args(["--max-output-bytes", exact_limit.as_str()])
        .output()
        .expect("run with exact output limit");
    assert_eq!(exact.status.code(), Some(0));

    let rejected_dir = scratch.0.join("rejected-limit");
    let rejected_limit = (total_bytes - 1).to_string();
    let rejected = Command::new(env!("CARGO_BIN_EXE_vv-lab"))
        .args([
            "run",
            scenario_path.to_str().unwrap(),
            "--seed",
            "42",
            "--output",
        ])
        .arg(&rejected_dir)
        .args(["--max-output-bytes", rejected_limit.as_str()])
        .output()
        .expect("run with too-small output limit");
    assert_eq!(rejected.status.code(), Some(1));
    assert!(!rejected_dir.exists());
}

#[test]
fn analyze_output_limit_fails_before_creating_output_parent() {
    let scratch = TempDirectory::new("analyze-limit");
    let scenario_path = scratch.0.join("scenario.json");
    fs::write(
        &scenario_path,
        include_str!("../scenarios/basic-mission.json"),
    )
    .expect("write scenario fixture");
    let output_dir = scratch.0.join("run");
    let run = Command::new(env!("CARGO_BIN_EXE_vv-lab"))
        .args([
            "run",
            scenario_path.to_str().unwrap(),
            "--seed",
            "42",
            "--output",
        ])
        .arg(&output_dir)
        .output()
        .expect("run scenario");
    assert_eq!(run.status.code(), Some(0));

    let analyze_parent = scratch.0.join("analysis-output");
    let analysis = Command::new(env!("CARGO_BIN_EXE_vv-lab"))
        .args(["analyze"])
        .arg(output_dir.join("events.json"))
        .args(["--output"])
        .arg(analyze_parent.join("mission.json"))
        .args(["--max-output-bytes", "1"])
        .output()
        .expect("analyze with too-small output limit");

    assert_eq!(analysis.status.code(), Some(1));
    assert!(!analyze_parent.exists());
}

fn report_with_telemetry(
    scenario: &Scenario,
    truth_position: Point,
    mission_state: MissionState,
    waypoint_index: usize,
    action: Action,
) -> RunReport {
    RunReport {
        schema_version: 1,
        scenario_name: scenario.name.clone(),
        seed: 42,
        tick_ms: scenario.tick_ms,
        status: RunStatus::Passed,
        ticks: 1,
        event_count: 0,
        final_hash: "synthetic-report-hash".to_owned(),
        final_truth: truth_position,
        final_observed: None,
        invariants: Vec::new(),
        transitions: Vec::new(),
        telemetry: vec![TickTelemetry {
            tick: 1,
            sim_time_ms: u64::from(scenario.tick_ms),
            truth_position,
            observed: None,
            observation_age_ticks: None,
            confidence_permille: 0,
            low_confidence_streak_ticks: 1,
            low_confidence_watchdog_required: false,
            heading: scenario.vehicle.heading,
            mission_state,
            safety_state: SafetyState::Nominal,
            waypoint_index,
            action,
        }],
    }
}
