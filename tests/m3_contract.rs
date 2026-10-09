use autonomous_systems_vv_lab::contracts::{
    ContractSet, PropertyType, TraceFact, TransitionDomain, TransitionState,
};
use autonomous_systems_vv_lab::temporal::{
    ContractOutcome, EvidenceReason, TraceTick, TraceTransition, evaluate,
};

fn tick(tick: u32, telemetry_present: bool, facts: Vec<TraceFact>) -> TraceTick {
    TraceTick {
        tick,
        telemetry_present,
        facts,
        transitions: Vec::new(),
    }
}

fn first_result(
    contract_json: &str,
    trace: &[TraceTick],
) -> autonomous_systems_vv_lab::ContractResult {
    let contracts = ContractSet::from_json(contract_json).expect("valid test contract");
    evaluate(&contracts, trace).remove(0)
}

#[test]
fn validates_a_small_versioned_declarative_operator_set() {
    let contracts = ContractSet::from_json(
        r#"{
            "schema_version": 1,
            "contracts": [
                {"type":"always","id":"zone_clear","predicate":"restricted_zone_clear"},
                {"type":"never","id":"no_fallback_motion","predicate":"movement_while_fallback"},
                {"type":"bounded_response","id":"fallback_response","trigger":"localization_unreliable","required":"safety_fallback","within_ticks":3},
                {"type":"bounded_response","id":"command_resolution","trigger":"movement_commanded","required":"command_resolved","within_ticks":2,"correlation":"command_id"},
                {"type":"ordered_transition","id":"mission_path","domain":"mission","sequence":[{"from":"pending","to":"running"},{"from":"running","to":"completed"}]},
                {"type":"bounded_progress","id":"progress","within_ticks":4,"minimum_increase_mm":10}
            ]
        }"#,
    )
    .expect("schema version 1 supports only typed operators");

    assert_eq!(contracts.contracts.len(), 6);
    assert_eq!(contracts.contracts[0].property_type(), PropertyType::Always);
}

#[test]
fn rejects_unknown_versions_fields_predicates_and_arbitrary_expressions() {
    for json in [
        r#"{"schema_version":2,"contracts":[{"type":"always","id":"x","predicate":"mission_active"}]}"#,
        r#"{"schema_version":1,"contracts":[{"type":"always","id":"x","predicate":"mission_active","expression":"true"}]}"#,
        r#"{"schema_version":1,"contracts":[{"type":"always","id":"x","predicate":"position.x > 0"}]}"#,
        r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"x","trigger":"movement_commanded","required":"command_resolved","within_ticks":2}]}"#,
        r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"x","trigger":"movement_commanded","required":"command_terminal","within_ticks":2}]}"#,
    ] {
        assert!(ContractSet::from_json(json).is_err(), "accepted {json}");
    }

    assert!(ContractSet::from_json(
        r#"{"schema_version":1,"contracts":[{"type":"ordered_transition","id":"actuator_path","domain":"actuator","sequence":[{"from":"idle","to":"moving"}]}]}"#
    )
    .is_err());
}

#[test]
fn always_and_never_are_checked_over_the_finite_trace() {
    let contracts = ContractSet::from_json(
        r#"{"schema_version":1,"contracts":[
            {"type":"always","id":"zone_clear","predicate":"restricted_zone_clear"},
            {"type":"never","id":"fallback_motion","predicate":"movement_while_fallback"}
        ]}"#,
    )
    .unwrap();
    let safe_trace = vec![
        tick(1, true, vec![TraceFact::RestrictedZoneClear]),
        tick(2, true, vec![TraceFact::RestrictedZoneClear]),
    ];
    assert!(
        evaluate(&contracts, &safe_trace)
            .iter()
            .all(|result| result.outcome == ContractOutcome::Pass)
    );

    let violation = vec![
        tick(1, true, vec![TraceFact::RestrictedZoneClear]),
        tick(2, true, vec![TraceFact::MovementWhileFallback]),
    ];
    let results = evaluate(&contracts, &violation);
    assert_eq!(results[0].outcome, ContractOutcome::Fail);
    assert_eq!(results[0].first_violation_tick, Some(2));
    assert_eq!(results[1].outcome, ContractOutcome::Fail);
    assert_eq!(results[1].first_violation_tick, Some(2));
}

#[test]
fn bounded_response_includes_trigger_and_deadline_but_pending_trace_is_inconclusive() {
    let contract = r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"stop_response","trigger":"stop_commanded","required":"vehicle_stationary","within_ticks":2}]}"#;
    let on_trigger = vec![tick(
        1,
        true,
        vec![TraceFact::StopCommanded, TraceFact::VehicleStationary],
    )];
    assert_eq!(
        first_result(contract, &on_trigger).outcome,
        ContractOutcome::Pass
    );

    let exact_deadline = vec![
        tick(1, true, vec![TraceFact::StopCommanded]),
        tick(2, true, vec![]),
        tick(3, true, vec![TraceFact::VehicleStationary]),
    ];
    let passed = first_result(contract, &exact_deadline);
    assert_eq!(passed.outcome, ContractOutcome::Pass);
    assert_eq!(passed.deadline_tick, Some(3));

    let missed_deadline = vec![
        tick(1, true, vec![TraceFact::StopCommanded]),
        tick(2, true, vec![]),
        tick(3, true, vec![]),
    ];
    let failed = first_result(contract, &missed_deadline);
    assert_eq!(failed.outcome, ContractOutcome::Fail);
    assert_eq!(failed.first_trigger_tick, Some(1));
    assert_eq!(failed.deadline_tick, Some(3));
    assert_eq!(failed.first_violation_tick, Some(3));

    let pending = first_result(contract, &[tick(1, true, vec![TraceFact::StopCommanded])]);
    assert_eq!(pending.outcome, ContractOutcome::Inconclusive);
    assert_eq!(
        pending.evidence[0].reason,
        EvidenceReason::TraceEndedBeforeDeadline
    );
}

#[test]
fn repeated_triggers_create_independent_overlapping_obligations() {
    let contract = r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"fallback","trigger":"stop_commanded","required":"safety_fallback","within_ticks":3}]}"#;
    let trace = vec![
        tick(1, true, vec![TraceFact::StopCommanded]),
        tick(2, true, vec![]),
        tick(3, true, vec![TraceFact::StopCommanded]),
        tick(4, true, vec![TraceFact::SafetyFallback]),
    ];
    let result = first_result(contract, &trace);
    assert_eq!(result.outcome, ContractOutcome::Pass);
    assert_eq!(
        result
            .evidence
            .iter()
            .map(|evidence| evidence.trigger_tick)
            .collect::<Vec<_>>(),
        vec![Some(1), Some(3)]
    );
}

#[test]
fn ordered_transition_rejects_an_observed_sequence_in_the_wrong_order() {
    let contract = r#"{"schema_version":1,"contracts":[{"type":"ordered_transition","id":"mission_lifecycle","domain":"mission","sequence":[{"from":"pending","to":"running"},{"from":"running","to":"completed"}]}]}"#;
    let trace = vec![
        TraceTick {
            tick: 1,
            telemetry_present: true,
            facts: Vec::new(),
            transitions: vec![TraceTransition {
                source_sequence: 1,
                domain: TransitionDomain::Mission,
                from: TransitionState::Pending,
                to: TransitionState::Completed,
            }],
        },
        tick(2, true, Vec::new()),
    ];
    let result = first_result(contract, &trace);
    assert_eq!(result.outcome, ContractOutcome::Fail);
    assert_eq!(result.first_violation_tick, Some(1));
    assert_eq!(
        result.evidence[0].reason,
        EvidenceReason::TransitionOrderMismatch
    );
}

#[test]
fn invalid_tick_order_is_not_reported_as_a_passing_property() {
    let contract = r#"{"schema_version":1,"contracts":[{"type":"always","id":"zone_clear","predicate":"restricted_zone_clear"}]}"#;
    let invalid = vec![
        tick(1, true, vec![TraceFact::RestrictedZoneClear]),
        tick(3, true, vec![TraceFact::RestrictedZoneClear]),
    ];
    let result = first_result(contract, &invalid);
    assert_eq!(result.outcome, ContractOutcome::Inconclusive);
    assert_eq!(result.evidence[0].reason, EvidenceReason::InvalidTrace);
}

#[test]
fn command_correlation_requires_the_same_command_identifier() {
    let contract = r#"{"schema_version":1,"contracts":[{"type":"bounded_response","id":"command_application","trigger":"movement_commanded","required":"command_applied","within_ticks":1,"correlation":"command_id"}]}"#;
    let trace = vec![
        tick(
            1,
            true,
            vec![
                TraceFact::MovementCommanded { command_id: 10 },
                TraceFact::MovementCommanded { command_id: 11 },
            ],
        ),
        tick(2, true, vec![TraceFact::CommandApplied { command_id: 11 }]),
    ];
    let result = first_result(contract, &trace);
    assert_eq!(result.outcome, ContractOutcome::Fail);
    assert_eq!(result.first_violation_tick, Some(2));
}
