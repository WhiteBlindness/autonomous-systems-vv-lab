use autonomous_systems_vv_lab::model::{EventPayload, EventsArtifact, Point};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let unique = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("vv-lab-{label}-{}-{unique}", std::process::id()));
        fs::create_dir(&path).expect("create isolated test directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Run {
    scratch: Scratch,
    output: Output,
}

impl Run {
    fn output_dir(&self) -> PathBuf {
        self.scratch.0.join("run")
    }

    fn artifact_path(&self) -> PathBuf {
        self.output_dir().join("events.json")
    }

    fn report(&self) -> Value {
        read_json(&self.output_dir().join("report.json"))
    }

    fn artifact(&self) -> Value {
        read_json(&self.artifact_path())
    }
}

fn fixture(path: &str) -> Value {
    serde_json::from_str(match path {
        "basic-mission" => include_str!("../scenarios/basic-mission.json"),
        "gps-dropout" => include_str!("../scenarios/gps-dropout.json"),
        "communication-fault" => include_str!("../scenarios/communication-fault.json"),
        "restricted-zone-failure" => {
            include_str!("../scenarios/restricted-zone-failure.json")
        }
        "restricted-zone-boundary" => {
            include_str!("../scenarios/restricted-zone-boundary.json")
        }
        "safe-fallback-failure" => {
            include_str!("../scenarios/safe-fallback-failure.json")
        }
        "invalid-transition" => include_str!("../scenarios/invalid-transition.json"),
        "world-bounds-failure" => include_str!("../scenarios/world-bounds-failure.json"),
        _ => panic!("unknown fixture: {path}"),
    })
    .expect("fixture is valid JSON")
}

fn run_scenario(scenario: &Value, seed: u64, label: &str) -> Run {
    let scratch = Scratch::new(label);
    let scenario_path = scratch.0.join("scenario.json");
    let output_dir = scratch.0.join("run");
    fs::write(
        &scenario_path,
        serde_json::to_vec_pretty(scenario).expect("serialize scenario"),
    )
    .expect("write scenario");
    let output = Command::new(env!("CARGO_BIN_EXE_vv-lab"))
        .args(["run", scenario_path.to_str().unwrap(), "--seed"])
        .arg(seed.to_string())
        .arg("--output")
        .arg(&output_dir)
        .output()
        .expect("run vv-lab");
    Run { scratch, output }
}

fn replay(path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vv-lab"))
        .args(["replay"])
        .arg(path)
        .output()
        .expect("replay vv-lab artifact")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read JSON artifact"))
        .expect("parse JSON artifact")
}

fn write_json(path: &Path, value: &Value) {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("serialize JSON"),
    )
    .expect("write JSON");
}

fn invariant<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["invariants"]
        .as_array()
        .expect("report invariants")
        .iter()
        .find(|item| item["name"] == name)
        .unwrap_or_else(|| panic!("missing invariant {name}"))
}

fn assert_failure_event_evidence(run: &Run, invariant_name: &str, expected_kind: &str) {
    let report = run.report();
    let artifact = run.artifact();
    let events = artifact["events"].as_array().expect("artifact events");
    let failures = invariant(&report, invariant_name)["failures"]
        .as_array()
        .expect("invariant failures");
    assert!(!failures.is_empty(), "expected {invariant_name} evidence");

    for failure in failures {
        assert_eq!(failure["trigger_event_kind"], expected_kind);
        let sequence = failure["trigger_event_sequence"]
            .as_u64()
            .expect("failure trigger sequence");
        let event = events
            .iter()
            .find(|record| record["sequence"] == sequence)
            .unwrap_or_else(|| panic!("missing event sequence {sequence}"));
        assert_eq!(event["tick"], failure["tick"]);
        assert_eq!(event["event"]["kind"], expected_kind);
        if invariant_name == "safe_fallback" {
            assert!(event["event"]["safety_state"].is_string());
            assert!(event["event"]["action"]["distance_mm"].is_number());
        }
    }
}

#[test]
fn identical_seed_has_identical_canonical_artifacts_and_replays() {
    let scenario = fixture("basic-mission");
    let first = run_scenario(&scenario, 42, "same-seed-a");
    let second = run_scenario(&scenario, 42, "same-seed-b");

    assert_eq!(first.output.status.code(), Some(0));
    assert_eq!(second.output.status.code(), Some(0));
    assert_eq!(
        fs::read(first.artifact_path()).unwrap(),
        fs::read(second.artifact_path()).unwrap()
    );
    assert_eq!(
        fs::read(first.output_dir().join("report.json")).unwrap(),
        fs::read(second.output_dir().join("report.json")).unwrap()
    );
    assert_eq!(replay(&first.artifact_path()).status.code(), Some(0));
}

#[test]
fn different_seeds_change_the_faulted_trajectory() {
    let scenario = fixture("gps-dropout");
    let first = run_scenario(&scenario, 42, "different-seed-a");
    let second = run_scenario(&scenario, 43, "different-seed-b");

    assert_eq!(first.output.status.code(), Some(0));
    assert_eq!(second.output.status.code(), Some(0));
    assert_ne!(
        first.report()["telemetry"],
        second.report()["telemetry"],
        "seeded GPS noise should change the observed trajectory"
    );
}

#[test]
fn gps_dropout_is_visible_and_recovery_restores_fresh_observations() {
    let run = run_scenario(&fixture("gps-dropout"), 42, "gps-dropout");
    assert_eq!(run.output.status.code(), Some(0));

    let events = run.artifact()["events"].as_array().unwrap().clone();
    let gps_samples: Vec<&Value> = events
        .iter()
        .filter(|record| record["event"]["kind"] == "gps_sample")
        .collect();
    assert!(gps_samples.iter().any(|record| record["tick"] == 3));
    assert!((4..=7).all(|tick| {
        gps_samples.iter().any(|record| {
            record["tick"] == tick
                && record["event"]["dropped"] == true
                && record["event"]["observation"] == Value::Null
        })
    }));
    assert!(
        gps_samples
            .iter()
            .any(|record| { record["tick"] == 8 && record["event"]["dropped"] == false })
    );

    let telemetry = run.report()["telemetry"].as_array().unwrap().clone();
    let during_dropout = telemetry
        .iter()
        .find(|entry| entry["tick"] == 6)
        .expect("telemetry at dropout tick");
    let recovered = telemetry
        .iter()
        .find(|entry| entry["tick"] == 8)
        .expect("telemetry after recovery");
    assert!(during_dropout["observation_age_ticks"].as_u64().unwrap() > 0);
    assert!(during_dropout["confidence_permille"].as_u64().unwrap() < 1000);
    assert_eq!(recovered["observation_age_ticks"], 0);
    assert_eq!(recovered["sim_time_ms"], 800);
    let recovery_sample = gps_samples
        .iter()
        .find(|record| record["tick"] == 8)
        .unwrap();
    assert_eq!(
        recovered["observed"]["position"],
        recovery_sample["event"]["observation"]
    );
    assert_ne!(
        recovery_sample["event"]["truth_at_sample"], recovery_sample["event"]["observation"],
        "configured GPS noise should affect accepted observations"
    );
}

#[test]
fn communication_faults_record_loss_delay_and_stable_event_order() {
    let run = run_scenario(&fixture("communication-fault"), 42, "communication-fault");
    assert_eq!(run.output.status.code(), Some(0));

    let artifact = run.artifact();
    let events = artifact["events"].as_array().unwrap();
    let mut packet_ticks = Vec::new();
    let mut due_by_packet = std::collections::BTreeMap::new();
    for (next_sequence, record) in (1_u64..).zip(events.iter()) {
        assert_eq!(record["sequence"], next_sequence);
        if record["event"]["kind"] == "packet_outcome" {
            packet_ticks.push(record["tick"].as_u64().unwrap());
            let event = &record["event"];
            let packet_sequence = event["packet_sequence"].as_u64().unwrap();
            if (3..=5).contains(&record["tick"].as_u64().unwrap()) {
                assert_eq!(event["delivered"], false);
                assert_eq!(event["due_tick"], Value::Null);
                due_by_packet.insert(packet_sequence, None);
            } else {
                assert_eq!(event["delivered"], true);
                assert!(event["due_tick"].as_u64().unwrap() >= record["tick"].as_u64().unwrap());
                due_by_packet.insert(packet_sequence, event["due_tick"].as_u64());
            }
        }
    }
    assert_eq!(packet_ticks, (1..=18).collect::<Vec<_>>());
    assert_eq!(artifact["event_count"], events.len());

    let deliveries: Vec<(u64, u64, bool)> = events
        .iter()
        .filter(|record| record["event"]["kind"] == "sensor_delivery")
        .map(|record| {
            let packet_sequence = record["event"]["packet_sequence"].as_u64().unwrap();
            let due_tick =
                due_by_packet[&packet_sequence].expect("delivered packet has a due tick");
            let accepted = record["event"]["accepted"].as_bool().unwrap();
            assert_eq!(record["tick"].as_u64(), Some(due_tick));
            (due_tick, packet_sequence, accepted)
        })
        .collect();
    let mut sorted_deliveries = deliveries.clone();
    sorted_deliveries.sort_by_key(|delivery| (delivery.0, delivery.1));
    assert_eq!(deliveries, sorted_deliveries);
    assert!(deliveries.iter().any(|(_, _, accepted)| !accepted));
}

#[test]
fn restricted_zone_crossing_and_boundary_contact_fail_the_invariant() {
    let crossing = run_scenario(
        &fixture("restricted-zone-failure"),
        42,
        "restricted-zone-crossing",
    );
    assert_eq!(crossing.output.status.code(), Some(2));
    assert_eq!(crossing.report()["status"], "invariant_failed");
    assert_eq!(
        invariant(&crossing.report(), "restricted_zone")["passed"],
        false
    );
    assert_failure_event_evidence(&crossing, "restricted_zone", "tick_result");
    assert_eq!(replay(&crossing.artifact_path()).status.code(), Some(0));

    let boundary = run_scenario(
        &fixture("restricted-zone-boundary"),
        42,
        "restricted-zone-boundary",
    );
    assert_eq!(boundary.output.status.code(), Some(2));
    assert_eq!(
        invariant(&boundary.report(), "restricted_zone")["passed"],
        false
    );
    assert_failure_event_evidence(&boundary, "restricted_zone", "tick_result");
}

#[test]
fn stale_observation_without_fallback_fails_at_a_low_confidence_deadline() {
    let run = run_scenario(
        &fixture("safe-fallback-failure"),
        42,
        "safe-fallback-failure",
    );
    assert_eq!(run.output.status.code(), Some(2));
    let report = run.report();
    let fallback = invariant(&report, "safe_fallback");
    assert_eq!(fallback["passed"], false);
    assert_failure_event_evidence(&run, "safe_fallback", "tick_result");
    let failed_ticks: Vec<u64> = fallback["failures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|failure| failure["tick"].as_u64().unwrap())
        .collect();
    assert!(failed_ticks.iter().any(|tick| (2..=10).contains(tick)));
    assert!(report["telemetry"].as_array().unwrap().iter().any(|entry| {
        entry["confidence_permille"]
            .as_u64()
            .is_some_and(|value| value < 1000)
    }));
    let telemetry = report["telemetry"].as_array().unwrap();
    let first_low_tick = telemetry.iter().find(|entry| entry["tick"] == 2).unwrap();
    assert_eq!(first_low_tick["low_confidence_streak_ticks"], 1);
    assert_eq!(first_low_tick["low_confidence_watchdog_required"], false);
    let deadline_tick = telemetry.iter().find(|entry| entry["tick"] == 3).unwrap();
    assert_eq!(deadline_tick["low_confidence_streak_ticks"], 2);
    assert_eq!(deadline_tick["low_confidence_watchdog_required"], true);
}

#[test]
fn safety_fallback_starts_and_recovers_on_configured_ticks() {
    let mut scenario = fixture("gps-dropout");
    scenario["steps"] = json!(8);
    scenario["sensors"]["gps"]["dropout_windows"] = json!([{ "start_tick": 2, "end_tick": 4 }]);
    scenario["sensors"]["gps"]["noise_max_mm"] = json!(0);
    scenario["sensors"]["max_observation_age_ticks"] = json!(1);
    scenario["safety"] = json!({
        "confidence_threshold_permille": 500,
        "fallback_after_ticks": 2,
        "recovery_after_ticks": 2
    });
    let run = run_scenario(&scenario, 42, "fallback-recovery-deadline");
    assert_eq!(run.output.status.code(), Some(0));

    let telemetry = run.report()["telemetry"].as_array().unwrap().clone();
    let safety_state = |tick| {
        telemetry
            .iter()
            .find(|entry| entry["tick"] == tick)
            .unwrap()["safety_state"]
            .clone()
    };
    assert_eq!(safety_state(3), "nominal");
    assert_eq!(safety_state(4), "fallback");
    assert_eq!(safety_state(5), "fallback");
    assert_eq!(safety_state(6), "nominal");
}

#[test]
fn low_confidence_observation_cannot_complete_a_mission_waypoint() {
    let mut scenario = fixture("gps-dropout");
    scenario["steps"] = json!(1);
    scenario["sensors"]["gps"]["dropout_windows"] = json!([]);
    scenario["mission"]["waypoints"] = json!([{ "x_mm": 1061, "y_mm": 1029 }]);
    scenario["mission"]["arrival_radius_mm"] = json!(0);
    scenario["safety"] = json!({
        "confidence_threshold_permille": 500,
        "fallback_after_ticks": 3,
        "recovery_after_ticks": 2
    });

    let run = run_scenario(&scenario, 42, "low-confidence-waypoint");
    assert_eq!(run.output.status.code(), Some(0));
    let report = run.report();
    let telemetry = &report["telemetry"][0];
    assert_eq!(telemetry["mission_state"], "running");
    assert_eq!(telemetry["waypoint_index"], 0);
    assert_eq!(telemetry["truth_position"], scenario["vehicle"]["start"]);
    assert_eq!(telemetry["observed"]["position"]["x_mm"], 1061);
    assert_eq!(telemetry["observed"]["position"]["y_mm"], 1029);
    assert_eq!(telemetry["confidence_permille"], 400);
    assert!(telemetry["action"]["heading"].is_null());
    assert_eq!(telemetry["action"]["distance_mm"], 0);

    let artifact = run.artifact();
    let gps_sample = artifact["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["event"]["kind"] == "gps_sample")
        .expect("first GPS sample event");
    assert_eq!(gps_sample["tick"], 1);
    assert_eq!(gps_sample["event"]["observation"]["x_mm"], 1061);
    assert_eq!(gps_sample["event"]["observation"]["y_mm"], 1029);
    assert_eq!(gps_sample["event"]["confidence_permille"], 400);
}

#[test]
fn invalid_mission_transition_is_reported_as_an_invariant_failure() {
    let run = run_scenario(&fixture("invalid-transition"), 42, "invalid-transition");
    assert_eq!(run.output.status.code(), Some(2));
    assert_eq!(
        invariant(&run.report(), "valid_state_transitions")["passed"],
        false
    );
    assert_failure_event_evidence(&run, "valid_state_transitions", "transition");
}

#[test]
fn out_of_bounds_motion_is_reported_and_failure_artifacts_replay() {
    let run = run_scenario(&fixture("world-bounds-failure"), 42, "world-bounds-failure");
    assert_eq!(run.output.status.code(), Some(2));
    assert_eq!(invariant(&run.report(), "world_bounds")["passed"], false);
    assert_failure_event_evidence(&run, "world_bounds", "tick_result");
    assert_eq!(replay(&run.artifact_path()).status.code(), Some(0));
}

#[test]
fn replay_rejects_corrupt_deleted_reordered_and_modified_artifacts() {
    let baseline = run_scenario(&fixture("basic-mission"), 42, "tamper-baseline");
    assert_eq!(baseline.output.status.code(), Some(0));
    let original = baseline.artifact();

    let mut corrupt = original.clone();
    corrupt["events"][0]["hash"] = json!("0".repeat(64));
    assert_replay_rejected(&baseline.scratch, "corrupt", &corrupt);

    let mut deleted = original.clone();
    deleted["events"].as_array_mut().unwrap().remove(0);
    assert_replay_rejected(&baseline.scratch, "deleted", &deleted);

    let mut reordered = original.clone();
    reordered["events"].as_array_mut().unwrap().swap(0, 1);
    assert_replay_rejected(&baseline.scratch, "reordered", &reordered);

    let mut changed_report = original.clone();
    changed_report["expected_report"]["seed"] = json!(999);
    assert_replay_rejected(&baseline.scratch, "changed-report", &changed_report);

    let mut changed_schema = original.clone();
    changed_schema["schema_version"] = json!(999);
    assert_replay_rejected(&baseline.scratch, "changed-schema", &changed_schema);

    let mut changed_config = original;
    changed_config["scenario"]["vehicle"]["step_mm"] = json!(501);
    assert_replay_rejected(&baseline.scratch, "changed-config", &changed_config);

    let noisy_run = run_scenario(&fixture("gps-dropout"), 42, "coherent-tamper-baseline");
    let mut coherent_tamper: EventsArtifact =
        serde_json::from_slice(&fs::read(noisy_run.artifact_path()).unwrap())
            .expect("deserialize typed artifact");
    let noise_limit = coherent_tamper.scenario.sensors.gps.noise_max_mm;
    let sample = coherent_tamper.events.iter_mut().find_map(|record| {
        if let EventPayload::GpsSample {
            truth_at_sample,
            observation,
            dropped,
            noise_x_mm,
            noise_y_mm,
            confidence_permille,
            ..
        } = &mut record.event
        {
            if !*dropped && observation.is_some() {
                *noise_x_mm = if *noise_x_mm < noise_limit {
                    *noise_x_mm + 1
                } else {
                    *noise_x_mm - 1
                };
                *observation = Some(Point {
                    x_mm: truth_at_sample.x_mm + *noise_x_mm,
                    y_mm: truth_at_sample.y_mm + *noise_y_mm,
                });
                let total_noise = noise_x_mm.unsigned_abs() + noise_y_mm.unsigned_abs();
                let max_noise = (2 * noise_limit) as u32;
                *confidence_permille = (1000_u32 - total_noise * 1000 / max_noise) as u16;
                Some(())
            } else {
                None
            }
        } else {
            None
        }
    });
    assert!(
        sample.is_some(),
        "fixture must contain an accepted GPS sample"
    );
    rehash_artifact(&mut coherent_tamper);
    let coherent_path = baseline.scratch.0.join("self-consistent-tamper.json");
    fs::write(
        &coherent_path,
        serde_json::to_vec_pretty(&coherent_tamper).unwrap(),
    )
    .unwrap();
    assert_eq!(replay(&coherent_path).status.code(), Some(1));
}

#[derive(Serialize)]
struct EventHashInput<'a> {
    sequence: u64,
    tick: u32,
    event: &'a EventPayload,
    previous_hash: &'a str,
}

#[derive(Serialize)]
struct ArtifactHashInput<'a> {
    schema_version: u32,
    scenario: &'a autonomous_systems_vv_lab::model::Scenario,
    seed: u64,
    events: &'a [autonomous_systems_vv_lab::model::EventRecord],
    event_count: u64,
    final_hash: &'a str,
    expected_report: &'a autonomous_systems_vv_lab::model::RunReport,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn rehash_artifact(artifact: &mut EventsArtifact) {
    let mut previous_hash = "0".repeat(64);
    for record in &mut artifact.events {
        record.previous_hash.clone_from(&previous_hash);
        let input = EventHashInput {
            sequence: record.sequence,
            tick: record.tick,
            event: &record.event,
            previous_hash: &record.previous_hash,
        };
        record.hash = digest(&serde_json::to_vec(&input).unwrap());
        previous_hash.clone_from(&record.hash);
    }
    artifact.event_count = artifact.events.len() as u64;
    artifact.final_hash = previous_hash;
    artifact.expected_report.event_count = artifact.event_count;
    artifact
        .expected_report
        .final_hash
        .clone_from(&artifact.final_hash);
    let input = ArtifactHashInput {
        schema_version: artifact.schema_version,
        scenario: &artifact.scenario,
        seed: artifact.seed,
        events: &artifact.events,
        event_count: artifact.event_count,
        final_hash: &artifact.final_hash,
        expected_report: &artifact.expected_report,
    };
    artifact.artifact_sha256 = digest(&serde_json::to_vec(&input).unwrap());
}

fn assert_replay_rejected(parent: &Scratch, label: &str, artifact: &Value) {
    let path = parent.0.join(format!("{label}.json"));
    write_json(&path, artifact);
    let result = replay(&path);
    assert_eq!(
        result.status.code(),
        Some(1),
        "replay accepted {label} artifact"
    );
}

#[test]
fn invalid_schema_ranges_and_polygon_shapes_are_rejected() {
    let baseline = fixture("basic-mission");

    let mut unknown = baseline.clone();
    unknown["surprise"] = json!(true);
    assert_input_rejected(&unknown, "unknown-field");

    let mut out_of_range = baseline.clone();
    out_of_range["sensors"]["gps"]["dropout_permille"] = json!(1001);
    assert_input_rejected(&out_of_range, "range-error");

    let mut negative = baseline.clone();
    negative["sensors"]["gps"]["dropout_permille"] = json!(-1);
    assert_input_rejected(&negative, "negative-range");

    let mut over_limit = baseline.clone();
    over_limit["steps"] = json!(10001);
    assert_input_rejected(&over_limit, "resource-limit");

    let mut degenerate_polygon = baseline.clone();
    degenerate_polygon["restricted_zone"]["vertices"] = json!([
        { "x_mm": 1000, "y_mm": 1000 },
        { "x_mm": 2000, "y_mm": 1000 }
    ]);
    assert_input_rejected(&degenerate_polygon, "invalid-polygon");

    let mut self_intersecting = baseline.clone();
    self_intersecting["restricted_zone"]["vertices"] = json!([
        { "x_mm": 3000, "y_mm": 3000 },
        { "x_mm": 5000, "y_mm": 5000 },
        { "x_mm": 3000, "y_mm": 5000 },
        { "x_mm": 5000, "y_mm": 3000 }
    ]);
    assert_input_rejected(&self_intersecting, "self-intersecting-polygon");

    let mut out_of_bounds = baseline;
    out_of_bounds["vehicle"]["start"]["x_mm"] = json!(10001);
    assert_input_rejected(&out_of_bounds, "out-of-bounds-start");
}

fn assert_input_rejected(scenario: &Value, label: &str) {
    let run = run_scenario(scenario, 42, label);
    assert_eq!(run.output.status.code(), Some(1));
    assert!(
        !run.output_dir().join("events.json").exists(),
        "invalid input must not produce a replayable artifact"
    );
}

#[test]
fn unknown_commands_and_missing_arguments_return_usage_errors() {
    for arguments in [vec!["unknown-command"], vec![], vec!["run"], vec!["replay"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_vv-lab"))
            .args(arguments)
            .output()
            .expect("invoke CLI");
        assert_eq!(output.status.code(), Some(1));
    }
}

#[test]
fn benchmark_reports_seed_iterations_and_simulated_work() {
    let output = Command::new(env!("CARGO_BIN_EXE_vv-lab"))
        .args([
            "benchmark",
            "scenarios/basic-mission.json",
            "--seed",
            "42",
            "--iterations",
            "3",
        ])
        .output()
        .expect("run benchmark command");
    assert_eq!(output.status.code(), Some(0));
    let result: Value = serde_json::from_slice(&output.stdout).expect("benchmark JSON output");
    assert_eq!(result["scenario_name"], "basic-mission");
    assert_eq!(result["seed"], 42);
    assert_eq!(result["iterations"], 3);
    assert_eq!(result["steps_per_run"], 24);
    assert_eq!(result["simulated_ticks"], 72);
    let final_hash = result["final_hash"].as_str().expect("benchmark final hash");
    assert_eq!(final_hash.len(), 64);
    assert!(final_hash.bytes().all(|byte| byte.is_ascii_hexdigit()));

    let reference = run_scenario(&fixture("basic-mission"), 42, "benchmark-reference");
    assert_eq!(reference.output.status.code(), Some(0));
    assert_eq!(final_hash, reference.report()["final_hash"]);
}

#[test]
fn malformed_benchmark_options_return_usage_errors() {
    let invalid_arguments = [
        vec![
            "benchmark",
            "scenarios/basic-mission.json",
            "--seed",
            "42",
            "--iterations",
            "0",
        ],
        vec![
            "benchmark",
            "scenarios/basic-mission.json",
            "--seed",
            "42",
            "--iterations",
            "1",
            "--unexpected",
            "value",
        ],
        vec!["benchmark", "scenarios/basic-mission.json", "--seed", "42"],
    ];
    for arguments in invalid_arguments {
        let output = Command::new(env!("CARGO_BIN_EXE_vv-lab"))
            .args(arguments)
            .output()
            .expect("invoke malformed benchmark command");
        assert_eq!(output.status.code(), Some(1));
    }
}
