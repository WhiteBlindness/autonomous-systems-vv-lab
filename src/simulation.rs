use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::actuator::{ActuationOutcome, ActuatorBehavior, ActuatorConfig, ActuatorFaultEvent};
use crate::artifact::{
    artifact_digest, extract_exogenous, hash_serializable, verify_artifact_integrity,
};
use crate::contracts::{
    CommandOutcome, ContractSet, PropertyType, TraceFact, TransitionDomain, TransitionState,
};
use crate::engine::{CoreOutput, execute, execute_with_actuator};
use crate::error::LabError;
use crate::mission_analysis::{MissionAnalysis, MissionOutcome, MissionPolicy, analyze_mission};
use crate::model::{
    Action, EventPayload, EventRecord, EventsArtifact, MissionState, ObservedTelemetry, Point,
    RunReport, RunStatus, SCHEMA_VERSION, SafetyState, Scenario,
};
use crate::temporal::{
    ContractOutcome, ContractResult, EvidenceReason, ExpectedCondition, ObservationState,
    TraceTick, TraceTransition, evaluate,
};

const MAX_SCENARIO_BYTES: usize = 2 * 1024 * 1024;
const MAX_ARTIFACT_BYTES: usize = 128 * 1024 * 1024;
const MAX_VERIFICATION_BYTES: usize = 32 * 1024 * 1024;
pub const VERIFICATION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub struct ReplayOutcome {
    pub report: crate::model::RunReport,
}

/// M3-only verification sidecar. It binds declarative contracts and actuator
/// outcomes to the unchanged v1 events artifact.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationArtifact {
    pub schema_version: u32,
    pub source_artifact_sha256: String,
    pub source_final_hash: String,
    pub contract_set: ContractSet,
    pub actuator_config: ActuatorConfig,
    pub actuator_trace_sha256: String,
    pub actuator_fault_event_count: u64,
    pub mission_policy: MissionPolicy,
    pub summary: VerificationSummary,
    pub contract_results: Vec<ContractResult>,
    pub failures: Vec<ContractFailureEvidence>,
    pub reproduction_command: String,
    pub verification_sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct VerificationSummary {
    pub temporal_status: ContractOutcome,
    pub safety_status: RunStatus,
    pub mission_outcome: MissionOutcome,
    pub contracts_passed: u32,
    pub contracts_failed: u32,
    pub contracts_inconclusive: u32,
}

/// A concise, source-linked report for the first counterexample of a contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContractFailureEvidence {
    pub contract_id: String,
    pub contract_version: u32,
    pub property_type: PropertyType,
    pub first_trigger_tick: Option<u32>,
    pub applicable_deadline_tick: Option<u64>,
    pub first_violation_tick: u32,
    pub expected: ExpectedCondition,
    pub expected_condition: String,
    pub actual_condition: String,
    pub actual_fact: Option<TraceFact>,
    pub actual_transition: Option<TraceTransition>,
    pub observation: ObservationState,
    pub evidence_reason: EvidenceReason,
    pub monitor_derived_command_outcome: Option<CommandOutcome>,
    pub detail: Option<String>,
    pub trigger_command: Option<CommandAtTick>,
    pub violation_command: Option<CommandAtTick>,
    pub actuator_behavior_at_trigger: Option<ActuatorBehavior>,
    pub actuator_behavior_at_violation: Option<ActuatorBehavior>,
    pub truth_from: Option<Point>,
    pub truth_position: Option<Point>,
    pub fault_events: Vec<LinkedActuatorFaultEvent>,
    pub source_event_sequences: Vec<u64>,
    pub counterexample_ticks: Vec<CounterexampleTick>,
    pub source_artifact_sha256: String,
    pub reproduction_command: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandAtTick {
    pub tick: u32,
    pub event_sequence: u64,
    pub action: Action,
}

/// An actuator event linked to the legacy event records that establish its
/// requested command, observation tick and optional execution tick.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinkedActuatorFaultEvent {
    pub event: ActuatorFaultEvent,
    pub observed_event_sequence: u64,
    pub source_command_event_sequence: u64,
    pub execution_event_sequence: Option<u64>,
}

/// Tick-level source material retained only around a temporal counterexample.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CounterexampleTick {
    pub tick: u32,
    pub truth_from: Point,
    pub truth_position: Point,
    pub observed: Option<ObservedTelemetry>,
    pub mission_state: MissionState,
    pub safety_state: SafetyState,
    pub requested_action: Action,
    pub realized_action: Action,
    pub actuator_behavior: ActuatorBehavior,
    pub fault_events: Vec<LinkedActuatorFaultEvent>,
    pub source_event_sequences: Vec<u64>,
}

#[derive(Clone, Debug)]
pub struct VerifiedRunOutcome {
    pub artifact: EventsArtifact,
    pub mission: MissionAnalysis,
    pub verification: VerificationArtifact,
}

#[derive(Clone, Debug)]
pub struct VerifiedReplayOutcome {
    pub report: RunReport,
    pub mission: MissionAnalysis,
    pub verification: VerificationArtifact,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedBenchmarkResult {
    pub iterations: u32,
    pub simulated_ticks: u64,
    pub simulation_elapsed_micros: u64,
    pub artifact_elapsed_micros: u64,
    pub temporal_monitor_elapsed_micros: u64,
    pub final_hash: [u8; 32],
}

pub fn run_scenario(scenario: Scenario, seed: u64) -> Result<EventsArtifact, LabError> {
    scenario
        .validate()
        .map_err(|error| LabError(error.to_string()))?;
    let output = execute(&scenario, seed, None)?;
    Ok(assemble_artifact(
        scenario,
        seed,
        output.events,
        output.report,
    ))
}

/// Run M3 verification with the supplied actuator and declarative contracts.
/// The returned events/report artifacts retain their existing v1 schemas.
pub fn run_verified_scenario(
    scenario: Scenario,
    seed: u64,
    actuator_config: ActuatorConfig,
    contract_set: ContractSet,
    mission_policy: MissionPolicy,
    reproduction_command: String,
) -> Result<VerifiedRunOutcome, LabError> {
    scenario
        .validate()
        .map_err(|error| LabError(error.to_string()))?;
    actuator_config.validate(scenario.steps).map_err(LabError)?;
    contract_set
        .validate()
        .map_err(|error| LabError(error.to_string()))?;
    let mission_policy = mission_policy.validate()?;

    let output = execute_with_actuator(&scenario, seed, None, &actuator_config)?;
    let artifact = assemble_artifact(scenario, seed, output.events, output.report);
    let mission = analyze_mission(
        &artifact.scenario,
        &artifact.expected_report,
        mission_policy,
    )?;
    let verification = build_verification_artifact(
        &artifact,
        &output.actuation,
        actuator_config,
        contract_set,
        mission_policy,
        mission.outcome,
        reproduction_command,
    )?;
    Ok(VerifiedRunOutcome {
        artifact,
        mission,
        verification,
    })
}

pub fn replay_artifact(artifact: EventsArtifact) -> Result<ReplayOutcome, LabError> {
    let report = recompute_artifact(&artifact)?;
    Ok(ReplayOutcome { report })
}

/// Recompute the v1 artifact with its M3 actuator configuration and monitor,
/// then compare the complete regenerated sidecar.
pub fn replay_verified_artifact(
    artifact: EventsArtifact,
    verification: VerificationArtifact,
) -> Result<VerifiedReplayOutcome, LabError> {
    verify_verification_integrity(&verification, &artifact)?;
    let output = recompute_core(&artifact, Some(&verification.actuator_config))?;
    let mission = analyze_mission(
        &artifact.scenario,
        &output.report,
        verification.mission_policy,
    )?;
    if mission.outcome != verification.summary.mission_outcome {
        return Err(LabError(
            "recomputed mission outcome differs from verification.json".into(),
        ));
    }
    let recomputed = build_verification_artifact(
        &artifact,
        &output.actuation,
        verification.actuator_config.clone(),
        verification.contract_set.clone(),
        verification.mission_policy,
        mission.outcome,
        verification.reproduction_command.clone(),
    )?;
    if recomputed != verification {
        return Err(LabError(
            "recomputed temporal verification differs from verification.json".into(),
        ));
    }
    Ok(VerifiedReplayOutcome {
        report: output.report,
        mission,
        verification,
    })
}

/// Measure actuator-enabled simulation, artifact binding, and temporal
/// evaluation separately. JSON file I/O and Python orchestration are excluded.
pub fn benchmark_verified_scenario(
    scenario: &Scenario,
    seed: u64,
    actuator_config: &ActuatorConfig,
    contract_set: &ContractSet,
    mission_policy: MissionPolicy,
    iterations: u32,
) -> Result<VerifiedBenchmarkResult, LabError> {
    if !(1..=10_000).contains(&iterations) {
        return Err(LabError("iterations must be between 1 and 10000".into()));
    }
    scenario
        .validate()
        .map_err(|error| LabError(error.to_string()))?;
    actuator_config.validate(scenario.steps).map_err(LabError)?;
    contract_set
        .validate()
        .map_err(|error| LabError(error.to_string()))?;
    mission_policy.validate()?;

    let warmup = execute_with_actuator(scenario, seed, None, actuator_config)?;
    let warmup_artifact = assemble_artifact(scenario.clone(), seed, warmup.events, warmup.report);
    let warmup_trace = build_temporal_trace(&warmup_artifact, &warmup.actuation)?;
    std::hint::black_box(evaluate(contract_set, &warmup_trace));

    let mut simulation_elapsed_micros = 0_u64;
    let mut artifact_elapsed_micros = 0_u64;
    let mut temporal_monitor_elapsed_micros = 0_u64;
    let mut final_hash = [0_u8; 32];
    for _ in 0..iterations {
        let started = Instant::now();
        let output = execute_with_actuator(scenario, seed, None, actuator_config)?;
        simulation_elapsed_micros =
            simulation_elapsed_micros.saturating_add(elapsed_micros(started));

        let started = Instant::now();
        let artifact = assemble_artifact(scenario.clone(), seed, output.events, output.report);
        artifact_elapsed_micros = artifact_elapsed_micros.saturating_add(elapsed_micros(started));
        final_hash = parse_hash(&artifact.final_hash)?;

        let started = Instant::now();
        let trace = build_temporal_trace(&artifact, &output.actuation)?;
        let _results = evaluate(contract_set, &trace);
        temporal_monitor_elapsed_micros =
            temporal_monitor_elapsed_micros.saturating_add(elapsed_micros(started));
    }

    Ok(VerifiedBenchmarkResult {
        iterations,
        simulated_ticks: u64::from(scenario.steps) * u64::from(iterations),
        simulation_elapsed_micros,
        artifact_elapsed_micros,
        temporal_monitor_elapsed_micros,
        final_hash,
    })
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn parse_hash(value: &str) -> Result<[u8; 32], LabError> {
    if value.len() != 64 {
        return Err(LabError("final hash is not a SHA-256 digest".into()));
    }
    let mut digest = [0_u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = u8::from_str_radix(&value[offset..offset + 2], 16)
            .map_err(|_| LabError("final hash is not a SHA-256 digest".into()))?;
    }
    Ok(digest)
}

pub fn analyze_artifact(
    artifact: EventsArtifact,
    policy: MissionPolicy,
) -> Result<MissionAnalysis, LabError> {
    let report = recompute_artifact(&artifact)?;
    analyze_mission(&artifact.scenario, &report, policy)
}

fn assemble_artifact(
    scenario: Scenario,
    seed: u64,
    events: Vec<EventRecord>,
    report: RunReport,
) -> EventsArtifact {
    let event_count = events.len() as u64;
    let final_hash = events
        .last()
        .map(|event| event.hash.clone())
        .unwrap_or_else(|| crate::artifact::GENESIS_HASH.to_owned());
    let artifact_sha256 =
        artifact_digest(&scenario, seed, &events, event_count, &final_hash, &report);
    EventsArtifact {
        schema_version: SCHEMA_VERSION,
        scenario,
        seed,
        events,
        event_count,
        final_hash,
        expected_report: report,
        artifact_sha256,
    }
}

fn build_verification_artifact(
    artifact: &EventsArtifact,
    actuation: &[ActuationOutcome],
    actuator_config: ActuatorConfig,
    contract_set: ContractSet,
    mission_policy: MissionPolicy,
    mission_outcome: MissionOutcome,
    reproduction_command: String,
) -> Result<VerificationArtifact, LabError> {
    let trace = build_temporal_trace(artifact, actuation)?;
    let contract_results = evaluate(&contract_set, &trace);
    let failures = build_failure_evidence(
        artifact,
        actuation,
        &trace,
        &contract_results,
        &reproduction_command,
    )?;
    let mut counts = (0_u32, 0_u32, 0_u32);
    for result in &contract_results {
        match result.outcome {
            ContractOutcome::Pass => counts.0 = counts.0.saturating_add(1),
            ContractOutcome::Fail => counts.1 = counts.1.saturating_add(1),
            ContractOutcome::Inconclusive => counts.2 = counts.2.saturating_add(1),
        }
    }
    let temporal_status = if counts.1 > 0 {
        ContractOutcome::Fail
    } else if counts.2 > 0 {
        ContractOutcome::Inconclusive
    } else {
        ContractOutcome::Pass
    };
    let actuator_fault_event_count = actuation
        .iter()
        .map(|outcome| outcome.fault_events.len() as u64)
        .sum();
    let mut verification = VerificationArtifact {
        schema_version: VERIFICATION_SCHEMA_VERSION,
        source_artifact_sha256: artifact.artifact_sha256.clone(),
        source_final_hash: artifact.final_hash.clone(),
        contract_set,
        actuator_config,
        actuator_trace_sha256: hash_serializable(actuation),
        actuator_fault_event_count,
        mission_policy,
        summary: VerificationSummary {
            temporal_status,
            safety_status: artifact.expected_report.status,
            mission_outcome,
            contracts_passed: counts.0,
            contracts_failed: counts.1,
            contracts_inconclusive: counts.2,
        },
        contract_results,
        failures,
        reproduction_command,
        verification_sha256: String::new(),
    };
    verification.verification_sha256 = verification_digest(&verification);
    Ok(verification)
}

fn verification_digest(verification: &VerificationArtifact) -> String {
    hash_serializable(&UnsignedVerificationArtifact {
        schema_version: verification.schema_version,
        source_artifact_sha256: &verification.source_artifact_sha256,
        source_final_hash: &verification.source_final_hash,
        contract_set: &verification.contract_set,
        actuator_config: &verification.actuator_config,
        actuator_trace_sha256: &verification.actuator_trace_sha256,
        actuator_fault_event_count: verification.actuator_fault_event_count,
        mission_policy: verification.mission_policy,
        summary: verification.summary,
        contract_results: &verification.contract_results,
        failures: &verification.failures,
        reproduction_command: &verification.reproduction_command,
    })
}

#[derive(Serialize)]
struct UnsignedVerificationArtifact<'a> {
    schema_version: u32,
    source_artifact_sha256: &'a str,
    source_final_hash: &'a str,
    contract_set: &'a ContractSet,
    actuator_config: &'a ActuatorConfig,
    actuator_trace_sha256: &'a str,
    actuator_fault_event_count: u64,
    mission_policy: MissionPolicy,
    summary: VerificationSummary,
    contract_results: &'a [ContractResult],
    failures: &'a [ContractFailureEvidence],
    reproduction_command: &'a str,
}

fn build_temporal_trace(
    artifact: &EventsArtifact,
    actuation: &[ActuationOutcome],
) -> Result<Vec<TraceTick>, LabError> {
    let scenario = &artifact.scenario;
    let mut ticks = BTreeMap::new();
    let mut restricted_zone = BTreeMap::new();
    let mut transitions = BTreeMap::<u32, Vec<TraceTransition>>::new();
    for record in &artifact.events {
        match &record.event {
            EventPayload::TickResult {
                truth_from,
                truth_position,
                observed,
                mission_state,
                safety_state,
                action,
                ..
            } => {
                if ticks
                    .insert(
                        record.tick,
                        SourceTick {
                            event_sequence: record.sequence,
                            truth_from: *truth_from,
                            truth_position: *truth_position,
                            observed: observed.clone(),
                            mission_state: *mission_state,
                            safety_state: *safety_state,
                            action: *action,
                        },
                    )
                    .is_some()
                {
                    return Err(LabError(format!(
                        "duplicate tick result at tick {}",
                        record.tick
                    )));
                }
            }
            EventPayload::InvariantCheck { name, passed, .. } if name == "restricted_zone" => {
                if restricted_zone.insert(record.tick, *passed).is_some() {
                    return Err(LabError(format!(
                        "duplicate restricted_zone invariant at tick {}",
                        record.tick
                    )));
                }
            }
            EventPayload::Transition {
                subsystem,
                from,
                to,
                ..
            } => {
                let Some(domain) = transition_domain(subsystem) else {
                    continue;
                };
                transitions
                    .entry(record.tick)
                    .or_default()
                    .push(TraceTransition {
                        source_sequence: record.sequence,
                        domain,
                        from: transition_state(from)?,
                        to: transition_state(to)?,
                    });
            }
            _ => {}
        }
    }

    if ticks.len() != scenario.steps as usize || actuation.len() != scenario.steps as usize {
        return Err(LabError(
            "M3 temporal trace requires one tick result and actuator outcome per configured tick"
                .into(),
        ));
    }
    let mut progress = MissionProgressTracker::new();
    progress.sample(scenario.vehicle.start, scenario);
    let mut result = Vec::with_capacity(scenario.steps as usize);
    let mut previous_sequence = 0_u64;
    let mut outcomes_by_tick = BTreeMap::new();
    for outcome in actuation {
        if outcomes_by_tick.insert(outcome.tick, outcome).is_some() {
            return Err(LabError(format!(
                "duplicate actuator outcome at tick {}",
                outcome.tick
            )));
        }
    }

    for tick in 1..=scenario.steps {
        let source = ticks
            .get(&tick)
            .ok_or_else(|| LabError(format!("missing tick result at tick {tick}")))?;
        let outcome = outcomes_by_tick
            .get(&tick)
            .ok_or_else(|| LabError(format!("missing actuator outcome at tick {tick}")))?;
        if source.event_sequence <= previous_sequence {
            return Err(LabError("tick result source order is invalid".into()));
        }
        previous_sequence = source.event_sequence;
        progress.sample(source.truth_position, scenario);

        let mut facts = Vec::new();
        // The event stream records an absent GPS observation as a known
        // localization failure. Missing monitor telemetry without this fact
        // remains unknown inside the temporal monitor.
        let localization_unreliable = source.observed.as_ref().is_none_or(|observation| {
            !observation.fresh
                || observation.confidence_permille < scenario.safety.confidence_threshold_permille
        });
        if localization_unreliable {
            facts.push(TraceFact::LocalizationUnreliable);
        }
        if source.safety_state == SafetyState::Fallback {
            facts.push(TraceFact::SafetyFallback);
        }
        if is_stop(source.action) {
            facts.push(TraceFact::StopCommanded);
        }
        let stationary = source.truth_from == source.truth_position;
        if stationary {
            facts.push(TraceFact::VehicleStationary);
        }
        if *restricted_zone
            .get(&tick)
            .ok_or_else(|| LabError(format!("missing restricted_zone result at tick {tick}")))?
        {
            facts.push(TraceFact::RestrictedZoneClear);
        }
        if source.mission_state == MissionState::Running {
            facts.push(TraceFact::MissionActive);
        }
        let mission_completed = transitions.get(&tick).is_some_and(|tick_transitions| {
            tick_transitions.iter().any(|transition| {
                transition.domain == TransitionDomain::Mission
                    && transition.to == TransitionState::Completed
            })
        });
        if mission_completed {
            facts.push(TraceFact::MissionCompleted);
        }
        if tick == scenario.steps {
            facts.push(TraceFact::MissionTerminated);
        }
        facts.push(TraceFact::MissionProgress {
            progress_mm: progress.total_mm,
        });

        let requested_movement = is_movement(source.action);
        if requested_movement {
            facts.push(TraceFact::MovementCommanded {
                command_id: source.event_sequence,
            });
        }
        let actual_movement = is_movement(outcome.realized_action);
        if source.safety_state == SafetyState::Fallback
            && source.truth_from != source.truth_position
        {
            facts.push(TraceFact::MovementWhileFallback);
        }
        if requested_movement {
            let delayed_this_command = outcome.fault_events.iter().any(|event| {
                event.source_command_tick == tick
                    && event.kind == crate::actuator::ActuatorEventKind::CommandDelayed
            });
            let failure_was_observed = outcome.fault_events.iter().any(|event| {
                event.source_command_tick == tick
                    && (event.kind == crate::actuator::ActuatorEventKind::CommandLost
                        || (event.kind
                            == crate::actuator::ActuatorEventKind::CurrentCommandIgnoredByDelayedCommand
                            && !delayed_this_command))
            });
            if failure_was_observed {
                facts.push(TraceFact::CommandResolved {
                    command_id: source.event_sequence,
                    outcome: CommandOutcome::ExplicitFailure,
                });
            }
        }
        if actual_movement && outcome.behavior != ActuatorBehavior::ContinuedMovement {
            let source_command = ticks.get(&outcome.source_command_tick).ok_or_else(|| {
                LabError(format!(
                    "actuator outcome at tick {tick} references unknown source command tick {}",
                    outcome.source_command_tick
                ))
            })?;
            if is_movement(source_command.action) {
                facts.push(TraceFact::CommandApplied {
                    command_id: source_command.event_sequence,
                });
                facts.push(TraceFact::CommandResolved {
                    command_id: source_command.event_sequence,
                    outcome: CommandOutcome::Applied,
                });
            }
        }
        let tick_transitions = transitions.remove(&tick).unwrap_or_default();
        result.push(TraceTick {
            tick,
            telemetry_present: source.observed.is_some(),
            facts,
            transitions: tick_transitions,
        });
    }
    if !restricted_zone.is_empty() && restricted_zone.keys().any(|tick| *tick > scenario.steps) {
        return Err(LabError(
            "restricted_zone results exist outside the configured trace".into(),
        ));
    }
    Ok(result)
}

fn build_failure_evidence(
    artifact: &EventsArtifact,
    actuation: &[ActuationOutcome],
    trace: &[TraceTick],
    results: &[ContractResult],
    reproduction_command: &str,
) -> Result<Vec<ContractFailureEvidence>, LabError> {
    let tick_sequences = tick_result_sequences(&artifact.events)?;
    let outcomes_by_tick: BTreeMap<_, _> = actuation
        .iter()
        .map(|outcome| (outcome.tick, outcome))
        .collect();
    let mut failures = Vec::new();
    for result in results
        .iter()
        .filter(|result| result.outcome == ContractOutcome::Fail)
    {
        let evidence = result
            .evidence
            .iter()
            .find(|evidence| evidence.outcome == ContractOutcome::Fail)
            .ok_or_else(|| {
                LabError(format!(
                    "failed contract {} has no failure evidence",
                    result.contract_id
                ))
            })?;
        let violation_tick = evidence
            .violation_tick
            .or(result.first_violation_tick)
            .ok_or_else(|| {
                LabError(format!(
                    "failed contract {} has no violation tick",
                    result.contract_id
                ))
            })?;
        let trigger_tick = evidence.trigger_tick.or(result.first_trigger_tick);
        let relevant_ticks = counterexample_tick_set(
            trigger_tick,
            evidence.deadline_tick,
            violation_tick,
            artifact.scenario.steps,
        );
        let source_commands = trigger_tick.into_iter().collect::<BTreeSet<_>>();
        let linked_events = link_fault_events(
            actuation,
            &tick_sequences,
            &relevant_ticks,
            &source_commands,
        )?;
        let counterexample_ticks = build_counterexample_ticks(
            artifact,
            actuation,
            &tick_sequences,
            &relevant_ticks,
            &linked_events,
        )?;
        let mut source_event_sequences = BTreeSet::new();
        for tick in &relevant_ticks {
            source_event_sequences.extend(
                artifact
                    .events
                    .iter()
                    .filter(|event| event.tick == *tick)
                    .map(|event| event.sequence),
            );
        }
        for event in &linked_events {
            source_event_sequences.insert(event.observed_event_sequence);
            source_event_sequences.insert(event.source_command_event_sequence);
            source_event_sequences.extend(event.execution_event_sequence);
        }
        let trigger_command = trigger_tick
            .map(|tick| command_at_tick(artifact, &tick_sequences, tick))
            .transpose()?;
        let violation_command = Some(command_at_tick(artifact, &tick_sequences, violation_tick)?);
        let trigger_source = trigger_tick.and_then(|tick| outcomes_by_tick.get(&tick).copied());
        let violation_source = outcomes_by_tick.get(&violation_tick).copied();
        let violation_event = artifact
            .events
            .iter()
            .find(|event| {
                event.tick == violation_tick
                    && matches!(event.event, EventPayload::TickResult { .. })
            })
            .ok_or_else(|| {
                LabError(format!(
                    "missing tick result at violation tick {violation_tick}"
                ))
            })?;
        let (truth_from, truth_position) = match &violation_event.event {
            EventPayload::TickResult {
                truth_from,
                truth_position,
                ..
            } => (*truth_from, *truth_position),
            _ => unreachable!("filtered to tick result"),
        };
        let actual_trace_tick = trace
            .get(violation_tick.saturating_sub(1) as usize)
            .ok_or_else(|| {
                LabError(format!(
                    "missing temporal observation at violation tick {violation_tick}"
                ))
            })?;
        let actual_source = artifact
            .expected_report
            .telemetry
            .iter()
            .find(|telemetry| telemetry.tick == violation_tick)
            .ok_or_else(|| {
                LabError(format!(
                    "missing telemetry at violation tick {violation_tick}"
                ))
            })?;
        let actual_outcome = outcomes_by_tick.get(&violation_tick).copied();
        let actual_condition = format!(
            "trace_facts={:?}; mission_state={}; safety_state={}; truth_from=({},{}); truth_position=({},{}); controller_action={:?}; realized_action={:?}; actuator_behavior={:?}",
            actual_trace_tick.facts,
            actual_source.mission_state,
            actual_source.safety_state,
            truth_from.x_mm,
            truth_from.y_mm,
            truth_position.x_mm,
            truth_position.y_mm,
            actual_source.action,
            actual_outcome.map(|outcome| outcome.realized_action),
            actual_outcome.map(|outcome| outcome.behavior),
        );
        failures.push(ContractFailureEvidence {
            contract_id: result.contract_id.clone(),
            contract_version: result.schema_version,
            property_type: result.property_type,
            first_trigger_tick: trigger_tick,
            applicable_deadline_tick: evidence.deadline_tick.or(result.deadline_tick),
            first_violation_tick: violation_tick,
            expected: evidence.expected.clone(),
            expected_condition: format!("{:?}", evidence.expected),
            actual_condition,
            actual_fact: evidence.observed_fact,
            actual_transition: evidence.observed_transition,
            observation: evidence.observation,
            evidence_reason: evidence.reason,
            monitor_derived_command_outcome: evidence.monitor_derived_command_outcome,
            detail: evidence.detail.clone(),
            trigger_command,
            violation_command,
            actuator_behavior_at_trigger: trigger_source.map(|outcome| outcome.behavior),
            actuator_behavior_at_violation: violation_source.map(|outcome| outcome.behavior),
            truth_from: Some(truth_from),
            truth_position: Some(truth_position),
            fault_events: linked_events,
            source_event_sequences: source_event_sequences.into_iter().collect(),
            counterexample_ticks,
            source_artifact_sha256: artifact.artifact_sha256.clone(),
            reproduction_command: reproduction_command.to_owned(),
        });
    }
    Ok(failures)
}

fn tick_result_sequences(events: &[EventRecord]) -> Result<BTreeMap<u32, u64>, LabError> {
    let mut sequences = BTreeMap::new();
    for event in events {
        if matches!(event.event, EventPayload::TickResult { .. })
            && sequences.insert(event.tick, event.sequence).is_some()
        {
            return Err(LabError(format!(
                "duplicate tick result event at tick {}",
                event.tick
            )));
        }
    }
    Ok(sequences)
}

fn command_at_tick(
    artifact: &EventsArtifact,
    tick_sequences: &BTreeMap<u32, u64>,
    tick: u32,
) -> Result<CommandAtTick, LabError> {
    let telemetry = artifact
        .expected_report
        .telemetry
        .iter()
        .find(|telemetry| telemetry.tick == tick)
        .ok_or_else(|| LabError(format!("missing telemetry at tick {tick}")))?;
    let event_sequence = tick_sequences
        .get(&tick)
        .copied()
        .ok_or_else(|| LabError(format!("missing tick result event at tick {tick}")))?;
    Ok(CommandAtTick {
        tick,
        event_sequence,
        action: telemetry.action,
    })
}

fn counterexample_tick_set(
    trigger_tick: Option<u32>,
    deadline_tick: Option<u64>,
    violation_tick: u32,
    total_ticks: u32,
) -> BTreeSet<u32> {
    let mut ticks = BTreeSet::new();
    let first = trigger_tick
        .unwrap_or(violation_tick)
        .saturating_sub(1)
        .max(1);
    let last_obligation_tick = deadline_tick
        .and_then(|tick| u32::try_from(tick).ok())
        .unwrap_or(violation_tick)
        .max(violation_tick);
    let last = last_obligation_tick.saturating_add(1).min(total_ticks);
    for tick in first..=last {
        ticks.insert(tick);
    }
    ticks
}

fn link_fault_events(
    actuation: &[ActuationOutcome],
    tick_sequences: &BTreeMap<u32, u64>,
    relevant_ticks: &BTreeSet<u32>,
    source_command_ticks: &BTreeSet<u32>,
) -> Result<Vec<LinkedActuatorFaultEvent>, LabError> {
    let mut result = Vec::new();
    for outcome in actuation {
        for event in &outcome.fault_events {
            if !relevant_ticks.contains(&event.tick)
                && !source_command_ticks.contains(&event.source_command_tick)
            {
                continue;
            }
            let observed_event_sequence = sequence_for_tick(tick_sequences, event.tick)?;
            let source_command_event_sequence =
                sequence_for_tick(tick_sequences, event.source_command_tick)?;
            let execution_event_sequence = event
                .execution_tick
                .map(|tick| sequence_for_tick(tick_sequences, tick))
                .transpose()?;
            result.push(LinkedActuatorFaultEvent {
                event: event.clone(),
                observed_event_sequence,
                source_command_event_sequence,
                execution_event_sequence,
            });
        }
    }
    Ok(result)
}

fn sequence_for_tick(tick_sequences: &BTreeMap<u32, u64>, tick: u32) -> Result<u64, LabError> {
    tick_sequences
        .get(&tick)
        .copied()
        .ok_or_else(|| LabError(format!("event references missing tick result {tick}")))
}

fn build_counterexample_ticks(
    artifact: &EventsArtifact,
    actuation: &[ActuationOutcome],
    tick_sequences: &BTreeMap<u32, u64>,
    selected_ticks: &BTreeSet<u32>,
    linked_events: &[LinkedActuatorFaultEvent],
) -> Result<Vec<CounterexampleTick>, LabError> {
    let outcomes_by_tick: BTreeMap<_, _> = actuation
        .iter()
        .map(|outcome| (outcome.tick, outcome))
        .collect();
    let mut result = Vec::with_capacity(selected_ticks.len());
    for tick in selected_ticks {
        let event = artifact
            .events
            .iter()
            .find(|event| {
                event.tick == *tick && matches!(event.event, EventPayload::TickResult { .. })
            })
            .ok_or_else(|| LabError(format!("missing tick result at tick {tick}")))?;
        let (truth_from, truth_position) = match event.event {
            EventPayload::TickResult {
                truth_from,
                truth_position,
                ..
            } => (truth_from, truth_position),
            _ => unreachable!("filtered to tick result"),
        };
        let telemetry = artifact
            .expected_report
            .telemetry
            .iter()
            .find(|telemetry| telemetry.tick == *tick)
            .ok_or_else(|| LabError(format!("missing telemetry at tick {tick}")))?;
        let outcome = outcomes_by_tick
            .get(tick)
            .copied()
            .ok_or_else(|| LabError(format!("missing actuator outcome at tick {tick}")))?;
        let fault_events = linked_events
            .iter()
            .filter(|linked| linked.event.tick == *tick)
            .cloned()
            .collect();
        let source_event_sequences = artifact
            .events
            .iter()
            .filter(|source| source.tick == *tick)
            .map(|source| source.sequence)
            .chain(std::iter::once(sequence_for_tick(tick_sequences, *tick)?))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        result.push(CounterexampleTick {
            tick: *tick,
            truth_from,
            truth_position,
            observed: telemetry.observed.clone(),
            mission_state: telemetry.mission_state,
            safety_state: telemetry.safety_state,
            requested_action: telemetry.action,
            realized_action: outcome.realized_action,
            actuator_behavior: outcome.behavior,
            fault_events,
            source_event_sequences,
        });
    }
    Ok(result)
}

fn is_stop(action: Action) -> bool {
    action.heading.is_none() && action.distance_mm == 0
}

fn is_movement(action: Action) -> bool {
    action.heading.is_some() && action.distance_mm > 0
}

fn transition_domain(subsystem: &str) -> Option<TransitionDomain> {
    match subsystem {
        "mission" => Some(TransitionDomain::Mission),
        "safety" => Some(TransitionDomain::Safety),
        _ => None,
    }
}

fn transition_state(state: &str) -> Result<TransitionState, LabError> {
    match state {
        "pending" => Ok(TransitionState::Pending),
        "running" => Ok(TransitionState::Running),
        "completed" => Ok(TransitionState::Completed),
        "nominal" => Ok(TransitionState::Nominal),
        "fallback" => Ok(TransitionState::Fallback),
        _ => Err(LabError(format!("unknown transition state '{state}'"))),
    }
}

#[derive(Clone)]
struct SourceTick {
    event_sequence: u64,
    truth_from: Point,
    truth_position: Point,
    observed: Option<ObservedTelemetry>,
    mission_state: MissionState,
    safety_state: SafetyState,
    action: Action,
}

struct MissionProgressTracker {
    waypoint_index: usize,
    best_distance_mm: Option<i64>,
    total_mm: u64,
}

impl MissionProgressTracker {
    fn new() -> Self {
        Self {
            waypoint_index: 0,
            best_distance_mm: None,
            total_mm: 0,
        }
    }

    fn sample(&mut self, position: Point, scenario: &Scenario) {
        while let Some(target) = scenario.mission.waypoints.get(self.waypoint_index).copied() {
            let distance = position.manhattan_distance(target);
            if let Some(best) = self.best_distance_mm {
                if distance < best {
                    self.total_mm = self.total_mm.saturating_add((best - distance) as u64);
                    self.best_distance_mm = Some(distance);
                }
            } else {
                self.best_distance_mm = Some(distance);
            }
            if !position.within_radius(target, scenario.mission.arrival_radius_mm) {
                break;
            }
            self.waypoint_index += 1;
            self.best_distance_mm = None;
        }
    }
}

fn recompute_artifact(artifact: &EventsArtifact) -> Result<crate::model::RunReport, LabError> {
    Ok(recompute_core(artifact, None)?.report)
}

fn recompute_core(
    artifact: &EventsArtifact,
    actuator_config: Option<&ActuatorConfig>,
) -> Result<CoreOutput, LabError> {
    artifact
        .scenario
        .validate()
        .map_err(|error| LabError(error.to_string()))?;
    verify_artifact_integrity(artifact)?;
    if let Some(config) = actuator_config {
        config.validate(artifact.scenario.steps).map_err(LabError)?;
    }
    let exogenous = extract_exogenous(artifact)?;
    let replay = if let Some(config) = actuator_config {
        execute_with_actuator(&artifact.scenario, artifact.seed, Some(&exogenous), config)?
    } else {
        execute(&artifact.scenario, artifact.seed, Some(&exogenous))?
    };
    if replay.events != artifact.events {
        return Err(LabError(
            "replay event stream differs from the recorded event stream".into(),
        ));
    }
    if replay.report != artifact.expected_report {
        return Err(LabError(
            "recomputed report differs from expected_report".into(),
        ));
    }
    Ok(replay)
}

pub fn parse_scenario(text: &str) -> Result<Scenario, LabError> {
    if text.len() > MAX_SCENARIO_BYTES {
        return Err(LabError("scenario file exceeds the 2 MiB limit".into()));
    }
    Scenario::from_json(text).map_err(LabError)
}

pub fn parse_artifact(text: &str) -> Result<EventsArtifact, LabError> {
    if text.len() > MAX_ARTIFACT_BYTES {
        return Err(LabError("events artifact exceeds the 128 MiB limit".into()));
    }
    serde_json::from_str(text).map_err(|error| LabError(error.to_string()))
}

pub fn parse_verification_artifact(text: &str) -> Result<VerificationArtifact, LabError> {
    if text.len() > MAX_VERIFICATION_BYTES {
        return Err(LabError(
            "verification artifact exceeds the 32 MiB limit".into(),
        ));
    }
    let verification: VerificationArtifact =
        serde_json::from_str(text).map_err(|error| LabError(error.to_string()))?;
    validate_verification_shape(&verification)?;
    Ok(verification)
}

fn validate_verification_shape(verification: &VerificationArtifact) -> Result<(), LabError> {
    if verification.schema_version != VERIFICATION_SCHEMA_VERSION {
        return Err(LabError(
            "unsupported verification artifact schema_version".into(),
        ));
    }
    validate_sha256(
        &verification.source_artifact_sha256,
        "source_artifact_sha256",
    )?;
    validate_sha256(&verification.source_final_hash, "source_final_hash")?;
    validate_sha256(&verification.actuator_trace_sha256, "actuator_trace_sha256")?;
    validate_sha256(&verification.verification_sha256, "verification_sha256")?;
    verification
        .contract_set
        .validate()
        .map_err(|error| LabError(error.to_string()))?;
    verification.mission_policy.validate()?;
    verification
        .actuator_config
        .validate(10_000)
        .map_err(LabError)?;
    if verification.reproduction_command.trim().is_empty()
        || verification.reproduction_command.len() > 4096
    {
        return Err(LabError(
            "reproduction_command must contain 1 to 4096 bytes".into(),
        ));
    }
    if verification.contract_results.len() != verification.contract_set.contracts.len() {
        return Err(LabError(
            "contract_results count does not match the contract set".into(),
        ));
    }
    let mut counts = (0_u32, 0_u32, 0_u32);
    for (contract, result) in verification
        .contract_set
        .contracts
        .iter()
        .zip(&verification.contract_results)
    {
        if result.contract_id != contract.id()
            || result.schema_version != verification.contract_set.schema_version
            || result.property_type != contract.property_type()
        {
            return Err(LabError(
                "contract result identity differs from the declared contract".into(),
            ));
        }
        match result.outcome {
            ContractOutcome::Pass => counts.0 = counts.0.saturating_add(1),
            ContractOutcome::Fail => counts.1 = counts.1.saturating_add(1),
            ContractOutcome::Inconclusive => counts.2 = counts.2.saturating_add(1),
        }
    }
    let temporal_status = if counts.1 > 0 {
        ContractOutcome::Fail
    } else if counts.2 > 0 {
        ContractOutcome::Inconclusive
    } else {
        ContractOutcome::Pass
    };
    if verification.summary.contracts_passed != counts.0
        || verification.summary.contracts_failed != counts.1
        || verification.summary.contracts_inconclusive != counts.2
        || verification.summary.temporal_status != temporal_status
        || verification.failures.len() != counts.1 as usize
    {
        return Err(LabError(
            "verification summary does not match contract results".into(),
        ));
    }
    if verification
        .failures
        .iter()
        .any(|failure| failure.source_artifact_sha256 != verification.source_artifact_sha256)
    {
        return Err(LabError(
            "contract failure evidence references a different source artifact".into(),
        ));
    }
    if verification.verification_sha256 != verification_digest(verification) {
        return Err(LabError(
            "verification_sha256 does not match the verification artifact".into(),
        ));
    }
    Ok(())
}

fn verify_verification_integrity(
    verification: &VerificationArtifact,
    artifact: &EventsArtifact,
) -> Result<(), LabError> {
    validate_verification_shape(verification)?;
    if verification.source_artifact_sha256 != artifact.artifact_sha256
        || verification.source_final_hash != artifact.final_hash
    {
        return Err(LabError(
            "verification artifact does not reference the supplied events artifact".into(),
        ));
    }
    verification
        .actuator_config
        .validate(artifact.scenario.steps)
        .map_err(LabError)?;
    Ok(())
}

fn validate_sha256(value: &str, field: &str) -> Result<(), LabError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(LabError(format!(
            "{field} must be a lowercase SHA-256 digest"
        )));
    }
    Ok(())
}

pub fn status_exit_code(status: RunStatus) -> u8 {
    if status == RunStatus::Passed { 0 } else { 2 }
}
