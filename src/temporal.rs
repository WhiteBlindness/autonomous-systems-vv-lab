//! Deterministic finite-trace monitoring for declarative verification contracts.
//!
//! Tick numbers start at 1 and must be contiguous. A bounded response with
//! `within_ticks = N` accepts a response on the trigger tick or through
//! `trigger_tick + N`, inclusive. Thus a bound of zero permits only a same-tick
//! response. Facts on the tick are simultaneous: triggers are registered before
//! responses are matched, so a same-tick response can satisfy its new obligation.
//! For an uncorrelated response contract, one observed response discharges every
//! obligation for that contract that is still open on the tick. Command-scoped
//! responses instead discharge only the obligation with the matching command id.
//!
//! A known positive fact is usable even when sensor telemetry is missing. An
//! absent `localization_unreliable` fact is unknown on a telemetry-missing tick;
//! other supported conditions are reconstructed from controller, actuator, and
//! ground-truth event data, so their absence on a recorded tick is false. A trace
//! that ends with an unresolved obligation is INCONCLUSIVE. A failure is reported
//! only when all observations needed to disprove the obligation are present.
//! Mission completion does not end bounded-response monitoring: stop and command
//! obligations remain observable through the configured trace horizon. Bounded
//! progress applies only while `mission_active` is true; completion before its
//! deadline closes the window as INCONCLUSIVE unless the required increase was
//! already observed.
//! `mission_terminated` marks the final configured trace tick and may coexist
//! with `mission_active`, which describes that tick's state. The distinct
//! `mission_completed` event records logical mission completion; it does not
//! end response monitoring.
//! An unknown response observation does not count as a response. If it occurs
//! during an obligation's window and no known response arrives by the deadline,
//! that obligation is INCONCLUSIVE rather than FAIL.
//!
//! PASS means the property held over the supplied finite observation interval. It
//! does not establish a mathematical guarantee over every possible future run.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::contracts::{
    CommandOutcome, Contract, ContractSet, Correlation, Predicate, PropertyType, TraceFact,
    TraceFactIdentity, TransitionDomain, TransitionEdge, TransitionState,
};

const MAX_TRACE_TICKS: usize = 10_000;

/// One recorded observation step presented to the finite-trace monitor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraceTick {
    /// Logical tick number, starting at 1.
    pub tick: u32,
    /// Whether sensor telemetry was available for this tick.
    pub telemetry_present: bool,
    /// Positive typed facts; their source remains the simulator or event stream.
    pub facts: Vec<TraceFact>,
    /// Transition events in source-event order for this tick.
    pub transitions: Vec<TraceTransition>,
}

/// One typed state-machine transition with its source event sequence number.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraceTransition {
    pub source_sequence: u64,
    pub domain: TransitionDomain,
    pub from: TransitionState,
    pub to: TransitionState,
}

impl TraceTransition {
    fn edge(self) -> TransitionEdge {
        TransitionEdge {
            from: self.from,
            to: self.to,
        }
    }
}

/// Stable three-way result of finite-trace evaluation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContractOutcome {
    Pass,
    Fail,
    Inconclusive,
}

/// One report row for a declared contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContractResult {
    /// Contract document version used during evaluation.
    pub schema_version: u32,
    pub contract_id: String,
    pub property_type: PropertyType,
    pub outcome: ContractOutcome,
    /// Earliest known trigger associated with this result, when applicable.
    pub first_trigger_tick: Option<u32>,
    /// Deadline corresponding to the earliest failure, or first unresolved trigger.
    pub deadline_tick: Option<u64>,
    /// Earliest tick with sufficient evidence of a violation.
    pub first_violation_tick: Option<u32>,
    /// Per-obligation or per-violation evidence, in deterministic trace order.
    pub evidence: Vec<ContractEvidence>,
}

/// Typed evidence for one obligation, violation, or unknown observation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContractEvidence {
    pub outcome: ContractOutcome,
    pub trigger_tick: Option<u32>,
    pub deadline_tick: Option<u64>,
    pub violation_tick: Option<u32>,
    /// Index into the input trace for the qualifying trigger observation.
    pub trigger_trace_index: Option<usize>,
    /// Index into the input trace for the failure or observation that closed it.
    pub violation_trace_index: Option<usize>,
    pub expected: ExpectedCondition,
    pub observed_fact: Option<TraceFact>,
    pub observed_transition: Option<TraceTransition>,
    /// A terminal outcome inferred for this obligation only. It is not an
    /// actuator event and cannot satisfy another contract.
    pub monitor_derived_command_outcome: Option<CommandOutcome>,
    pub observation: ObservationState,
    pub reason: EvidenceReason,
    /// Stable diagnostic context, such as the correlated command id.
    pub detail: Option<String>,
}

/// Typed condition or transition that the evidence is about.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExpectedCondition {
    Predicate {
        predicate: Predicate,
    },
    Transition {
        edge: TransitionEdge,
    },
    NoTransition {
        domain: TransitionDomain,
    },
    MissionProgressAtLeast {
        baseline_mm: u64,
        minimum_increase_mm: u64,
        required_total_mm: u64,
    },
    TraceIntegrity,
}

/// What the monitor could establish at the cited trace index.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationState {
    KnownTrue,
    KnownFalse,
    Unknown,
    MonitorDerived,
    TransitionObserved,
    Invalid,
}

/// Reason an evidence row was produced.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceReason {
    ConditionObserved,
    PredicateFalse,
    ProhibitedPredicateObserved,
    DeadlineExpired,
    MissingObservation,
    MissingProgressMeasurement,
    TraceEndedBeforeDeadline,
    MissionTerminatedBeforeDeadline,
    ProgressAchieved,
    ProgressNotReached,
    TransitionOrderMismatch,
    TransitionSequenceIncomplete,
    CommandTimedOutDerived,
    InvalidTrace,
    InvalidContractSet,
}

/// Evaluate all contracts against the supplied finite trace.
///
/// The function is deterministic and does not mutate either input. A malformed
/// trace or invalid in-memory contract set yields INCONCLUSIVE results with a
/// typed integrity reason instead of panicking or silently treating missing data
/// as false.
pub fn evaluate(contracts: &ContractSet, trace: &[TraceTick]) -> Vec<ContractResult> {
    if let Err(error) = contracts.validate() {
        return contracts
            .contracts
            .iter()
            .map(|contract| {
                invalid_result(
                    contracts.schema_version,
                    contract,
                    EvidenceReason::InvalidContractSet,
                    error.to_string(),
                )
            })
            .collect();
    }
    if let Err(error) = validate_trace(trace) {
        return contracts
            .contracts
            .iter()
            .map(|contract| {
                invalid_result(
                    contracts.schema_version,
                    contract,
                    EvidenceReason::InvalidTrace,
                    error.clone(),
                )
            })
            .collect();
    }

    contracts
        .contracts
        .iter()
        .map(|contract| {
            let evidence = match contract {
                Contract::Always { predicate, .. } => evaluate_always(*predicate, trace),
                Contract::Never { predicate, .. } => evaluate_never(*predicate, trace),
                Contract::BoundedResponse {
                    trigger,
                    required,
                    within_ticks,
                    correlation,
                    trigger_for_ticks,
                    ..
                } => evaluate_bounded_response(
                    *trigger,
                    *required,
                    *within_ticks,
                    *correlation,
                    *trigger_for_ticks,
                    trace,
                ),
                Contract::OrderedTransition {
                    domain, sequence, ..
                } => evaluate_ordered_transition(*domain, sequence, trace),
                Contract::BoundedProgress {
                    within_ticks,
                    minimum_increase_mm,
                    ..
                } => evaluate_bounded_progress(*within_ticks, *minimum_increase_mm, trace),
            };
            build_result(contracts.schema_version, contract, evidence)
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObservedCondition {
    True(Option<TraceFact>),
    False,
    Unknown,
}

fn observe(predicate: Predicate, tick: &TraceTick) -> ObservedCondition {
    if let Some(fact) = tick
        .facts
        .iter()
        .copied()
        .find(|fact| fact.matches(predicate))
    {
        return ObservedCondition::True(Some(fact));
    }
    if predicate == Predicate::LocalizationUnreliable && !tick.telemetry_present {
        ObservedCondition::Unknown
    } else {
        ObservedCondition::False
    }
}

fn evaluate_always(predicate: Predicate, trace: &[TraceTick]) -> Vec<ContractEvidence> {
    let mut evidence = Vec::new();
    for (index, tick) in trace.iter().enumerate() {
        match observe(predicate, tick) {
            ObservedCondition::True(_) => {}
            ObservedCondition::False => evidence.push(evidence_row(EvidenceDraft {
                outcome: ContractOutcome::Fail,
                trigger_tick: None,
                deadline_tick: None,
                violation_tick: Some(tick.tick),
                trigger_trace_index: None,
                violation_trace_index: Some(index),
                expected: ExpectedCondition::Predicate { predicate },
                observed_fact: None,
                observed_transition: None,
                observation: ObservationState::KnownFalse,
                reason: EvidenceReason::PredicateFalse,
                detail: None,
            })),
            ObservedCondition::Unknown => evidence.push(evidence_row(EvidenceDraft {
                outcome: ContractOutcome::Inconclusive,
                trigger_tick: None,
                deadline_tick: None,
                violation_tick: None,
                trigger_trace_index: None,
                violation_trace_index: Some(index),
                expected: ExpectedCondition::Predicate { predicate },
                observed_fact: None,
                observed_transition: None,
                observation: ObservationState::Unknown,
                reason: EvidenceReason::MissingObservation,
                detail: None,
            })),
        }
    }
    evidence
}

fn evaluate_never(predicate: Predicate, trace: &[TraceTick]) -> Vec<ContractEvidence> {
    let mut evidence = Vec::new();
    for (index, tick) in trace.iter().enumerate() {
        match observe(predicate, tick) {
            ObservedCondition::True(fact) => evidence.push(evidence_row(EvidenceDraft {
                outcome: ContractOutcome::Fail,
                trigger_tick: None,
                deadline_tick: None,
                violation_tick: Some(tick.tick),
                trigger_trace_index: None,
                violation_trace_index: Some(index),
                expected: ExpectedCondition::Predicate { predicate },
                observed_fact: fact,
                observed_transition: None,
                observation: ObservationState::KnownTrue,
                reason: EvidenceReason::ProhibitedPredicateObserved,
                detail: None,
            })),
            ObservedCondition::False => {}
            ObservedCondition::Unknown => evidence.push(evidence_row(EvidenceDraft {
                outcome: ContractOutcome::Inconclusive,
                trigger_tick: None,
                deadline_tick: None,
                violation_tick: None,
                trigger_trace_index: None,
                violation_trace_index: Some(index),
                expected: ExpectedCondition::Predicate { predicate },
                observed_fact: None,
                observed_transition: None,
                observation: ObservationState::Unknown,
                reason: EvidenceReason::MissingObservation,
                detail: None,
            })),
        }
    }
    evidence
}

#[derive(Clone, Debug)]
struct PendingResponse {
    trigger_tick: u32,
    deadline_tick: u64,
    trigger_index: usize,
    command_id: Option<u64>,
    unknown_count_at_trigger: u64,
}

fn evaluate_bounded_response(
    trigger: Predicate,
    required: Predicate,
    within_ticks: u32,
    correlation: Option<Correlation>,
    trigger_for_ticks: u32,
    trace: &[TraceTick],
) -> Vec<ContractEvidence> {
    if correlation == Some(Correlation::CommandId) {
        return evaluate_correlated_response(required, within_ticks, trace);
    }
    let mut evidence = Vec::new();
    let mut pending = VecDeque::<PendingResponse>::new();
    let mut trigger_streak = 0_u32;
    let mut qualified_run = false;
    let mut unknown_response_count = 0_u64;

    for (index, tick) in trace.iter().enumerate() {
        match observe(trigger, tick) {
            ObservedCondition::True(_) => {
                trigger_streak = trigger_streak.saturating_add(1);
                if !qualified_run && trigger_streak >= trigger_for_ticks {
                    pending.push_back(PendingResponse {
                        trigger_tick: tick.tick,
                        deadline_tick: u64::from(tick.tick) + u64::from(within_ticks),
                        trigger_index: index,
                        command_id: None,
                        unknown_count_at_trigger: unknown_response_count,
                    });
                    qualified_run = true;
                }
            }
            ObservedCondition::False => {
                trigger_streak = 0;
                qualified_run = false;
            }
            ObservedCondition::Unknown => {
                trigger_streak = 0;
                qualified_run = false;
                evidence.push(evidence_row(EvidenceDraft {
                    outcome: ContractOutcome::Inconclusive,
                    trigger_tick: None,
                    deadline_tick: None,
                    violation_tick: None,
                    trigger_trace_index: None,
                    violation_trace_index: Some(index),
                    expected: ExpectedCondition::Predicate { predicate: trigger },
                    observed_fact: None,
                    observed_transition: None,
                    observation: ObservationState::Unknown,
                    reason: EvidenceReason::MissingObservation,
                    detail: Some("trigger could not be determined at this tick".into()),
                }));
            }
        }

        let response = observe(required, tick);
        if response == ObservedCondition::Unknown {
            unknown_response_count += 1;
        }
        if let ObservedCondition::True(fact) = response {
            while let Some(obligation) = pending.pop_front() {
                evidence.push(evidence_row(EvidenceDraft {
                    outcome: ContractOutcome::Pass,
                    trigger_tick: Some(obligation.trigger_tick),
                    deadline_tick: Some(obligation.deadline_tick),
                    violation_tick: None,
                    trigger_trace_index: Some(obligation.trigger_index),
                    violation_trace_index: Some(index),
                    expected: ExpectedCondition::Predicate {
                        predicate: required,
                    },
                    observed_fact: fact,
                    observed_transition: None,
                    observation: ObservationState::KnownTrue,
                    reason: EvidenceReason::ConditionObserved,
                    detail: None,
                }));
            }
        } else {
            while pending
                .front()
                .is_some_and(|obligation| u64::from(tick.tick) == obligation.deadline_tick)
            {
                let obligation = pending.pop_front().expect("front was checked");
                let unknown = unknown_response_count > obligation.unknown_count_at_trigger;
                let outcome = if unknown {
                    ContractOutcome::Inconclusive
                } else {
                    ContractOutcome::Fail
                };
                evidence.push(evidence_row(EvidenceDraft {
                    outcome,
                    trigger_tick: Some(obligation.trigger_tick),
                    deadline_tick: Some(obligation.deadline_tick),
                    violation_tick: (outcome == ContractOutcome::Fail).then_some(tick.tick),
                    trigger_trace_index: Some(obligation.trigger_index),
                    violation_trace_index: Some(index),
                    expected: ExpectedCondition::Predicate {
                        predicate: required,
                    },
                    observed_fact: None,
                    observed_transition: None,
                    observation: if unknown {
                        ObservationState::Unknown
                    } else {
                        ObservationState::KnownFalse
                    },
                    reason: if unknown {
                        EvidenceReason::MissingObservation
                    } else {
                        EvidenceReason::DeadlineExpired
                    },
                    detail: None,
                }));
            }
        }
    }

    if let Some(last_tick) = trace.last() {
        while let Some(obligation) = pending.pop_front() {
            evidence.push(evidence_row(EvidenceDraft {
                outcome: ContractOutcome::Inconclusive,
                trigger_tick: Some(obligation.trigger_tick),
                deadline_tick: Some(obligation.deadline_tick),
                violation_tick: None,
                trigger_trace_index: Some(obligation.trigger_index),
                violation_trace_index: Some(trace.len() - 1),
                expected: ExpectedCondition::Predicate {
                    predicate: required,
                },
                observed_fact: None,
                observed_transition: None,
                observation: ObservationState::Unknown,
                reason: EvidenceReason::TraceEndedBeforeDeadline,
                detail: Some(format!(
                    "trace ended at tick {}; command_id={:?}",
                    last_tick.tick, obligation.command_id
                )),
            }));
        }
    }

    evidence
}

/// Use keyed maps for per-command obligations so one command response cannot
/// satisfy another command and monitoring remains linear in the trace length.
fn evaluate_correlated_response(
    required: Predicate,
    within_ticks: u32,
    trace: &[TraceTick],
) -> Vec<ContractEvidence> {
    let mut evidence = Vec::new();
    let mut pending = BTreeMap::<u64, PendingResponse>::new();
    let mut deadlines = BTreeMap::<u64, Vec<u64>>::new();

    for (index, tick) in trace.iter().enumerate() {
        for fact in &tick.facts {
            if let TraceFact::MovementCommanded { command_id } = fact {
                let deadline_tick = u64::from(tick.tick) + u64::from(within_ticks);
                pending.insert(
                    *command_id,
                    PendingResponse {
                        trigger_tick: tick.tick,
                        deadline_tick,
                        trigger_index: index,
                        command_id: Some(*command_id),
                        unknown_count_at_trigger: 0,
                    },
                );
                deadlines
                    .entry(deadline_tick)
                    .or_default()
                    .push(*command_id);
            }
        }

        for fact in &tick.facts {
            let Some(command_id) = fact.command_id() else {
                continue;
            };
            if !fact.matches(required) {
                continue;
            }
            if let Some(obligation) = pending.remove(&command_id) {
                evidence.push(evidence_row(EvidenceDraft {
                    outcome: ContractOutcome::Pass,
                    trigger_tick: Some(obligation.trigger_tick),
                    deadline_tick: Some(obligation.deadline_tick),
                    violation_tick: None,
                    trigger_trace_index: Some(obligation.trigger_index),
                    violation_trace_index: Some(index),
                    expected: ExpectedCondition::Predicate {
                        predicate: required,
                    },
                    observed_fact: Some(*fact),
                    observed_transition: None,
                    observation: ObservationState::KnownTrue,
                    reason: EvidenceReason::ConditionObserved,
                    detail: Some(format!("command_id={command_id}")),
                }));
            }
        }

        if let Some(command_ids) = deadlines.remove(&u64::from(tick.tick)) {
            for command_id in command_ids {
                let Some(obligation) = pending.remove(&command_id) else {
                    continue;
                };
                let mut row = evidence_row(EvidenceDraft {
                    outcome: ContractOutcome::Fail,
                    trigger_tick: Some(obligation.trigger_tick),
                    deadline_tick: Some(obligation.deadline_tick),
                    violation_tick: Some(tick.tick),
                    trigger_trace_index: Some(obligation.trigger_index),
                    violation_trace_index: Some(index),
                    expected: ExpectedCondition::Predicate {
                        predicate: required,
                    },
                    observed_fact: None,
                    observed_transition: None,
                    observation: ObservationState::MonitorDerived,
                    reason: EvidenceReason::CommandTimedOutDerived,
                    detail: Some(format!(
                        "command_id={command_id}; required {} absent at deadline",
                        if required == Predicate::CommandApplied {
                            "command application"
                        } else {
                            "actuator resolution"
                        }
                    )),
                });
                row.monitor_derived_command_outcome = Some(CommandOutcome::Timeout);
                evidence.push(row);
            }
        }
    }

    for (command_id, obligation) in pending {
        evidence.push(evidence_row(EvidenceDraft {
            outcome: ContractOutcome::Inconclusive,
            trigger_tick: Some(obligation.trigger_tick),
            deadline_tick: Some(obligation.deadline_tick),
            violation_tick: None,
            trigger_trace_index: Some(obligation.trigger_index),
            violation_trace_index: Some(trace.len() - 1),
            expected: ExpectedCondition::Predicate {
                predicate: required,
            },
            observed_fact: None,
            observed_transition: None,
            observation: ObservationState::Unknown,
            reason: EvidenceReason::TraceEndedBeforeDeadline,
            detail: Some(format!("command_id={command_id}")),
        }));
    }
    evidence
}

fn evaluate_ordered_transition(
    domain: TransitionDomain,
    sequence: &[TransitionEdge],
    trace: &[TraceTick],
) -> Vec<ContractEvidence> {
    let mut evidence = Vec::new();
    let mut next_edge = 0;
    let mut first_edge_tick = None;
    let mut first_edge_index = None;

    'ticks: for (index, tick) in trace.iter().enumerate() {
        for transition in tick
            .transitions
            .iter()
            .copied()
            .filter(|item| item.domain == domain)
        {
            if next_edge >= sequence.len() {
                evidence.push(evidence_row(EvidenceDraft {
            outcome: ContractOutcome::Fail,
            trigger_tick: first_edge_tick,
            deadline_tick: None,
            violation_tick: Some(tick.tick),
            trigger_trace_index: first_edge_index,
            violation_trace_index: Some(index),
            expected: ExpectedCondition::NoTransition { domain },
            observed_fact: None,
            observed_transition: Some(transition),
            observation: ObservationState::TransitionObserved,
            reason: EvidenceReason::TransitionOrderMismatch,
            detail: Some(format!(
                        "additional transition {:?}->{:?} at source sequence {} follows the declared sequence",
                        transition.from, transition.to, transition.source_sequence
                    )),
        }));
                break 'ticks;
            }
            let expected = sequence[next_edge];
            if transition.edge() != expected {
                evidence.push(evidence_row(EvidenceDraft {
                    outcome: ContractOutcome::Fail,
                    trigger_tick: first_edge_tick,
                    deadline_tick: None,
                    violation_tick: Some(tick.tick),
                    trigger_trace_index: first_edge_index,
                    violation_trace_index: Some(index),
                    expected: ExpectedCondition::Transition { edge: expected },
                    observed_fact: None,
                    observed_transition: Some(transition),
                    observation: ObservationState::TransitionObserved,
                    reason: EvidenceReason::TransitionOrderMismatch,
                    detail: Some(format!(
                        "observed {:?}->{:?} at source sequence {}",
                        transition.from, transition.to, transition.source_sequence
                    )),
                }));
                break 'ticks;
            }
            first_edge_tick.get_or_insert(tick.tick);
            first_edge_index.get_or_insert(index);
            next_edge += 1;
            if next_edge == sequence.len() {
                // The declared sequence is exact. An additional transition in
                // this domain is also outside the declared path.
                continue;
            }
        }
    }

    if evidence.is_empty() && next_edge < sequence.len() {
        let last = trace.len() - 1;
        evidence.push(evidence_row(EvidenceDraft {
            outcome: ContractOutcome::Inconclusive,
            trigger_tick: first_edge_tick,
            deadline_tick: None,
            violation_tick: None,
            trigger_trace_index: first_edge_index,
            violation_trace_index: Some(last),
            expected: ExpectedCondition::Transition {
                edge: sequence[next_edge],
            },
            observed_fact: None,
            observed_transition: None,
            observation: ObservationState::Unknown,
            reason: EvidenceReason::TransitionSequenceIncomplete,
            detail: Some(format!(
                "observed {next_edge} of {} declared edges",
                sequence.len()
            )),
        }));
    }
    evidence
}

#[derive(Clone, Debug)]
struct PendingProgress {
    trigger_tick: u32,
    deadline_tick: u64,
    trigger_index: usize,
    baseline_mm: u64,
    target_mm: u64,
    missing_measurement_count_at_trigger: u64,
}

fn evaluate_bounded_progress(
    within_ticks: u32,
    minimum_increase_mm: u64,
    trace: &[TraceTick],
) -> Vec<ContractEvidence> {
    let mut evidence = Vec::new();
    let mut pending = VecDeque::<PendingProgress>::new();
    let mut missing_measurement_count = 0_u64;

    for (index, tick) in trace.iter().enumerate() {
        let progress = tick.facts.iter().find_map(|fact| match fact {
            TraceFact::MissionProgress { progress_mm } => Some(*progress_mm),
            _ => None,
        });
        let active = observe(Predicate::MissionActive, tick);

        if progress.is_none() {
            missing_measurement_count += 1;
        }

        // Progress and each obligation's target are monotonic, so resolving the
        // queue head first keeps overlapping windows linear in trace length.
        if let Some(value) = progress {
            while pending.front().is_some_and(|obligation| {
                value.saturating_sub(obligation.baseline_mm) >= minimum_increase_mm
            }) {
                let obligation = pending.pop_front().expect("front was checked");
                evidence.push(evidence_row(EvidenceDraft {
                    outcome: ContractOutcome::Pass,
                    trigger_tick: Some(obligation.trigger_tick),
                    deadline_tick: Some(obligation.deadline_tick),
                    violation_tick: None,
                    trigger_trace_index: Some(obligation.trigger_index),
                    violation_trace_index: Some(index),
                    expected: ExpectedCondition::MissionProgressAtLeast {
                        baseline_mm: obligation.baseline_mm,
                        minimum_increase_mm,
                        required_total_mm: obligation.target_mm,
                    },
                    observed_fact: Some(TraceFact::MissionProgress { progress_mm: value }),
                    observed_transition: None,
                    observation: ObservationState::KnownTrue,
                    reason: EvidenceReason::ProgressAchieved,
                    detail: None,
                }));
            }
        }

        // Progress obligations apply only while the mission remains active. If
        // it completes first, unfinished windows are inconclusive rather than
        // failures caused by the completed mission no longer advancing.
        if !matches!(active, ObservedCondition::True(_)) {
            while let Some(obligation) = pending.pop_front() {
                let (observation, reason) = if active == ObservedCondition::Unknown {
                    (
                        ObservationState::Unknown,
                        EvidenceReason::MissingObservation,
                    )
                } else {
                    (
                        ObservationState::Unknown,
                        EvidenceReason::MissionTerminatedBeforeDeadline,
                    )
                };
                evidence.push(evidence_row(EvidenceDraft {
                    outcome: ContractOutcome::Inconclusive,
                    trigger_tick: Some(obligation.trigger_tick),
                    deadline_tick: Some(obligation.deadline_tick),
                    violation_tick: None,
                    trigger_trace_index: Some(obligation.trigger_index),
                    violation_trace_index: Some(index),
                    expected: ExpectedCondition::MissionProgressAtLeast {
                        baseline_mm: obligation.baseline_mm,
                        minimum_increase_mm,
                        required_total_mm: obligation.target_mm,
                    },
                    observed_fact: progress
                        .map(|progress_mm| TraceFact::MissionProgress { progress_mm }),
                    observed_transition: None,
                    observation,
                    reason,
                    detail: Some(format!("mission is not known active at tick {}", tick.tick)),
                }));
            }
        }

        while pending
            .front()
            .is_some_and(|obligation| u64::from(tick.tick) == obligation.deadline_tick)
        {
            let obligation = pending.pop_front().expect("front was checked");
            let unknown =
                missing_measurement_count > obligation.missing_measurement_count_at_trigger;
            let outcome = if unknown {
                ContractOutcome::Inconclusive
            } else {
                ContractOutcome::Fail
            };
            evidence.push(evidence_row(EvidenceDraft {
                outcome,
                trigger_tick: Some(obligation.trigger_tick),
                deadline_tick: Some(obligation.deadline_tick),
                violation_tick: (outcome == ContractOutcome::Fail).then_some(tick.tick),
                trigger_trace_index: Some(obligation.trigger_index),
                violation_trace_index: Some(index),
                expected: ExpectedCondition::MissionProgressAtLeast {
                    baseline_mm: obligation.baseline_mm,
                    minimum_increase_mm,
                    required_total_mm: obligation.target_mm,
                },
                observed_fact: progress
                    .map(|progress_mm| TraceFact::MissionProgress { progress_mm }),
                observed_transition: None,
                observation: if unknown {
                    ObservationState::Unknown
                } else {
                    ObservationState::KnownFalse
                },
                reason: if unknown {
                    EvidenceReason::MissingProgressMeasurement
                } else {
                    EvidenceReason::ProgressNotReached
                },
                detail: None,
            }));
        }

        if tick.facts.contains(&TraceFact::MissionCompleted)
            || tick.facts.contains(&TraceFact::MissionTerminated)
        {
            while let Some(obligation) = pending.pop_front() {
                evidence.push(evidence_row(EvidenceDraft {
                    outcome: ContractOutcome::Inconclusive,
                    trigger_tick: Some(obligation.trigger_tick),
                    deadline_tick: Some(obligation.deadline_tick),
                    violation_tick: None,
                    trigger_trace_index: Some(obligation.trigger_index),
                    violation_trace_index: Some(index),
                    expected: ExpectedCondition::MissionProgressAtLeast {
                        baseline_mm: obligation.baseline_mm,
                        minimum_increase_mm,
                        required_total_mm: obligation.target_mm,
                    },
                    observed_fact: progress
                        .map(|progress_mm| TraceFact::MissionProgress { progress_mm }),
                    observed_transition: None,
                    observation: ObservationState::Unknown,
                    reason: EvidenceReason::MissionTerminatedBeforeDeadline,
                    detail: None,
                }));
            }
        }

        if matches!(active, ObservedCondition::True(_)) {
            match progress {
                Some(baseline_mm) => {
                    let target_mm = baseline_mm.saturating_add(minimum_increase_mm);
                    let deadline_tick = u64::from(tick.tick) + u64::from(within_ticks);
                    if within_ticks == 0 {
                        evidence.push(evidence_row(EvidenceDraft {
                            outcome: ContractOutcome::Fail,
                            trigger_tick: Some(tick.tick),
                            deadline_tick: Some(deadline_tick),
                            violation_tick: Some(tick.tick),
                            trigger_trace_index: Some(index),
                            violation_trace_index: Some(index),
                            expected: ExpectedCondition::MissionProgressAtLeast {
                                baseline_mm,
                                minimum_increase_mm,
                                required_total_mm: target_mm,
                            },
                            observed_fact: Some(TraceFact::MissionProgress {
                                progress_mm: baseline_mm,
                            }),
                            observed_transition: None,
                            observation: ObservationState::KnownFalse,
                            reason: EvidenceReason::ProgressNotReached,
                            detail: Some(
                                "a zero-tick window cannot increase from its trigger baseline"
                                    .into(),
                            ),
                        }));
                    } else {
                        pending.push_back(PendingProgress {
                            trigger_tick: tick.tick,
                            deadline_tick,
                            trigger_index: index,
                            baseline_mm,
                            target_mm,
                            missing_measurement_count_at_trigger: missing_measurement_count,
                        });
                    }
                }
                None => evidence.push(evidence_row(EvidenceDraft {
                    outcome: ContractOutcome::Inconclusive,
                    trigger_tick: Some(tick.tick),
                    deadline_tick: Some(u64::from(tick.tick) + u64::from(within_ticks)),
                    violation_tick: None,
                    trigger_trace_index: Some(index),
                    violation_trace_index: Some(index),
                    expected: ExpectedCondition::MissionProgressAtLeast {
                        baseline_mm: 0,
                        minimum_increase_mm,
                        required_total_mm: minimum_increase_mm,
                    },
                    observed_fact: None,
                    observed_transition: None,
                    observation: ObservationState::Unknown,
                    reason: EvidenceReason::MissingProgressMeasurement,
                    detail: Some("active mission has no progress measurement on this tick".into()),
                })),
            }
        } else if active == ObservedCondition::Unknown {
            evidence.push(evidence_row(EvidenceDraft {
                outcome: ContractOutcome::Inconclusive,
                trigger_tick: None,
                deadline_tick: None,
                violation_tick: None,
                trigger_trace_index: None,
                violation_trace_index: Some(index),
                expected: ExpectedCondition::Predicate {
                    predicate: Predicate::MissionActive,
                },
                observed_fact: None,
                observed_transition: None,
                observation: ObservationState::Unknown,
                reason: EvidenceReason::MissingObservation,
                detail: None,
            }));
        }
    }

    if let Some(last_tick) = trace.last() {
        while let Some(obligation) = pending.pop_front() {
            evidence.push(evidence_row(EvidenceDraft {
                outcome: ContractOutcome::Inconclusive,
                trigger_tick: Some(obligation.trigger_tick),
                deadline_tick: Some(obligation.deadline_tick),
                violation_tick: None,
                trigger_trace_index: Some(obligation.trigger_index),
                violation_trace_index: Some(trace.len() - 1),
                expected: ExpectedCondition::MissionProgressAtLeast {
                    baseline_mm: obligation.baseline_mm,
                    minimum_increase_mm,
                    required_total_mm: obligation.target_mm,
                },
                observed_fact: None,
                observed_transition: None,
                observation: ObservationState::Unknown,
                reason: EvidenceReason::TraceEndedBeforeDeadline,
                detail: Some(format!("trace ended at tick {}", last_tick.tick)),
            }));
        }
    }
    evidence
}

struct EvidenceDraft {
    outcome: ContractOutcome,
    trigger_tick: Option<u32>,
    deadline_tick: Option<u64>,
    violation_tick: Option<u32>,
    trigger_trace_index: Option<usize>,
    violation_trace_index: Option<usize>,
    expected: ExpectedCondition,
    observed_fact: Option<TraceFact>,
    observed_transition: Option<TraceTransition>,
    observation: ObservationState,
    reason: EvidenceReason,
    detail: Option<String>,
}

fn evidence_row(draft: EvidenceDraft) -> ContractEvidence {
    ContractEvidence {
        outcome: draft.outcome,
        trigger_tick: draft.trigger_tick,
        deadline_tick: draft.deadline_tick,
        violation_tick: draft.violation_tick,
        trigger_trace_index: draft.trigger_trace_index,
        violation_trace_index: draft.violation_trace_index,
        expected: draft.expected,
        observed_fact: draft.observed_fact,
        observed_transition: draft.observed_transition,
        monitor_derived_command_outcome: None,
        observation: draft.observation,
        reason: draft.reason,
        detail: draft.detail,
    }
}

fn build_result(
    schema_version: u32,
    contract: &Contract,
    evidence: Vec<ContractEvidence>,
) -> ContractResult {
    let outcome = if evidence
        .iter()
        .any(|row| row.outcome == ContractOutcome::Fail)
    {
        ContractOutcome::Fail
    } else if evidence
        .iter()
        .any(|row| row.outcome == ContractOutcome::Inconclusive)
    {
        ContractOutcome::Inconclusive
    } else {
        ContractOutcome::Pass
    };
    let first_trigger_tick = evidence.iter().filter_map(|row| row.trigger_tick).min();
    let first_violation = evidence
        .iter()
        .filter(|row| row.outcome == ContractOutcome::Fail)
        .min_by_key(|row| row.violation_tick.unwrap_or(u32::MAX));
    let first_violation_tick = first_violation.and_then(|row| row.violation_tick);
    let deadline_tick = first_violation
        .and_then(|row| row.deadline_tick)
        .or_else(|| {
            evidence
                .iter()
                .find(|row| row.trigger_tick.is_some())
                .and_then(|row| row.deadline_tick)
        });
    ContractResult {
        schema_version,
        contract_id: contract.id().to_owned(),
        property_type: contract.property_type(),
        outcome,
        first_trigger_tick,
        deadline_tick,
        first_violation_tick,
        evidence,
    }
}

fn invalid_result(
    schema_version: u32,
    contract: &Contract,
    reason: EvidenceReason,
    detail: String,
) -> ContractResult {
    build_result(
        schema_version,
        contract,
        vec![evidence_row(EvidenceDraft {
            outcome: ContractOutcome::Inconclusive,
            trigger_tick: None,
            deadline_tick: None,
            violation_tick: None,
            trigger_trace_index: None,
            violation_trace_index: None,
            expected: ExpectedCondition::TraceIntegrity,
            observed_fact: None,
            observed_transition: None,
            observation: ObservationState::Invalid,
            reason,
            detail: Some(detail),
        })],
    )
}

fn validate_trace(trace: &[TraceTick]) -> Result<(), String> {
    if trace.is_empty() {
        return Err("trace is empty".into());
    }
    if trace.len() > MAX_TRACE_TICKS {
        return Err(format!("trace exceeds {MAX_TRACE_TICKS} ticks"));
    }
    let mut mission_completed = false;
    for (index, tick) in trace.iter().enumerate() {
        let expected = u32::try_from(index + 1).map_err(|_| "trace tick index overflows")?;
        if tick.tick != expected {
            return Err(format!(
                "ticks must start at 1 and be contiguous: index {index} has tick {}",
                tick.tick
            ));
        }
        validate_tick_facts(tick)?;
        if tick.facts.contains(&TraceFact::MissionTerminated) && index + 1 != trace.len() {
            return Err(format!(
                "mission_terminated may appear only on the final trace tick, found at tick {}",
                tick.tick
            ));
        }
        if tick.facts.contains(&TraceFact::MissionCompleted) {
            if mission_completed {
                return Err("mission_completed may appear only once in a trace".into());
            }
            mission_completed = true;
        } else if mission_completed && tick.facts.contains(&TraceFact::MissionActive) {
            return Err(format!(
                "mission_active observed after mission_completed at tick {}",
                tick.tick
            ));
        }
    }
    validate_transition_order(trace)?;
    validate_command_facts(trace)?;
    validate_progress_monotonicity(trace)?;
    Ok(())
}

fn validate_tick_facts(tick: &TraceTick) -> Result<(), String> {
    let mut singletons = BTreeSet::<TraceFactIdentity>::new();
    let mut command_facts = BTreeSet::new();
    for fact in &tick.facts {
        if let Some(command_id) = fact.command_id() {
            let identity = fact.identity();
            if !command_facts.insert((identity, command_id)) {
                return Err(format!(
                    "duplicate {:?} fact for command_id {command_id} at tick {}",
                    identity, tick.tick
                ));
            }
        } else if !singletons.insert(fact.identity()) {
            return Err(format!(
                "duplicate {:?} fact at tick {}",
                fact.identity(),
                tick.tick
            ));
        }
    }
    Ok(())
}

fn validate_transition_order(trace: &[TraceTick]) -> Result<(), String> {
    let mut previous_sequence = 0_u64;
    for tick in trace {
        for transition in &tick.transitions {
            if transition.source_sequence == 0 || transition.source_sequence <= previous_sequence {
                return Err(format!(
                    "transition source sequences must increase: {} at tick {}",
                    transition.source_sequence, tick.tick
                ));
            }
            if transition.from == transition.to
                || !transition_state_matches(transition.domain, transition.from)
                || !transition_state_matches(transition.domain, transition.to)
            {
                return Err(format!(
                    "invalid {:?} transition {:?}->{:?} at tick {}",
                    transition.domain, transition.from, transition.to, tick.tick
                ));
            }
            previous_sequence = transition.source_sequence;
        }
    }
    Ok(())
}

fn transition_state_matches(domain: TransitionDomain, state: TransitionState) -> bool {
    match domain {
        TransitionDomain::Mission => matches!(
            state,
            TransitionState::Pending | TransitionState::Running | TransitionState::Completed
        ),
        TransitionDomain::Safety => {
            matches!(state, TransitionState::Nominal | TransitionState::Fallback)
        }
    }
}

fn validate_command_facts(trace: &[TraceTick]) -> Result<(), String> {
    let mut commanded = BTreeMap::<u64, u32>::new();
    let mut resolutions = BTreeSet::new();
    let mut applications = BTreeSet::new();
    for tick in trace {
        for fact in &tick.facts {
            match fact {
                TraceFact::MovementCommanded { command_id } => {
                    if *command_id == 0 || commanded.insert(*command_id, tick.tick).is_some() {
                        return Err(format!(
                            "command_id {command_id} is zero or reused at tick {}",
                            tick.tick
                        ));
                    }
                }
                TraceFact::CommandApplied { command_id } => {
                    if !applications.insert(*command_id) {
                        return Err(format!("command_id {command_id} applied more than once"));
                    }
                }
                TraceFact::CommandResolved { command_id, .. }
                    if !resolutions.insert(*command_id) =>
                {
                    return Err(format!("command_id {command_id} resolved more than once"));
                }
                _ => {}
            }
        }
    }
    for tick in trace {
        for fact in &tick.facts {
            match fact {
                TraceFact::CommandApplied { command_id }
                | TraceFact::CommandResolved { command_id, .. } => {
                    let Some(command_tick) = commanded.get(command_id) else {
                        return Err(format!(
                            "command outcome references unknown command_id {command_id}"
                        ));
                    };
                    if *command_tick > tick.tick {
                        return Err(format!(
                            "command_id {command_id} resolves before it was issued"
                        ));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn validate_progress_monotonicity(trace: &[TraceTick]) -> Result<(), String> {
    let mut previous = None;
    for tick in trace {
        let progress = tick.facts.iter().find_map(|fact| match fact {
            TraceFact::MissionProgress { progress_mm } => Some(*progress_mm),
            _ => None,
        });
        if let Some(current) = progress {
            if previous.is_some_and(|last| current < last) {
                return Err(format!(
                    "cumulative mission progress decreased at tick {}",
                    tick.tick
                ));
            }
            previous = Some(current);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(json: &str) -> ContractSet {
        ContractSet::from_json(json).unwrap()
    }

    fn tick(tick: u32, telemetry_present: bool, facts: Vec<TraceFact>) -> TraceTick {
        TraceTick {
            tick,
            telemetry_present,
            facts,
            transitions: Vec::new(),
        }
    }

    fn result(contracts: &ContractSet, trace: &[TraceTick], index: usize) -> ContractResult {
        evaluate(contracts, trace).remove(index)
    }

    #[test]
    fn always_never_and_missing_sensor_telemetry_have_three_valued_results() {
        let contracts = set(r#"{"schema_version":1,"contracts":[
              {"type":"always","id":"zone_clear","predicate":"restricted_zone_clear"},
              {"type":"never","id":"fallback_motion","predicate":"movement_while_fallback"},
              {"type":"always","id":"localization_known","predicate":"localization_unreliable"}
            ]}"#);
        let trace = vec![
            tick(
                1,
                true,
                vec![
                    TraceFact::RestrictedZoneClear,
                    TraceFact::LocalizationUnreliable,
                ],
            ),
            tick(2, false, vec![TraceFact::RestrictedZoneClear]),
        ];
        let results = evaluate(&contracts, &trace);
        assert_eq!(results[0].outcome, ContractOutcome::Pass);
        assert_eq!(results[1].outcome, ContractOutcome::Pass);
        assert_eq!(results[2].outcome, ContractOutcome::Inconclusive);
        assert_eq!(
            results[2].evidence[0].reason,
            EvidenceReason::MissingObservation
        );

        let violation = vec![
            tick(1, true, vec![TraceFact::RestrictedZoneClear]),
            tick(2, true, vec![TraceFact::MovementWhileFallback]),
        ];
        assert_eq!(
            result(&contracts, &violation, 1).first_violation_tick,
            Some(2)
        );
    }

    #[test]
    fn bounded_response_includes_trigger_and_exact_deadline_ticks() {
        let contracts = set(
            r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"stop","trigger":"stop_commanded","required":"vehicle_stationary","within_ticks":2}]}"#,
        );
        let on_trigger = vec![tick(
            1,
            true,
            vec![TraceFact::StopCommanded, TraceFact::VehicleStationary],
        )];
        assert_eq!(
            result(&contracts, &on_trigger, 0).outcome,
            ContractOutcome::Pass
        );

        let exact_deadline = vec![
            tick(1, true, vec![TraceFact::StopCommanded]),
            tick(2, true, vec![]),
            tick(3, true, vec![TraceFact::VehicleStationary]),
        ];
        let passed = result(&contracts, &exact_deadline, 0);
        assert_eq!(passed.outcome, ContractOutcome::Pass);
        assert_eq!(passed.evidence[0].deadline_tick, Some(3));

        let late = vec![
            tick(1, true, vec![TraceFact::StopCommanded]),
            tick(2, true, vec![]),
            tick(3, true, vec![]),
            tick(4, true, vec![TraceFact::VehicleStationary]),
        ];
        let failed = result(&contracts, &late, 0);
        assert_eq!(failed.outcome, ContractOutcome::Fail);
        assert_eq!(failed.first_trigger_tick, Some(1));
        assert_eq!(failed.deadline_tick, Some(3));
        assert_eq!(failed.first_violation_tick, Some(3));
        assert_eq!(failed.evidence[0].violation_trace_index, Some(2));
    }

    #[test]
    fn zero_bound_and_response_obligations_continue_after_mission_completion() {
        let contracts = set(
            r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"fallback","trigger":"localization_unreliable","required":"safety_fallback","within_ticks":0}]}"#,
        );
        let same_tick = vec![tick(
            1,
            true,
            vec![TraceFact::LocalizationUnreliable, TraceFact::SafetyFallback],
        )];
        assert_eq!(
            result(&contracts, &same_tick, 0).outcome,
            ContractOutcome::Pass
        );

        let mission_completion_then_deadline = vec![
            tick(
                1,
                true,
                vec![
                    TraceFact::LocalizationUnreliable,
                    TraceFact::MissionCompleted,
                ],
            ),
            tick(2, true, vec![]),
            tick(3, true, vec![TraceFact::MissionTerminated]),
        ];
        let pending_contract = set(
            r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"fallback","trigger":"localization_unreliable","required":"safety_fallback","within_ticks":2}]}"#,
        );
        let completed_result = result(&pending_contract, &mission_completion_then_deadline, 0);
        assert_eq!(completed_result.outcome, ContractOutcome::Fail);
        assert_eq!(completed_result.first_violation_tick, Some(3));

        let pending_at_trace_end = vec![tick(1, true, vec![TraceFact::LocalizationUnreliable])];
        let pending_result = result(&pending_contract, &pending_at_trace_end, 0);
        assert_eq!(pending_result.outcome, ContractOutcome::Inconclusive);
        assert_eq!(
            pending_result.evidence[0].reason,
            EvidenceReason::TraceEndedBeforeDeadline
        );
    }

    #[test]
    fn repeated_triggers_create_overlapping_obligations_and_unknown_trigger_resets_streak() {
        let contracts = set(
            r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"fallback","trigger":"localization_unreliable","required":"safety_fallback","within_ticks":2,"trigger_for_ticks":2}]}"#,
        );
        let trace = vec![
            tick(1, true, vec![TraceFact::LocalizationUnreliable]),
            tick(2, false, vec![]),
            tick(3, true, vec![TraceFact::LocalizationUnreliable]),
            tick(4, true, vec![TraceFact::LocalizationUnreliable]),
            tick(5, true, vec![TraceFact::LocalizationUnreliable]),
            tick(6, true, vec![TraceFact::SafetyFallback]),
        ];
        let streak_result = result(&contracts, &trace, 0);
        assert_eq!(streak_result.first_trigger_tick, Some(4));
        assert_eq!(streak_result.outcome, ContractOutcome::Inconclusive);
        assert_eq!(
            streak_result
                .evidence
                .iter()
                .filter(|row| row.outcome == ContractOutcome::Pass)
                .count(),
            1
        );
        assert!(streak_result.evidence.iter().any(|row| {
            row.outcome == ContractOutcome::Inconclusive
                && row.reason == EvidenceReason::MissingObservation
        }));

        let overlapping = set(
            r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"fallback","trigger":"stop_commanded","required":"safety_fallback","within_ticks":3}]}"#,
        );
        let trace = vec![
            tick(1, true, vec![TraceFact::StopCommanded]),
            tick(2, true, vec![]),
            tick(3, true, vec![TraceFact::StopCommanded]),
            tick(4, true, vec![TraceFact::SafetyFallback]),
        ];
        let overlapping_result = result(&overlapping, &trace, 0);
        assert_eq!(overlapping_result.outcome, ContractOutcome::Pass);
        assert_eq!(
            overlapping_result
                .evidence
                .iter()
                .map(|row| row.trigger_tick)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(3)]
        );
    }

    #[test]
    fn correlated_command_response_does_not_accept_another_commands_outcome() {
        let contracts = set(
            r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"command_resolves","trigger":"movement_commanded","required":"command_resolved","within_ticks":1,"correlation":"command_id"}]}"#,
        );
        let trace = vec![
            tick(
                1,
                true,
                vec![TraceFact::MovementCommanded { command_id: 10 }],
            ),
            tick(
                2,
                true,
                vec![
                    TraceFact::MovementCommanded { command_id: 11 },
                    TraceFact::CommandResolved {
                        command_id: 10,
                        outcome: CommandOutcome::ExplicitFailure,
                    },
                ],
            ),
            tick(3, true, vec![]),
        ];
        let command_result = result(&contracts, &trace, 0);
        assert_eq!(command_result.outcome, ContractOutcome::Fail);
        assert_eq!(command_result.evidence.len(), 2);
        assert!(
            command_result.evidence[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("command_id=10")
        );
        assert_eq!(
            command_result.evidence[0].monitor_derived_command_outcome,
            None
        );
        assert_eq!(
            command_result.evidence[1].monitor_derived_command_outcome,
            Some(CommandOutcome::Timeout)
        );
        assert_eq!(
            command_result.evidence[1].observation,
            ObservationState::MonitorDerived
        );
        assert_eq!(
            command_result.evidence[1].reason,
            EvidenceReason::CommandTimedOutDerived
        );
        assert_eq!(command_result.evidence[1].violation_tick, Some(3));

        let strict_apply = set(
            r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"command_applies","trigger":"movement_commanded","required":"command_applied","within_ticks":1,"correlation":"command_id"}]}"#,
        );
        let ignored = vec![
            tick(
                1,
                true,
                vec![TraceFact::MovementCommanded { command_id: 20 }],
            ),
            tick(2, true, vec![]),
        ];
        let failed = result(&strict_apply, &ignored, 0);
        assert_eq!(failed.outcome, ContractOutcome::Fail);
        assert_eq!(failed.first_violation_tick, Some(2));
        assert_eq!(
            failed.evidence[0].monitor_derived_command_outcome,
            Some(CommandOutcome::Timeout)
        );

        let late_apply = vec![
            tick(
                1,
                true,
                vec![TraceFact::MovementCommanded { command_id: 30 }],
            ),
            tick(2, true, vec![]),
            tick(3, true, vec![TraceFact::CommandApplied { command_id: 30 }]),
        ];
        assert_eq!(
            result(&strict_apply, &late_apply, 0).outcome,
            ContractOutcome::Fail
        );

        let explicit_resolution_only = set(
            r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"explicit_resolution","trigger":"movement_commanded","required":"command_resolved","within_ticks":1,"correlation":"command_id"}]}"#,
        );
        let missing_resolution = vec![
            tick(
                1,
                true,
                vec![TraceFact::MovementCommanded { command_id: 41 }],
            ),
            tick(2, true, vec![]),
        ];
        let unresolved = result(&explicit_resolution_only, &missing_resolution, 0);
        assert_eq!(unresolved.outcome, ContractOutcome::Fail);
        assert_eq!(
            unresolved.evidence[0].reason,
            EvidenceReason::CommandTimedOutDerived
        );
        assert_eq!(
            unresolved.evidence[0].monitor_derived_command_outcome,
            Some(CommandOutcome::Timeout)
        );

        let ended_early = vec![tick(
            1,
            true,
            vec![TraceFact::MovementCommanded { command_id: 42 }],
        )];
        let pending = result(&explicit_resolution_only, &ended_early, 0);
        assert_eq!(pending.outcome, ContractOutcome::Inconclusive);
        assert_eq!(pending.evidence[0].monitor_derived_command_outcome, None);
    }

    #[test]
    fn ordered_transition_requires_exact_sequence_and_reports_wrong_event_order() {
        let contracts = set(
            r#"{"schema_version":1,"contracts":[{"type":"ordered_transition","id":"mission","domain":"mission","sequence":[{"from":"pending","to":"running"},{"from":"running","to":"completed"}]}]}"#,
        );
        let pass = vec![
            TraceTick {
                transitions: vec![TraceTransition {
                    source_sequence: 2,
                    domain: TransitionDomain::Mission,
                    from: TransitionState::Pending,
                    to: TransitionState::Running,
                }],
                ..tick(1, true, vec![])
            },
            TraceTick {
                transitions: vec![TraceTransition {
                    source_sequence: 9,
                    domain: TransitionDomain::Mission,
                    from: TransitionState::Running,
                    to: TransitionState::Completed,
                }],
                ..tick(2, true, vec![])
            },
        ];
        assert_eq!(result(&contracts, &pass, 0).outcome, ContractOutcome::Pass);

        let incomplete = vec![TraceTick {
            transitions: vec![TraceTransition {
                source_sequence: 1,
                domain: TransitionDomain::Mission,
                from: TransitionState::Pending,
                to: TransitionState::Running,
            }],
            ..tick(1, true, vec![])
        }];
        assert_eq!(
            result(&contracts, &incomplete, 0).outcome,
            ContractOutcome::Inconclusive
        );

        let wrong_order = vec![TraceTick {
            transitions: vec![TraceTransition {
                source_sequence: 1,
                domain: TransitionDomain::Mission,
                from: TransitionState::Pending,
                to: TransitionState::Completed,
            }],
            ..tick(1, true, vec![])
        }];
        let failed = result(&contracts, &wrong_order, 0);
        assert_eq!(failed.outcome, ContractOutcome::Fail);
        assert_eq!(failed.first_violation_tick, Some(1));
    }

    #[test]
    fn bounded_progress_uses_cumulative_millimetres_and_separates_incomplete_end() {
        let contracts = set(
            r#"{"schema_version":1,"contracts":[{"type":"bounded_progress","id":"mission_progress","within_ticks":2,"minimum_increase_mm":10}]}"#,
        );
        let passed = vec![
            tick(
                1,
                true,
                vec![
                    TraceFact::MissionActive,
                    TraceFact::MissionProgress { progress_mm: 100 },
                ],
            ),
            tick(
                2,
                true,
                vec![
                    TraceFact::MissionActive,
                    TraceFact::MissionProgress { progress_mm: 105 },
                ],
            ),
            tick(
                3,
                true,
                vec![
                    TraceFact::MissionActive,
                    TraceFact::MissionProgress { progress_mm: 110 },
                ],
            ),
            tick(
                4,
                true,
                vec![
                    TraceFact::MissionActive,
                    TraceFact::MissionProgress { progress_mm: 115 },
                ],
            ),
            tick(
                5,
                true,
                vec![
                    TraceFact::MissionActive,
                    TraceFact::MissionProgress { progress_mm: 120 },
                ],
            ),
            tick(
                6,
                true,
                vec![
                    TraceFact::MissionTerminated,
                    TraceFact::MissionProgress { progress_mm: 130 },
                ],
            ),
        ];
        assert_eq!(
            result(&contracts, &passed, 0).outcome,
            ContractOutcome::Pass
        );

        let stalled = vec![
            tick(
                1,
                true,
                vec![
                    TraceFact::MissionActive,
                    TraceFact::MissionProgress { progress_mm: 20 },
                ],
            ),
            tick(
                2,
                true,
                vec![
                    TraceFact::MissionActive,
                    TraceFact::MissionProgress { progress_mm: 20 },
                ],
            ),
            tick(
                3,
                true,
                vec![
                    TraceFact::MissionActive,
                    TraceFact::MissionProgress { progress_mm: 20 },
                ],
            ),
        ];
        let failed = result(&contracts, &stalled, 0);
        assert_eq!(failed.outcome, ContractOutcome::Fail);
        assert_eq!(failed.first_violation_tick, Some(3));
        assert_eq!(failed.deadline_tick, Some(3));

        let incomplete = vec![
            tick(
                1,
                true,
                vec![
                    TraceFact::MissionActive,
                    TraceFact::MissionProgress { progress_mm: 20 },
                ],
            ),
            tick(
                2,
                true,
                vec![
                    TraceFact::MissionTerminated,
                    TraceFact::MissionProgress { progress_mm: 24 },
                ],
            ),
        ];
        assert_eq!(
            result(&contracts, &incomplete, 0).outcome,
            ContractOutcome::Inconclusive
        );
    }

    #[test]
    fn bounded_progress_is_inconclusive_when_measurements_or_active_window_end_early() {
        let contracts = set(
            r#"{"schema_version":1,"contracts":[{"type":"bounded_progress","id":"mission_progress","within_ticks":2,"minimum_increase_mm":10}]}"#,
        );
        let missing_measurement = vec![
            tick(
                1,
                true,
                vec![
                    TraceFact::MissionActive,
                    TraceFact::MissionProgress { progress_mm: 100 },
                ],
            ),
            tick(2, true, vec![TraceFact::MissionActive]),
            tick(
                3,
                true,
                vec![
                    TraceFact::MissionActive,
                    TraceFact::MissionProgress { progress_mm: 100 },
                ],
            ),
        ];
        let missing_result = result(&contracts, &missing_measurement, 0);
        assert_eq!(missing_result.outcome, ContractOutcome::Inconclusive);
        assert!(missing_result.evidence.iter().any(|row| {
            row.trigger_tick == Some(1)
                && row.reason == EvidenceReason::MissingProgressMeasurement
                && row.outcome == ContractOutcome::Inconclusive
        }));

        let completed_early = vec![
            tick(
                1,
                true,
                vec![
                    TraceFact::MissionActive,
                    TraceFact::MissionProgress { progress_mm: 100 },
                ],
            ),
            tick(
                2,
                true,
                vec![TraceFact::MissionProgress { progress_mm: 104 }],
            ),
        ];
        let completed_result = result(&contracts, &completed_early, 0);
        assert_eq!(completed_result.outcome, ContractOutcome::Inconclusive);
        assert_eq!(
            completed_result.evidence[0].reason,
            EvidenceReason::MissionTerminatedBeforeDeadline
        );
    }

    #[test]
    fn final_trace_marker_can_coexist_with_active_state_on_that_tick() {
        let contracts = set(
            r#"{"schema_version":1,"contracts":[{"type":"always","id":"active_on_last_tick","predicate":"mission_active"}]}"#,
        );
        let trace = vec![tick(
            1,
            true,
            vec![TraceFact::MissionActive, TraceFact::MissionTerminated],
        )];
        assert_eq!(result(&contracts, &trace, 0).outcome, ContractOutcome::Pass);
    }

    #[test]
    fn rejects_missing_ticks_corrupt_command_refs_and_transition_order() {
        let contracts = set(
            r#"{"schema_version":1,"contracts":[{"type":"always","id":"clear","predicate":"restricted_zone_clear"}]}"#,
        );
        for invalid in [
            vec![tick(2, true, vec![TraceFact::RestrictedZoneClear])],
            vec![
                tick(1, true, vec![TraceFact::RestrictedZoneClear]),
                tick(3, true, vec![TraceFact::RestrictedZoneClear]),
            ],
            vec![tick(
                1,
                true,
                vec![TraceFact::CommandApplied { command_id: 7 }],
            )],
            vec![
                tick(1, true, vec![TraceFact::MissionTerminated]),
                tick(2, true, vec![TraceFact::RestrictedZoneClear]),
            ],
            vec![TraceTick {
                transitions: vec![
                    TraceTransition {
                        source_sequence: 8,
                        domain: TransitionDomain::Safety,
                        from: TransitionState::Nominal,
                        to: TransitionState::Fallback,
                    },
                    TraceTransition {
                        source_sequence: 7,
                        domain: TransitionDomain::Safety,
                        from: TransitionState::Fallback,
                        to: TransitionState::Nominal,
                    },
                ],
                ..tick(1, true, vec![TraceFact::RestrictedZoneClear])
            }],
        ] {
            let result = result(&contracts, &invalid, 0);
            assert_eq!(result.outcome, ContractOutcome::Inconclusive);
            assert_eq!(result.evidence[0].reason, EvidenceReason::InvalidTrace);
        }
    }
}
