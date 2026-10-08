use std::collections::BTreeMap;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::LabError;
use crate::model::{
    EventPayload, EventRecord, EventsArtifact, RunReport, SCHEMA_VERSION, Scenario,
};

pub(crate) const GENESIS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GpsExogenous {
    pub(crate) packet_sequence: u64,
    pub(crate) truth_at_sample: crate::model::Point,
    pub(crate) observation: Option<crate::model::Point>,
    pub(crate) dropped: bool,
    pub(crate) noise_x_mm: i32,
    pub(crate) noise_y_mm: i32,
    pub(crate) confidence_permille: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PacketExogenous {
    pub(crate) packet_sequence: u64,
    pub(crate) delivered: bool,
    pub(crate) delay_ticks: u32,
    pub(crate) due_tick: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExogenousTick {
    pub(crate) gps: GpsExogenous,
    pub(crate) packet: PacketExogenous,
}

#[derive(Clone, Debug)]
pub(crate) struct Recorder {
    pub(crate) events: Vec<EventRecord>,
    previous_hash: String,
}

impl Recorder {
    pub(crate) fn new() -> Self {
        Self {
            events: Vec::new(),
            previous_hash: GENESIS_HASH.to_owned(),
        }
    }

    pub(crate) fn push(&mut self, tick: u32, event: EventPayload) -> u64 {
        let sequence = self.events.len() as u64 + 1;
        let previous_hash = self.previous_hash.clone();
        let hash = hash_serializable(&EventHashInput {
            sequence,
            tick,
            event: &event,
            previous_hash: &previous_hash,
        });
        self.events.push(EventRecord {
            sequence,
            tick,
            event,
            previous_hash,
            hash: hash.clone(),
        });
        self.previous_hash = hash;
        sequence
    }

    pub(crate) fn final_hash(&self) -> &str {
        &self.previous_hash
    }
}

#[derive(Serialize)]
struct EventHashInput<'a> {
    sequence: u64,
    tick: u32,
    event: &'a EventPayload,
    previous_hash: &'a str,
}

#[derive(Serialize)]
struct UnsignedArtifact<'a> {
    schema_version: u32,
    scenario: &'a Scenario,
    seed: u64,
    events: &'a [EventRecord],
    event_count: u64,
    final_hash: &'a str,
    expected_report: &'a RunReport,
}

pub(crate) fn artifact_digest(
    scenario: &Scenario,
    seed: u64,
    events: &[EventRecord],
    event_count: u64,
    final_hash: &str,
    report: &RunReport,
) -> String {
    hash_serializable(&UnsignedArtifact {
        schema_version: SCHEMA_VERSION,
        scenario,
        seed,
        events,
        event_count,
        final_hash,
        expected_report: report,
    })
}

pub(crate) fn verify_artifact_integrity(artifact: &EventsArtifact) -> Result<(), LabError> {
    if artifact.schema_version != SCHEMA_VERSION {
        return Err(LabError(
            "unsupported events artifact schema_version".into(),
        ));
    }
    if artifact.event_count != artifact.events.len() as u64 {
        return Err(LabError("event_count does not match events array".into()));
    }
    let mut previous_hash = GENESIS_HASH.to_owned();
    for (index, event) in artifact.events.iter().enumerate() {
        let expected_sequence = index as u64 + 1;
        if event.sequence != expected_sequence {
            return Err(LabError(format!(
                "event sequence is invalid at array index {index}"
            )));
        }
        if event.previous_hash != previous_hash {
            return Err(LabError(format!(
                "previous_hash is invalid at event {}",
                event.sequence
            )));
        }
        let expected_hash = hash_serializable(&EventHashInput {
            sequence: event.sequence,
            tick: event.tick,
            event: &event.event,
            previous_hash: &event.previous_hash,
        });
        if event.hash != expected_hash {
            return Err(LabError(format!(
                "hash is invalid at event {}",
                event.sequence
            )));
        }
        previous_hash = event.hash.clone();
    }
    if artifact.final_hash != previous_hash {
        return Err(LabError("final_hash does not match the event chain".into()));
    }
    let expected_manifest = artifact_digest(
        &artifact.scenario,
        artifact.seed,
        &artifact.events,
        artifact.event_count,
        &artifact.final_hash,
        &artifact.expected_report,
    );
    if artifact.artifact_sha256 != expected_manifest {
        return Err(LabError(
            "artifact_sha256 does not match the artifact manifest".into(),
        ));
    }
    Ok(())
}

pub(crate) fn extract_exogenous(
    artifact: &EventsArtifact,
) -> Result<BTreeMap<u32, ExogenousTick>, LabError> {
    let mut gps_by_tick = BTreeMap::new();
    let mut packet_by_tick = BTreeMap::new();
    let mut previous_tick = 0;
    for record in &artifact.events {
        if record.tick == 0 || record.tick > artifact.scenario.steps || record.tick < previous_tick
        {
            return Err(LabError(format!(
                "event tick order is invalid at event {}",
                record.sequence
            )));
        }
        previous_tick = record.tick;
        match &record.event {
            EventPayload::GpsSample {
                packet_sequence,
                truth_at_sample,
                observation,
                dropped,
                noise_x_mm,
                noise_y_mm,
                confidence_permille,
            } => {
                let value = GpsExogenous {
                    packet_sequence: *packet_sequence,
                    truth_at_sample: *truth_at_sample,
                    observation: *observation,
                    dropped: *dropped,
                    noise_x_mm: *noise_x_mm,
                    noise_y_mm: *noise_y_mm,
                    confidence_permille: *confidence_permille,
                };
                if gps_by_tick.insert(record.tick, value).is_some() {
                    return Err(LabError(format!(
                        "duplicate GPS sample for tick {}",
                        record.tick
                    )));
                }
            }
            EventPayload::PacketOutcome {
                packet_sequence,
                delivered,
                delay_ticks,
                due_tick,
            } => {
                let value = PacketExogenous {
                    packet_sequence: *packet_sequence,
                    delivered: *delivered,
                    delay_ticks: *delay_ticks,
                    due_tick: *due_tick,
                };
                if packet_by_tick.insert(record.tick, value).is_some() {
                    return Err(LabError(format!(
                        "duplicate packet outcome for tick {}",
                        record.tick
                    )));
                }
            }
            _ => {}
        }
    }
    let mut result = BTreeMap::new();
    for tick in 1..=artifact.scenario.steps {
        let gps = gps_by_tick
            .remove(&tick)
            .ok_or_else(|| LabError(format!("missing GPS sample for tick {tick}")))?;
        let packet = packet_by_tick
            .remove(&tick)
            .ok_or_else(|| LabError(format!("missing packet outcome for tick {tick}")))?;
        result.insert(tick, ExogenousTick { gps, packet });
    }
    if !gps_by_tick.is_empty() || !packet_by_tick.is_empty() {
        return Err(LabError(
            "exogenous events exist outside configured ticks".into(),
        ));
    }
    Ok(result)
}

fn hash_serializable<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).expect("typed deterministic data always serializes");
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
