use std::collections::BTreeMap;

use crate::artifact::{ExogenousTick, GpsExogenous, PacketExogenous, Recorder};
use crate::controller::{choose_action, control_context, update_mission, update_safety};
use crate::error::LabError;
use crate::model::{
    EventPayload, MissionState, ObservedTelemetry, Point, RunReport, SCHEMA_VERSION, SafetyState,
    Scenario, TickTelemetry, ValidationMutant,
};
use crate::prng::XorShift64Star;
use crate::verifier::{InvariantTracker, finalize_results, run_status, verify_tick};

#[derive(Clone, Debug)]
pub(crate) struct PendingPacket {
    packet_sequence: u64,
    sample_tick: u32,
    due_tick: u32,
    position: Point,
    confidence_permille: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct AcceptedObservation {
    position: Point,
    sample_tick: u32,
    base_confidence_permille: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct RunState {
    pub(crate) truth: Point,
    pub(crate) heading: crate::model::Heading,
    pub(crate) mission_state: MissionState,
    pub(crate) safety_state: SafetyState,
    pub(crate) waypoint_index: usize,
    observation: Option<AcceptedObservation>,
    pending_packets: Vec<PendingPacket>,
    low_confidence_streak: u32,
    pub(crate) recovery_streak: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct CoreOutput {
    pub(crate) events: Vec<crate::model::EventRecord>,
    pub(crate) report: RunReport,
}

pub(crate) fn execute(
    scenario: &Scenario,
    seed: u64,
    prerecorded: Option<&BTreeMap<u32, ExogenousTick>>,
) -> Result<CoreOutput, LabError> {
    let mut recorder = Recorder::new();
    let mut rng = XorShift64Star::new(seed);
    let mut state = RunState {
        truth: scenario.vehicle.start,
        heading: scenario.vehicle.heading,
        mission_state: MissionState::Pending,
        safety_state: SafetyState::Nominal,
        waypoint_index: 0,
        observation: None,
        pending_packets: Vec::new(),
        low_confidence_streak: 0,
        recovery_streak: 0,
    };
    let mut tracker = InvariantTracker::new(scenario.steps);

    for tick in 1..=scenario.steps {
        let exogenous = next_exogenous(scenario, tick, state.truth, &mut rng, prerecorded)?;
        validate_exogenous(scenario, tick, state.truth, &exogenous)?;
        append_exogenous(&mut recorder, tick, &exogenous);
        enqueue_packet(&mut state.pending_packets, tick, &exogenous);
        deliver_due_packets(
            &mut state,
            tick,
            scenario.sensors.max_observation_age_ticks,
            &mut recorder,
        );

        let observed = observed_telemetry(
            state.observation.as_ref(),
            tick,
            scenario.sensors.max_observation_age_ticks,
        );
        let confidence = observed
            .as_ref()
            .map_or(0, |value| value.confidence_permille);
        let low_confidence = observed.as_ref().is_none_or(|value| {
            !value.fresh
                || value.confidence_permille < scenario.safety.confidence_threshold_permille
        });
        state.low_confidence_streak = if low_confidence {
            state.low_confidence_streak.saturating_add(1)
        } else {
            0
        };

        let mut transition_sequences = Vec::new();
        update_mission(
            scenario,
            tick,
            observed.as_ref(),
            &mut state,
            &mut recorder,
            &mut tracker,
            &mut transition_sequences,
        );
        let control = control_context(
            scenario,
            state.mission_state,
            state.waypoint_index,
            observed.as_ref(),
        );
        let watchdog_required = state.low_confidence_streak >= scenario.safety.fallback_after_ticks;
        let fallback_disabled = scenario
            .validation_mutants
            .contains(&ValidationMutant::DisableSafetyFallback);
        if !fallback_disabled {
            update_safety(
                scenario,
                tick,
                low_confidence,
                watchdog_required,
                &mut state,
                &mut recorder,
                &mut tracker,
                &mut transition_sequences,
            );
        }
        let action = choose_action(scenario, &state, observed.as_ref(), &control);
        let truth_from = state.truth;
        state.truth = match action.heading {
            Some(heading) => state.truth.translated(heading, action.distance_mm),
            None => state.truth,
        };
        if let Some(heading) = action.heading {
            state.heading = heading;
        }

        let telemetry = TickTelemetry {
            tick,
            sim_time_ms: u64::from(tick) * u64::from(scenario.tick_ms),
            truth_position: state.truth,
            observed: observed.clone(),
            observation_age_ticks: observed.as_ref().map(|value| value.age_ticks),
            confidence_permille: confidence,
            low_confidence_streak_ticks: state.low_confidence_streak,
            low_confidence_watchdog_required: watchdog_required,
            heading: state.heading,
            mission_state: state.mission_state,
            safety_state: state.safety_state,
            waypoint_index: state.waypoint_index,
            action,
        };
        let tick_result_sequence = recorder.push(
            tick,
            EventPayload::TickResult {
                truth_from,
                truth_position: state.truth,
                observed: observed.clone(),
                heading: state.heading,
                mission_state: state.mission_state,
                safety_state: state.safety_state,
                waypoint_index: state.waypoint_index,
                action,
            },
        );
        verify_tick(
            scenario,
            tick,
            truth_from,
            state.truth,
            observed.as_ref(),
            confidence,
            watchdog_required,
            state.mission_state,
            state.safety_state,
            action,
            tick_result_sequence,
            &transition_sequences,
            &mut tracker,
            &mut recorder,
        );
        tracker.telemetry.push(telemetry);
    }

    let event_count = recorder.events.len() as u64;
    let (invariants, failed) = finalize_results(&tracker);
    let report = RunReport {
        schema_version: SCHEMA_VERSION,
        scenario_name: scenario.name.clone(),
        seed,
        tick_ms: scenario.tick_ms,
        status: run_status(failed),
        ticks: scenario.steps,
        event_count,
        final_hash: recorder.final_hash().to_owned(),
        final_truth: state.truth,
        final_observed: observed_telemetry(
            state.observation.as_ref(),
            scenario.steps,
            scenario.sensors.max_observation_age_ticks,
        ),
        invariants,
        transitions: tracker.transitions,
        telemetry: tracker.telemetry,
    };
    Ok(CoreOutput {
        events: recorder.events,
        report,
    })
}

fn next_exogenous(
    scenario: &Scenario,
    tick: u32,
    truth: Point,
    rng: &mut XorShift64Star,
    prerecorded: Option<&BTreeMap<u32, ExogenousTick>>,
) -> Result<ExogenousTick, LabError> {
    let Some(events) = prerecorded else {
        return generate_exogenous(scenario, tick, truth, rng);
    };
    let supplied = events
        .get(&tick)
        .cloned()
        .ok_or_else(|| LabError(format!("missing exogenous events for tick {tick}")))?;
    let expected = generate_exogenous(scenario, tick, truth, rng)?;
    if supplied != expected {
        return Err(LabError(format!(
            "recorded GPS or communication fault does not match seed at tick {tick}"
        )));
    }
    Ok(supplied)
}

fn generate_exogenous(
    scenario: &Scenario,
    tick: u32,
    truth: Point,
    rng: &mut XorShift64Star,
) -> Result<ExogenousTick, LabError> {
    let gps = &scenario.sensors.gps;
    let random_dropout = rng.chance_permille(gps.dropout_permille);
    let noise_x_mm = rng.inclusive_i32(-gps.noise_max_mm, gps.noise_max_mm);
    let noise_y_mm = rng.inclusive_i32(-gps.noise_max_mm, gps.noise_max_mm);
    let dropped = window_contains(&gps.dropout_windows, tick) || random_dropout;
    let confidence_permille = confidence_from_noise(noise_x_mm, noise_y_mm, gps.noise_max_mm);
    let observation = if dropped {
        None
    } else {
        Some(Point {
            x_mm: truth.x_mm + noise_x_mm,
            y_mm: truth.y_mm + noise_y_mm,
        })
    };

    let communication = &scenario.sensors.communication;
    let random_loss = rng.chance_permille(communication.packet_loss_permille);
    let delay_ticks =
        rng.inclusive_u32(communication.delay_min_ticks, communication.delay_max_ticks);
    let delivered = !window_contains(&communication.packet_loss_windows, tick) && !random_loss;
    let due_tick = if delivered {
        Some(
            tick.checked_add(delay_ticks)
                .ok_or_else(|| LabError("packet due tick overflow".into()))?,
        )
    } else {
        None
    };
    Ok(ExogenousTick {
        gps: GpsExogenous {
            packet_sequence: u64::from(tick),
            truth_at_sample: truth,
            observation,
            dropped,
            noise_x_mm,
            noise_y_mm,
            confidence_permille,
        },
        packet: PacketExogenous {
            packet_sequence: u64::from(tick),
            delivered,
            delay_ticks,
            due_tick,
        },
    })
}

fn validate_exogenous(
    scenario: &Scenario,
    tick: u32,
    truth: Point,
    exogenous: &ExogenousTick,
) -> Result<(), LabError> {
    let gps = &exogenous.gps;
    if gps.packet_sequence != u64::from(tick) || gps.truth_at_sample != truth {
        return Err(LabError(format!(
            "GPS source event does not match truth at tick {tick}"
        )));
    }
    if gps.dropped != gps.observation.is_none() {
        return Err(LabError(format!(
            "GPS dropout fields disagree at tick {tick}"
        )));
    }
    let noise_limit = scenario.sensors.gps.noise_max_mm as u32;
    if gps.noise_x_mm.unsigned_abs() > noise_limit || gps.noise_y_mm.unsigned_abs() > noise_limit {
        return Err(LabError(format!(
            "GPS noise exceeds configured limit at tick {tick}"
        )));
    }
    if gps.confidence_permille
        != confidence_from_noise(
            gps.noise_x_mm,
            gps.noise_y_mm,
            scenario.sensors.gps.noise_max_mm,
        )
    {
        return Err(LabError(format!(
            "GPS confidence does not match recorded noise at tick {tick}"
        )));
    }
    if let Some(observation) = gps.observation {
        let expected = Point {
            x_mm: truth.x_mm + gps.noise_x_mm,
            y_mm: truth.y_mm + gps.noise_y_mm,
        };
        if observation != expected {
            return Err(LabError(format!(
                "GPS observation does not match recorded noise at tick {tick}"
            )));
        }
    }
    if window_contains(&scenario.sensors.gps.dropout_windows, tick) && !gps.dropped {
        return Err(LabError(format!(
            "GPS sample in a configured dropout window was not dropped at tick {tick}"
        )));
    }

    let packet = &exogenous.packet;
    let communication = &scenario.sensors.communication;
    if packet.packet_sequence != u64::from(tick)
        || packet.delivered != packet.due_tick.is_some()
        || packet.delay_ticks < communication.delay_min_ticks
        || packet.delay_ticks > communication.delay_max_ticks
    {
        return Err(LabError(format!(
            "communication outcome is invalid at tick {tick}"
        )));
    }
    let expected_due_tick = if packet.delivered {
        Some(
            tick.checked_add(packet.delay_ticks)
                .ok_or_else(|| LabError("packet due tick overflow".into()))?,
        )
    } else {
        None
    };
    if packet.due_tick != expected_due_tick {
        return Err(LabError(format!(
            "packet due tick is invalid at tick {tick}"
        )));
    }
    if window_contains(&communication.packet_loss_windows, tick) && packet.delivered {
        return Err(LabError(format!(
            "packet in a configured loss window was delivered at tick {tick}"
        )));
    }
    Ok(())
}

fn append_exogenous(recorder: &mut Recorder, tick: u32, exogenous: &ExogenousTick) {
    recorder.push(
        tick,
        EventPayload::GpsSample {
            packet_sequence: exogenous.gps.packet_sequence,
            truth_at_sample: exogenous.gps.truth_at_sample,
            observation: exogenous.gps.observation,
            dropped: exogenous.gps.dropped,
            noise_x_mm: exogenous.gps.noise_x_mm,
            noise_y_mm: exogenous.gps.noise_y_mm,
            confidence_permille: exogenous.gps.confidence_permille,
        },
    );
    recorder.push(
        tick,
        EventPayload::PacketOutcome {
            packet_sequence: exogenous.packet.packet_sequence,
            delivered: exogenous.packet.delivered,
            delay_ticks: exogenous.packet.delay_ticks,
            due_tick: exogenous.packet.due_tick,
        },
    );
}

fn enqueue_packet(queue: &mut Vec<PendingPacket>, tick: u32, exogenous: &ExogenousTick) {
    let (Some(position), Some(due_tick)) = (exogenous.gps.observation, exogenous.packet.due_tick)
    else {
        return;
    };
    queue.push(PendingPacket {
        packet_sequence: exogenous.packet.packet_sequence,
        sample_tick: tick,
        due_tick,
        position,
        confidence_permille: exogenous.gps.confidence_permille,
    });
}

fn deliver_due_packets(
    state: &mut RunState,
    tick: u32,
    max_age_ticks: u32,
    recorder: &mut Recorder,
) {
    state
        .pending_packets
        .sort_by_key(|packet| (packet.due_tick, packet.packet_sequence));
    let mut still_pending = Vec::new();
    for packet in state.pending_packets.drain(..) {
        if packet.due_tick > tick {
            still_pending.push(packet);
            continue;
        }
        let newer_than_latest = state
            .observation
            .as_ref()
            .is_none_or(|observation| packet.sample_tick > observation.sample_tick);
        let not_expired = tick.saturating_sub(packet.sample_tick) <= max_age_ticks;
        let accepted = newer_than_latest && not_expired;
        recorder.push(
            tick,
            EventPayload::SensorDelivery {
                packet_sequence: packet.packet_sequence,
                sample_tick: packet.sample_tick,
                position: packet.position,
                confidence_permille: packet.confidence_permille,
                accepted,
                rejection_reason: (!accepted).then(|| "stale_packet".to_owned()),
            },
        );
        if accepted {
            state.observation = Some(AcceptedObservation {
                position: packet.position,
                sample_tick: packet.sample_tick,
                base_confidence_permille: packet.confidence_permille,
            });
        }
    }
    state.pending_packets = still_pending;
}

fn observed_telemetry(
    observation: Option<&AcceptedObservation>,
    tick: u32,
    max_age_ticks: u32,
) -> Option<ObservedTelemetry> {
    observation.map(|sample| {
        let age_ticks = tick.saturating_sub(sample.sample_tick);
        let fresh = age_ticks <= max_age_ticks;
        let confidence_permille = if fresh {
            let remaining = max_age_ticks + 1 - age_ticks;
            (u32::from(sample.base_confidence_permille) * remaining / (max_age_ticks + 1)) as u16
        } else {
            0
        };
        ObservedTelemetry {
            position: sample.position,
            sample_tick: sample.sample_tick,
            age_ticks,
            base_confidence_permille: sample.base_confidence_permille,
            confidence_permille,
            fresh,
        }
    })
}

fn window_contains(windows: &[crate::model::TickWindow], tick: u32) -> bool {
    windows
        .iter()
        .any(|window| tick >= window.start_tick && tick <= window.end_tick)
}

fn confidence_from_noise(noise_x_mm: i32, noise_y_mm: i32, maximum_noise_mm: i32) -> u16 {
    if maximum_noise_mm == 0 {
        return 1000;
    }
    let error = u64::from(noise_x_mm.unsigned_abs()) + u64::from(noise_y_mm.unsigned_abs());
    let maximum_error = 2 * maximum_noise_mm as u64;
    (1000_u64.saturating_sub(error * 1000 / maximum_error)) as u16
}
