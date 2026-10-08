use crate::artifact::{artifact_digest, extract_exogenous, verify_artifact_integrity};
use crate::engine::execute;
use crate::error::LabError;
use crate::mission_analysis::{MissionAnalysis, MissionPolicy, analyze_mission};
use crate::model::{EventsArtifact, RunStatus, SCHEMA_VERSION, Scenario};

const MAX_SCENARIO_BYTES: usize = 2 * 1024 * 1024;
const MAX_ARTIFACT_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ReplayOutcome {
    pub report: crate::model::RunReport,
}

pub fn run_scenario(scenario: Scenario, seed: u64) -> Result<EventsArtifact, LabError> {
    scenario
        .validate()
        .map_err(|error| LabError(error.to_string()))?;
    let output = execute(&scenario, seed, None)?;
    let event_count = output.events.len() as u64;
    let final_hash = output
        .events
        .last()
        .map(|event| event.hash.clone())
        .unwrap_or_else(|| crate::artifact::GENESIS_HASH.to_owned());
    let artifact_sha256 = artifact_digest(
        &scenario,
        seed,
        &output.events,
        event_count,
        &final_hash,
        &output.report,
    );
    Ok(EventsArtifact {
        schema_version: SCHEMA_VERSION,
        scenario,
        seed,
        events: output.events,
        event_count,
        final_hash,
        expected_report: output.report,
        artifact_sha256,
    })
}

pub fn replay_artifact(artifact: EventsArtifact) -> Result<ReplayOutcome, LabError> {
    let report = recompute_artifact(&artifact)?;
    Ok(ReplayOutcome { report })
}

pub fn analyze_artifact(
    artifact: EventsArtifact,
    policy: MissionPolicy,
) -> Result<MissionAnalysis, LabError> {
    let report = recompute_artifact(&artifact)?;
    analyze_mission(&artifact.scenario, &report, policy)
}

fn recompute_artifact(artifact: &EventsArtifact) -> Result<crate::model::RunReport, LabError> {
    artifact
        .scenario
        .validate()
        .map_err(|error| LabError(error.to_string()))?;
    verify_artifact_integrity(artifact)?;
    let exogenous = extract_exogenous(artifact)?;
    let replay = execute(&artifact.scenario, artifact.seed, Some(&exogenous))?;
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
    Ok(replay.report)
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

pub fn status_exit_code(status: RunStatus) -> u8 {
    if status == RunStatus::Passed { 0 } else { 2 }
}
