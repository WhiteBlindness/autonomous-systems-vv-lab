//! Declarative, versioned contracts for finite-trace verification.
//!
//! The schema deliberately exposes only typed predicates and operators. It does not
//! evaluate expressions, scripts, or user supplied code.

use serde::{Deserialize, Serialize};

/// Current version of the declarative contract document.
pub const CONTRACT_SCHEMA_VERSION: u32 = 1;
const MAX_CONTRACTS: usize = 128;
const MAX_CONTRACT_BYTES: usize = 1024 * 1024;
const MAX_CONTRACT_ID_BYTES: usize = 64;
const MAX_BOUND_TICKS: u32 = 10_000;
const MAX_TRANSITIONS: usize = 32;

/// A complete set of declarative properties for one verification run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContractSet {
    pub schema_version: u32,
    pub contracts: Vec<Contract>,
}

impl ContractSet {
    /// Parse JSON and validate both its shape and its semantic constraints.
    pub fn from_json(text: &str) -> Result<Self, ContractError> {
        if text.len() > MAX_CONTRACT_BYTES {
            return Err(ContractError(
                "contract document exceeds the 1 MiB limit".into(),
            ));
        }
        let contracts: Self = serde_json::from_str(text)
            .map_err(|error| ContractError(format!("invalid contract JSON: {error}")))?;
        contracts.validate()?;
        Ok(contracts)
    }

    /// Validate version, identifiers, operator bounds, and typed transition paths.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != CONTRACT_SCHEMA_VERSION {
            return Err(ContractError("unsupported contract schema_version".into()));
        }
        if self.contracts.is_empty() || self.contracts.len() > MAX_CONTRACTS {
            return Err(ContractError(format!(
                "contracts must contain 1 to {MAX_CONTRACTS} entries"
            )));
        }

        let mut ids = std::collections::BTreeSet::new();
        for contract in &self.contracts {
            let id = contract.id();
            validate_id(id)?;
            if !ids.insert(id) {
                return Err(ContractError(format!("duplicate contract id: {id}")));
            }
            match contract {
                Contract::Always { .. } | Contract::Never { .. } => {}
                Contract::BoundedResponse {
                    trigger,
                    required,
                    correlation,
                    within_ticks,
                    trigger_for_ticks,
                    ..
                } => {
                    validate_bound(*within_ticks, "within_ticks")?;
                    if !(1..=MAX_BOUND_TICKS).contains(trigger_for_ticks) {
                        return Err(ContractError(format!(
                            "trigger_for_ticks must be between 1 and {MAX_BOUND_TICKS}"
                        )));
                    }
                    let command_scoped = matches!(
                        trigger,
                        Predicate::MovementCommanded
                            | Predicate::CommandApplied
                            | Predicate::CommandResolved
                    ) || matches!(
                        required,
                        Predicate::CommandApplied | Predicate::CommandResolved
                    );
                    if command_scoped
                        && (*correlation != Some(Correlation::CommandId)
                            || *trigger != Predicate::MovementCommanded
                            || !matches!(
                                required,
                                Predicate::CommandApplied | Predicate::CommandResolved
                            )
                            || *trigger_for_ticks != 1)
                    {
                        return Err(ContractError(
                            "command-scoped bounded responses require trigger=movement_commanded, a command outcome predicate, correlation=command_id, and trigger_for_ticks=1".into(),
                        ));
                    }
                    if !command_scoped && correlation.is_some() {
                        return Err(ContractError(
                            "correlation is supported only for command-scoped bounded responses"
                                .into(),
                        ));
                    }
                }
                Contract::OrderedTransition {
                    domain, sequence, ..
                } => validate_transition_sequence(*domain, sequence)?,
                Contract::BoundedProgress {
                    within_ticks,
                    minimum_increase_mm,
                    ..
                } => {
                    validate_bound(*within_ticks, "within_ticks")?;
                    if *minimum_increase_mm == 0 {
                        return Err(ContractError(
                            "minimum_increase_mm must be greater than zero".into(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// One supported temporal operator. Unknown operators and fields are rejected.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Contract {
    /// The predicate must be true at every observed tick in the finite trace.
    Always { id: String, predicate: Predicate },
    /// The predicate must never be true in the finite trace.
    Never { id: String, predicate: Predicate },
    /// Each qualified trigger creates an independent bounded-response obligation.
    BoundedResponse {
        id: String,
        trigger: Predicate,
        required: Predicate,
        within_ticks: u32,
        #[serde(default)]
        correlation: Option<Correlation>,
        /// Consecutive known-true ticks required before qualification. The
        /// qualifying tick is the trigger tick; one obligation is created per
        /// continuous true run. A false or unknown tick resets the streak.
        #[serde(default = "default_trigger_for_ticks")]
        trigger_for_ticks: u32,
    },
    /// Observed transitions must match this exact, ordered edge sequence.
    OrderedTransition {
        id: String,
        domain: TransitionDomain,
        sequence: Vec<TransitionEdge>,
    },
    /// Every active-mission tick starts a window requiring bounded progress.
    BoundedProgress {
        id: String,
        within_ticks: u32,
        /// Minimum increase in cumulative, meaningful ground-truth progress.
        minimum_increase_mm: u64,
    },
}

impl Contract {
    /// Return the stable identifier shared by all operator variants.
    pub fn id(&self) -> &str {
        match self {
            Self::Always { id, .. }
            | Self::Never { id, .. }
            | Self::BoundedResponse { id, .. }
            | Self::OrderedTransition { id, .. }
            | Self::BoundedProgress { id, .. } => id,
        }
    }

    /// Return the operator discriminator used in reports.
    pub fn property_type(&self) -> PropertyType {
        match self {
            Self::Always { .. } => PropertyType::Always,
            Self::Never { .. } => PropertyType::Never,
            Self::BoundedResponse { .. } => PropertyType::BoundedResponse,
            Self::OrderedTransition { .. } => PropertyType::OrderedTransition,
            Self::BoundedProgress { .. } => PropertyType::BoundedProgress,
        }
    }
}

/// The supported boolean conditions. A condition absent from a telemetry-present
/// tick is false; facts on a telemetry-missing tick are unknown.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Predicate {
    LocalizationUnreliable,
    SafetyFallback,
    StopCommanded,
    VehicleStationary,
    RestrictedZoneClear,
    CommandApplied,
    /// Any explicit actuator outcome was recorded for a command.
    CommandResolved,
    /// A valid movement command was issued by the controller.
    MovementCommanded,
    /// Ground truth moved while the safety controller was in fallback.
    MovementWhileFallback,
    MissionActive,
}

impl Predicate {
    /// Match this typed condition against the positive facts on a tick.
    pub fn is_true_in(self, facts: &[TraceFact]) -> bool {
        facts.iter().any(|fact| fact.matches(self))
    }
}

/// A state-machine family used by ordered-transition contracts.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionDomain {
    Mission,
    Safety,
}

/// Key used to match command-resolution facts to their originating command.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Correlation {
    CommandId,
}

/// Terminal outcome of a command at the actuator boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandOutcome {
    Applied,
    ExplicitFailure,
    Timeout,
}

/// Typed states accepted by the finite set of supported state machines.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionState {
    Pending,
    Running,
    Completed,
    Nominal,
    Fallback,
}

/// One declared edge in an ordered-transition contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionEdge {
    pub from: TransitionState,
    pub to: TransitionState,
}

/// Operator label included in the stable result API.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PropertyType {
    Always,
    Never,
    BoundedResponse,
    OrderedTransition,
    BoundedProgress,
}

/// Error from parsing or validating a contract document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractError(pub String);

impl std::fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ContractError {}

fn default_trigger_for_ticks() -> u32 {
    1
}

fn validate_id(id: &str) -> Result<(), ContractError> {
    let bytes = id.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_CONTRACT_ID_BYTES
        || !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit()
        || !bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
        })
    {
        return Err(ContractError(
            "contract ids must be 1 to 64 lowercase ASCII letters, digits, '_' or '-' and start with a letter or digit".into(),
        ));
    }
    Ok(())
}

fn validate_bound(bound: u32, field: &str) -> Result<(), ContractError> {
    if bound > MAX_BOUND_TICKS {
        return Err(ContractError(format!(
            "{field} cannot exceed {MAX_BOUND_TICKS}"
        )));
    }
    Ok(())
}

fn validate_transition_sequence(
    domain: TransitionDomain,
    sequence: &[TransitionEdge],
) -> Result<(), ContractError> {
    if sequence.is_empty() || sequence.len() > MAX_TRANSITIONS {
        return Err(ContractError(format!(
            "ordered transition sequence must contain 1 to {MAX_TRANSITIONS} edges"
        )));
    }
    for (index, edge) in sequence.iter().enumerate() {
        if !state_belongs_to_domain(edge.from, domain) || !state_belongs_to_domain(edge.to, domain)
        {
            return Err(ContractError(format!(
                "transition edge {index} uses a state outside its declared domain"
            )));
        }
        if edge.from == edge.to {
            return Err(ContractError(format!(
                "transition edge {index} must change state"
            )));
        }
        if let Some(previous) = index
            .checked_sub(1)
            .and_then(|previous| sequence.get(previous))
            && previous.to != edge.from
        {
            return Err(ContractError(format!(
                "ordered transition sequence is not contiguous at edge {index}"
            )));
        }
    }
    Ok(())
}

fn state_belongs_to_domain(state: TransitionState, domain: TransitionDomain) -> bool {
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

/// One fact recorded for a tick. Unit variants mean the named condition is true.
/// When localization telemetry is missing, an absent localization fact is unknown;
/// other conditions come from event and ground-truth records.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TraceFact {
    LocalizationUnreliable,
    SafetyFallback,
    StopCommanded,
    VehicleStationary,
    RestrictedZoneClear,
    /// A valid movement command, keyed by the controller command identifier.
    MovementCommanded {
        command_id: u64,
    },
    /// Physical movement caused by a command; may occur after an earlier timeout.
    CommandApplied {
        command_id: u64,
    },
    /// Explicit completion or failure at the actuator boundary.
    CommandResolved {
        command_id: u64,
        outcome: CommandOutcome,
    },
    MovementWhileFallback,
    MissionActive,
    /// Logical mission completion. Response obligations remain monitored after
    /// this event; progress obligations are scoped by `MissionActive` instead.
    MissionCompleted,
    /// Marker on the final configured trace tick. It may coincide with
    /// `MissionActive`, which describes the state during that tick.
    MissionTerminated,
    /// Cumulative meaningful ground-truth progress in millimetres.
    MissionProgress {
        progress_mm: u64,
    },
}

impl TraceFact {
    /// Return the value for a condition represented by this fact, if it matches.
    pub fn matches(self, predicate: Predicate) -> bool {
        matches!(
            (self, predicate),
            (
                Self::LocalizationUnreliable,
                Predicate::LocalizationUnreliable
            ) | (Self::SafetyFallback, Predicate::SafetyFallback)
                | (Self::StopCommanded, Predicate::StopCommanded)
                | (Self::VehicleStationary, Predicate::VehicleStationary)
                | (Self::RestrictedZoneClear, Predicate::RestrictedZoneClear)
                | (
                    Self::MovementWhileFallback,
                    Predicate::MovementWhileFallback
                )
                | (Self::MissionActive, Predicate::MissionActive)
        ) || matches!(
            (self, predicate),
            (
                Self::CommandResolved {
                    outcome: CommandOutcome::Applied,
                    ..
                },
                Predicate::CommandApplied
            ) | (Self::CommandResolved { .. }, Predicate::CommandResolved)
                | (Self::CommandApplied { .. }, Predicate::CommandApplied)
                | (Self::MovementCommanded { .. }, Predicate::MovementCommanded)
        )
    }

    /// Return the command identifier carried by a command-scoped fact.
    pub fn command_id(self) -> Option<u64> {
        match self {
            Self::MovementCommanded { command_id }
            | Self::CommandApplied { command_id }
            | Self::CommandResolved { command_id, .. } => Some(command_id),
            _ => None,
        }
    }

    pub(crate) fn identity(self) -> TraceFactIdentity {
        match self {
            Self::LocalizationUnreliable => TraceFactIdentity::LocalizationUnreliable,
            Self::SafetyFallback => TraceFactIdentity::SafetyFallback,
            Self::StopCommanded => TraceFactIdentity::StopCommanded,
            Self::VehicleStationary => TraceFactIdentity::VehicleStationary,
            Self::RestrictedZoneClear => TraceFactIdentity::RestrictedZoneClear,
            Self::MovementCommanded { .. } => TraceFactIdentity::MovementCommanded,
            Self::CommandApplied { .. } => TraceFactIdentity::CommandApplied,
            Self::CommandResolved { .. } => TraceFactIdentity::CommandResolved,
            Self::MovementWhileFallback => TraceFactIdentity::MovementWhileFallback,
            Self::MissionActive => TraceFactIdentity::MissionActive,
            Self::MissionCompleted => TraceFactIdentity::MissionCompleted,
            Self::MissionTerminated => TraceFactIdentity::MissionTerminated,
            Self::MissionProgress { .. } => TraceFactIdentity::MissionProgress,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum TraceFactIdentity {
    LocalizationUnreliable,
    SafetyFallback,
    StopCommanded,
    VehicleStationary,
    RestrictedZoneClear,
    MovementCommanded,
    CommandApplied,
    CommandResolved,
    MovementWhileFallback,
    MissionActive,
    MissionCompleted,
    MissionTerminated,
    MissionProgress,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_supported_contract_shapes_and_default_trigger_duration() {
        let contracts = ContractSet::from_json(
            r#"{
              "schema_version": 1,
              "contracts": [
                {"type":"always","id":"zone_clear","predicate":"restricted_zone_clear"},
                {"type":"never","id":"no_stop_motion","predicate":"stop_commanded"},
                {"type":"bounded_response","id":"fallback","trigger":"localization_unreliable","required":"safety_fallback","within_ticks":3},
                {"type":"ordered_transition","id":"mission_path","domain":"mission","sequence":[{"from":"pending","to":"running"},{"from":"running","to":"completed"}]},
                {"type":"bounded_progress","id":"progress","within_ticks":4,"minimum_increase_mm":10}
              ]
            }"#,
        )
        .unwrap();

        assert!(matches!(
            &contracts.contracts[2],
            Contract::BoundedResponse {
                trigger_for_ticks: 1,
                ..
            }
        ));
    }

    #[test]
    fn rejects_unknown_fields_versions_duplicate_ids_and_ambiguous_paths() {
        for json in [
            r#"{"schema_version":1,"contracts":[{"type":"always","id":"x","predicate":"mission_active","expression":"true"}]}"#,
            r#"{"schema_version":1,"contracts":[{"type":"always","id":"x","predicate":"arbitrary_expression"}]}"#,
            r#"{"schema_version":2,"contracts":[{"type":"always","id":"x","predicate":"mission_active"}]}"#,
            r#"{"schema_version":1,"contracts":[{"type":"always","id":"x","predicate":"mission_active"},{"type":"never","id":"x","predicate":"mission_active"}]}"#,
            r#"{"schema_version":1,"contracts":[{"type":"ordered_transition","id":"x","domain":"mission","sequence":[{"from":"pending","to":"running"},{"from":"completed","to":"running"}]}]}"#,
            r#"{"schema_version":1,"contracts":[{"type":"always","id":"x","predicate":"mission_active","type2":"unsafe"}]}"#,
            r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"bad_correlation","trigger":"movement_commanded","required":"command_resolved","within_ticks":2}]}"#,
            r#"{"schema_version":1,"contracts":[{"type":"always","id":"bad_terminal","predicate":"command_terminal"}]}"#,
            r#"{"schema_version":1,"contracts":[{"type":"never","id":"bad_terminal","predicate":"command_terminal"}]}"#,
        ] {
            assert!(ContractSet::from_json(json).is_err(), "accepted {json}");
        }
    }

    #[test]
    fn validates_zero_response_bound_and_rejects_oversized_values() {
        assert!(ContractSet::from_json(&" ".repeat(MAX_CONTRACT_BYTES + 1)).is_err());

        let set = ContractSet::from_json(
            r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"same_tick","trigger":"stop_commanded","required":"vehicle_stationary","within_ticks":0}]}"#,
        )
        .unwrap();
        assert!(set.validate().is_ok());

        assert!(ContractSet::from_json(
            r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"too_far","trigger":"stop_commanded","required":"vehicle_stationary","within_ticks":10001}]}"#
        )
        .is_err());
    }
}
