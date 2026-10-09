use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use autonomous_systems_vv_lab::actuator::{ActuatorBehavior, ActuatorConfig, ActuatorEventKind};
use autonomous_systems_vv_lab::contracts::ContractSet;
use autonomous_systems_vv_lab::mission_analysis::{MissionOutcome, MissionPolicy};
use autonomous_systems_vv_lab::model::RunStatus;
use autonomous_systems_vv_lab::simulation::{
    benchmark_verified_scenario, replay_verified_artifact, run_verified_scenario,
};
use autonomous_systems_vv_lab::{ContractOutcome, parse_scenario, parse_verification_artifact};
use serde_json::Value;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let unique = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vv-lab-m3-integration-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create isolated M3 test directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stop_scenario() -> autonomous_systems_vv_lab::Scenario {
    parse_scenario(include_str!("../scenarios/m3/nominal-stop.json"))
        .expect("valid nominal stop fixture")
}

fn stop_contracts() -> ContractSet {
    ContractSet::from_json(include_str!("../scenarios/m3/contracts-stop.json"))
        .expect("valid stop contracts")
}

fn continued_stop_actuator() -> ActuatorConfig {
    ActuatorConfig::from_json(include_str!("../scenarios/m3/actuator-continued-stop.json"))
        .expect("valid continued movement fault")
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vv-lab"))
}

fn write_fixture(path: &Path, contents: &str) {
    fs::write(path, contents.as_bytes()).expect("write fixture");
}

fn json_file(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read JSON output")).expect("valid JSON output")
}

#[test]
fn verified_simulation_records_real_actuator_failure_and_replays_exactly() {
    let outcome = run_verified_scenario(
        stop_scenario(),
        42,
        continued_stop_actuator(),
        stop_contracts(),
        MissionPolicy::default(),
        "vv-lab replay events.json".to_owned(),
    )
    .expect("run verified scenario");

    assert_eq!(
        outcome.verification.summary.safety_status,
        RunStatus::Passed
    );
    assert_eq!(
        outcome.verification.summary.temporal_status,
        ContractOutcome::Fail
    );
    assert_eq!(
        outcome.verification.summary.mission_outcome,
        MissionOutcome::Completed
    );
    assert!(outcome.verification.summary.contracts_passed > 0);
    assert!(outcome.verification.summary.contracts_failed > 0);

    let stop_result = outcome
        .verification
        .contract_results
        .iter()
        .find(|result| result.contract_id == "stop_response")
        .expect("stop contract result");
    assert_eq!(stop_result.outcome, ContractOutcome::Fail);
    assert_eq!(stop_result.first_trigger_tick, Some(7));
    assert_eq!(stop_result.deadline_tick, Some(9));
    assert_eq!(stop_result.first_violation_tick, Some(9));

    let evidence = outcome
        .verification
        .failures
        .iter()
        .find(|failure| failure.contract_id == "stop_response")
        .expect("stop failure evidence");
    assert_eq!(evidence.contract_version, 1);
    assert_eq!(evidence.first_trigger_tick, Some(7));
    assert_eq!(evidence.applicable_deadline_tick, Some(9));
    assert_eq!(evidence.first_violation_tick, 9);
    assert_eq!(
        evidence.actuator_behavior_at_violation,
        Some(ActuatorBehavior::ContinuedMovement)
    );
    let violation_tick = evidence
        .counterexample_ticks
        .iter()
        .find(|tick| tick.tick == 9)
        .expect("violation tick retained in counterexample");
    assert_eq!(evidence.truth_position, Some(violation_tick.truth_position));
    assert_ne!(violation_tick.truth_from, violation_tick.truth_position);
    assert!(
        evidence
            .source_event_sequences
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    );
    assert_eq!(
        violation_tick.actuator_behavior,
        ActuatorBehavior::ContinuedMovement
    );
    assert!(evidence.fault_events.iter().any(|linked| {
        linked.event.fault_id == "stuck_after_stop"
            && linked.event.kind == ActuatorEventKind::ContinuedMovement
    }));

    let replay = replay_verified_artifact(outcome.artifact.clone(), outcome.verification.clone())
        .expect("verified replay matches run");
    assert_eq!(replay.report, outcome.artifact.expected_report);
    assert_eq!(replay.mission, outcome.mission);
    assert_eq!(replay.verification, outcome.verification);

    let mut corrupted_artifact = outcome.artifact;
    corrupted_artifact.seed = corrupted_artifact.seed.saturating_add(1);
    assert!(replay_verified_artifact(corrupted_artifact, outcome.verification).is_err());
}

#[test]
fn verified_simulation_and_benchmark_reject_invalid_inputs() {
    let scenario = stop_scenario();
    let mut invalid_contracts = stop_contracts();
    invalid_contracts.schema_version = 2;
    assert!(
        run_verified_scenario(
            scenario.clone(),
            42,
            ActuatorConfig::default(),
            invalid_contracts,
            MissionPolicy::default(),
            "vv-lab replay events.json".to_owned(),
        )
        .is_err()
    );

    assert!(
        run_verified_scenario(
            scenario.clone(),
            42,
            ActuatorConfig::default(),
            stop_contracts(),
            MissionPolicy {
                stall_window_ticks: 0,
                min_progress_mm: 1,
            },
            "vv-lab replay events.json".to_owned(),
        )
        .is_err()
    );

    let actuator = continued_stop_actuator();
    let contracts = stop_contracts();
    assert!(
        benchmark_verified_scenario(
            &scenario,
            42,
            &actuator,
            &contracts,
            MissionPolicy::default(),
            0,
        )
        .is_err()
    );
}

#[test]
fn verified_benchmark_measures_the_configured_iterations_and_is_seed_deterministic() {
    let scenario = stop_scenario();
    let actuator = continued_stop_actuator();
    let contracts = stop_contracts();
    let first = benchmark_verified_scenario(
        &scenario,
        42,
        &actuator,
        &contracts,
        MissionPolicy::default(),
        2,
    )
    .expect("M3 benchmark");
    let second = benchmark_verified_scenario(
        &scenario,
        42,
        &actuator,
        &contracts,
        MissionPolicy::default(),
        2,
    )
    .expect("repeat M3 benchmark");

    assert_eq!(first.iterations, 2);
    assert_eq!(first.simulated_ticks, u64::from(scenario.steps) * 2);
    assert_eq!(first.final_hash, second.final_hash);
    assert_ne!(first.final_hash, [0; 32]);
}

#[test]
fn m3_cli_writes_replayable_sidecars_and_exposes_verified_benchmark() {
    let scratch = Scratch::new("cli");
    let scenario_path = scratch.0.join("scenario.json");
    let contracts_path = scratch.0.join("contracts.json");
    let actuator_path = scratch.0.join("actuator.json");
    let output_dir = scratch.0.join("faulted-run");
    write_fixture(
        &scenario_path,
        include_str!("../scenarios/m3/nominal-stop.json"),
    );
    write_fixture(
        &contracts_path,
        include_str!("../scenarios/m3/contracts-stop.json"),
    );
    write_fixture(
        &actuator_path,
        include_str!("../scenarios/m3/actuator-continued-stop.json"),
    );

    let run = cli()
        .arg("run")
        .arg(&scenario_path)
        .args(["--seed", "42", "--output"])
        .arg(&output_dir)
        .arg("--contracts")
        .arg(&contracts_path)
        .arg("--actuator")
        .arg(&actuator_path)
        .output()
        .expect("invoke M3 run");
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("temporal status: Fail"));
    for name in [
        "events.json",
        "report.json",
        "mission.json",
        "verification.json",
    ] {
        assert!(output_dir.join(name).is_file(), "missing {name}");
    }
    let verification_path = output_dir.join("verification.json");
    let verification = json_file(&verification_path);
    assert_eq!(verification["summary"]["temporal_status"], "FAIL");
    assert_eq!(verification["summary"]["safety_status"], "passed");
    assert_eq!(verification["failures"][0]["contract_id"], "stop_response");

    let auto_replay = cli()
        .current_dir(&output_dir)
        .args(["replay", "events.json"])
        .output()
        .expect("invoke auto-discovered verified replay");
    assert_eq!(auto_replay.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&auto_replay.stdout).contains("temporal=Fail"));

    let explicit_replay = cli()
        .arg("replay")
        .arg(output_dir.join("events.json"))
        .arg("--verification")
        .arg(&verification_path)
        .output()
        .expect("invoke explicit verified replay");
    assert_eq!(explicit_replay.status.code(), Some(0));

    let benchmark = cli()
        .arg("benchmark")
        .arg(&scenario_path)
        .args(["--seed", "42", "--iterations", "2", "--contracts"])
        .arg(&contracts_path)
        .arg("--actuator")
        .arg(&actuator_path)
        .output()
        .expect("invoke M3 benchmark");
    assert_eq!(benchmark.status.code(), Some(0));
    let benchmark_json: Value = serde_json::from_slice(&benchmark.stdout).expect("benchmark JSON");
    assert_eq!(benchmark_json["mode"], "m3");
    assert_eq!(benchmark_json["simulated_ticks"], 32);
    assert_eq!(benchmark_json["iterations"], 2);
    assert_eq!(
        benchmark_json["final_hash"]
            .as_str()
            .expect("hash string")
            .len(),
        64
    );

    let mut corrupted = verification;
    corrupted["verification_sha256"] = Value::String("not-a-sha256-digest".to_owned());
    fs::write(
        &verification_path,
        serde_json::to_vec(&corrupted).expect("serialize corrupted sidecar"),
    )
    .expect("write corrupted sidecar");
    let rejected = cli()
        .arg("replay")
        .arg(output_dir.join("events.json"))
        .arg("--verification")
        .arg(&verification_path)
        .output()
        .expect("invoke replay with corrupted sidecar");
    assert_eq!(rejected.status.code(), Some(1));
}

#[test]
fn legacy_run_replaces_old_m3_sidecar_before_replay() {
    let scratch = Scratch::new("legacy-overwrite");
    let scenario_path = scratch.0.join("scenario.json");
    let contracts_path = scratch.0.join("contracts.json");
    let output_dir = scratch.0.join("reused-output");
    write_fixture(
        &scenario_path,
        include_str!("../scenarios/m3/nominal-stop.json"),
    );
    write_fixture(
        &contracts_path,
        include_str!("../scenarios/m3/contracts-stop.json"),
    );

    let verified = cli()
        .arg("run")
        .arg(&scenario_path)
        .args(["--seed", "42", "--output"])
        .arg(&output_dir)
        .arg("--contracts")
        .arg(&contracts_path)
        .output()
        .expect("write M3 artifacts");
    assert_eq!(verified.status.code(), Some(0));
    assert!(output_dir.join("verification.json").is_file());

    let legacy = cli()
        .arg("run")
        .arg(&scenario_path)
        .args(["--seed", "42", "--output"])
        .arg(&output_dir)
        .output()
        .expect("overwrite with a legacy run");
    assert_eq!(
        legacy.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&legacy.stderr)
    );
    assert!(!output_dir.join("verification.json").exists());

    let replay = cli()
        .arg("replay")
        .arg(output_dir.join("events.json"))
        .output()
        .expect("replay legacy artifact");
    assert_eq!(
        replay.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(String::from_utf8_lossy(&replay.stdout).contains("replay verified: Passed"));
}

#[test]
fn m3_cli_rejects_unbound_actuator_flags_and_output_over_budget() {
    let scratch = Scratch::new("cli-bounds");
    let scenario_path = scratch.0.join("scenario.json");
    let actuator_path = scratch.0.join("actuator.json");
    write_fixture(
        &scenario_path,
        include_str!("../scenarios/m3/nominal-stop.json"),
    );
    write_fixture(
        &actuator_path,
        include_str!("../scenarios/m3/actuator-none.json"),
    );

    let unbound_run = cli()
        .arg("run")
        .arg(&scenario_path)
        .args(["--seed", "42", "--output"])
        .arg(scratch.0.join("unbound"))
        .arg("--actuator")
        .arg(&actuator_path)
        .output()
        .expect("invoke invalid run options");
    assert_eq!(unbound_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unbound_run.stderr).contains("requires --contracts"));

    let unbound_benchmark = cli()
        .arg("benchmark")
        .arg(&scenario_path)
        .args(["--seed", "42", "--iterations", "1", "--actuator"])
        .arg(&actuator_path)
        .output()
        .expect("invoke invalid benchmark options");
    assert_eq!(unbound_benchmark.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unbound_benchmark.stderr).contains("requires --contracts"));

    let contracts_path = scratch.0.join("contracts.json");
    write_fixture(
        &contracts_path,
        include_str!("../scenarios/m3/contracts-stop.json"),
    );
    let limited_output_dir = scratch.0.join("limited");
    let over_budget = cli()
        .arg("run")
        .arg(&scenario_path)
        .args(["--seed", "42", "--output"])
        .arg(&limited_output_dir)
        .arg("--contracts")
        .arg(&contracts_path)
        .args(["--max-output-bytes", "1"])
        .output()
        .expect("invoke output-bounded run");
    assert_eq!(over_budget.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&over_budget.stderr).contains("--max-output-bytes"));
    assert!(!limited_output_dir.exists());
}

#[test]
fn m3_cli_uses_the_default_actuator_for_nominal_contract_runs() {
    let scratch = Scratch::new("cli-default-actuator");
    let scenario_path = scratch.0.join("scenario.json");
    let contracts_path = scratch.0.join("contracts.json");
    let output_dir = scratch.0.join("nominal-run");
    write_fixture(
        &scenario_path,
        include_str!("../scenarios/m3/nominal-stop.json"),
    );
    write_fixture(
        &contracts_path,
        include_str!("../scenarios/m3/contracts-nominal.json"),
    );
    let run = cli()
        .arg("run")
        .arg(&scenario_path)
        .args(["--seed", "42", "--output"])
        .arg(&output_dir)
        .arg("--contracts")
        .arg(&contracts_path)
        .output()
        .expect("invoke nominal M3 run");
    assert_eq!(run.status.code(), Some(0));
    let verification = parse_verification_artifact(
        &fs::read_to_string(output_dir.join("verification.json")).expect("sidecar exists"),
    )
    .expect("valid M3 sidecar");
    assert_eq!(verification.summary.temporal_status, ContractOutcome::Pass);
    assert_eq!(verification.actuator_config, ActuatorConfig::default());
}
