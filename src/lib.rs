pub mod actuator;
mod artifact;
pub mod contracts;
pub mod controller;
mod engine;
mod error;
pub mod geometry;
pub mod mission_analysis;
pub mod model;
pub mod prng;
pub mod simulation;
pub mod temporal;
mod verifier;

pub use error::LabError;
pub use mission_analysis::{
    MissionAnalysis, MissionOutcome, MissionPolicy, MissionReason, parse_mission_analysis,
};
pub use model::{EventsArtifact, RunReport, Scenario};
pub use simulation::{
    ContractFailureEvidence, ReplayOutcome, VerificationArtifact, VerificationSummary,
    VerifiedBenchmarkResult, VerifiedReplayOutcome, VerifiedRunOutcome, analyze_artifact,
    benchmark_verified_scenario, parse_artifact, parse_scenario, parse_verification_artifact,
    replay_artifact, replay_verified_artifact, run_scenario, run_verified_scenario,
    status_exit_code,
};
pub use temporal::{
    ContractEvidence, ContractOutcome, ContractResult, EvidenceReason, ExpectedCondition,
    ObservationState, TraceTick, TraceTransition, evaluate,
};
