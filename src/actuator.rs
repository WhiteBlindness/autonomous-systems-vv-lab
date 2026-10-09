//! Modelo determinístico de entrega e execução de comandos do atuador.
//!
//! Os intervalos de falha são inclusivos e referem-se ao passo em que o
//! controlador emite o comando. Um comando atrasado fica agendado para
//! `passo_de_origem + delay_ticks` e pode ser executado depois do fim do
//! intervalo de falha. Em cada passo, o modelo avalia primeiro o comando
//! atual e, depois, executa no máximo um comando atrasado vencido. Por isso,
//! um comando atrasado pode sobrepor-se a um pedido de paragem mais recente.
//!
//! As falhas simultâneas são sorteadas por identificador, em ordem
//! lexicográfica. Para um comando de movimento, perder o comando tem
//! precedência sobre o atraso; falhas de movimento contínuo só se aplicam a
//! um comando de paragem. Se houver vários comandos atrasados vencidos,
//! executa-se primeiro o mais antigo, ordenado por instante devido e passo de
//! origem. Os restantes ficam pendentes para os passos seguintes.

use serde::{Deserialize, Serialize};

use crate::model::Action;
use crate::prng::XorShift64Star;

pub const ACTUATOR_SCHEMA_VERSION: u32 = 1;
const MAX_STEPS: u32 = 10_000;
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_FAULTS: usize = 64;
const MAX_TOTAL_WINDOWS: usize = 256;
const MAX_DELAY_TICKS: u32 = 10_000;
const MAX_FAULT_ID_BYTES: usize = 64;
const ACTUATOR_SEED_DOMAIN: u64 = 0x4154_5452_2d4d_3301;

/// Configuração declarativa e versionada do atuador.
///
/// A configuração predefinida não injeta falhas e preserva a ação do M2.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActuatorConfig {
    pub schema_version: u32,
    /// Semente própria do atuador, combinada com a semente da missão.
    pub seed: u64,
    #[serde(default)]
    pub faults: Vec<ActuatorFault>,
}

impl Default for ActuatorConfig {
    fn default() -> Self {
        Self {
            schema_version: ACTUATOR_SCHEMA_VERSION,
            seed: 0,
            faults: Vec::new(),
        }
    }
}

impl ActuatorConfig {
    /// Lê JSON estrito e valida versão, campos e limites independentes da missão.
    pub fn from_json(text: &str) -> Result<Self, String> {
        if text.len() > MAX_CONFIG_BYTES {
            return Err("actuator configuration exceeds the 1 MiB limit".into());
        }
        let config: Self = serde_json::from_str(text).map_err(|error| error.to_string())?;
        config.validate_structure()?;
        Ok(config)
    }

    /// Valida a configuração contra o número de passos da missão.
    pub fn validate(&self, steps: u32) -> Result<(), String> {
        self.validate_structure()?;
        if !(1..=MAX_STEPS).contains(&steps) {
            return Err(format!("steps must be between 1 and {MAX_STEPS}"));
        }
        for fault in &self.faults {
            for window in fault.windows() {
                if window.end_tick > steps {
                    return Err(format!(
                        "fault '{}' window end_tick must not exceed steps",
                        fault.id()
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_structure(&self) -> Result<(), String> {
        if self.schema_version != ACTUATOR_SCHEMA_VERSION {
            return Err("unsupported actuator schema_version".into());
        }
        if self.faults.len() > MAX_FAULTS {
            return Err(format!(
                "at most {MAX_FAULTS} actuator faults are supported"
            ));
        }

        let mut ids = std::collections::BTreeSet::new();
        let mut window_count = 0usize;
        for fault in &self.faults {
            let id = fault.id();
            if id.is_empty()
                || id.len() > MAX_FAULT_ID_BYTES
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(format!(
                    "fault id '{id}' must contain 1 to {MAX_FAULT_ID_BYTES} ASCII letters, digits, '_' or '-'"
                ));
            }
            if !ids.insert(id) {
                return Err(format!("duplicate actuator fault id '{id}'"));
            }
            if fault.windows().is_empty() {
                return Err(format!("fault '{id}' must define at least one window"));
            }
            window_count = window_count.saturating_add(fault.windows().len());
            if fault.probability_permille() > 1000 {
                return Err(format!(
                    "fault '{id}' probability_permille cannot exceed 1000"
                ));
            }
            if let ActuatorFault::DelayedMovement { delay_ticks, .. } = fault
                && !(1..=MAX_DELAY_TICKS).contains(delay_ticks)
            {
                return Err(format!(
                    "fault '{id}' delay_ticks must be between 1 and {MAX_DELAY_TICKS}"
                ));
            }
            for window in fault.windows() {
                if window.start_tick == 0 || window.start_tick > window.end_tick {
                    return Err(format!(
                        "fault '{id}' windows must be inclusive ranges with start_tick >= 1 and start_tick <= end_tick"
                    ));
                }
            }
        }
        if window_count > MAX_TOTAL_WINDOWS {
            return Err(format!(
                "at most {MAX_TOTAL_WINDOWS} actuator fault windows are supported"
            ));
        }
        Ok(())
    }
}

/// Intervalo inclusivo de atividade de uma falha do atuador.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActuatorFaultWindow {
    pub start_tick: u32,
    pub end_tick: u32,
}

/// Falha limitada e declarativa. Não aceita expressões executáveis.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActuatorFault {
    LostMovement {
        id: String,
        windows: Vec<ActuatorFaultWindow>,
        probability_permille: u16,
    },
    DelayedMovement {
        id: String,
        windows: Vec<ActuatorFaultWindow>,
        probability_permille: u16,
        delay_ticks: u32,
    },
    ContinuedMovement {
        id: String,
        windows: Vec<ActuatorFaultWindow>,
        probability_permille: u16,
    },
}

impl ActuatorFault {
    fn id(&self) -> &str {
        match self {
            Self::LostMovement { id, .. }
            | Self::DelayedMovement { id, .. }
            | Self::ContinuedMovement { id, .. } => id,
        }
    }

    fn windows(&self) -> &[ActuatorFaultWindow] {
        match self {
            Self::LostMovement { windows, .. }
            | Self::DelayedMovement { windows, .. }
            | Self::ContinuedMovement { windows, .. } => windows,
        }
    }

    fn probability_permille(&self) -> u16 {
        match self {
            Self::LostMovement {
                probability_permille,
                ..
            }
            | Self::DelayedMovement {
                probability_permille,
                ..
            }
            | Self::ContinuedMovement {
                probability_permille,
                ..
            } => *probability_permille,
        }
    }

    fn is_active_at(&self, tick: u32) -> bool {
        self.windows()
            .iter()
            .any(|window| window.start_tick <= tick && tick <= window.end_tick)
    }
}

/// Classificação do comportamento observado neste passo.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActuatorBehavior {
    Identity,
    Lost,
    Delayed,
    ContinuedMovement,
    DelayedExecution,
}

/// Tipo de evento emitido pelo modelo do atuador.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActuatorEventKind {
    FaultWindowActive,
    CommandLost,
    CommandDelayed,
    ContinuedMovement,
    DelayedCommandExecuted,
    CurrentCommandIgnoredByDelayedCommand,
    PendingAtTermination,
}

/// Evidência observada ao aplicar uma falha do atuador.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActuatorFaultEvent {
    /// Passo em que o evento foi observado.
    pub tick: u32,
    pub fault_id: String,
    pub kind: ActuatorEventKind,
    /// Passo em que o controlador emitiu o comando associado.
    pub source_command_tick: u32,
    /// Passo em que um comando atrasado deveria ficar elegível.
    pub due_tick: Option<u32>,
    /// Passo em que o comando atrasado se moveu fisicamente.
    pub execution_tick: Option<u32>,
}

/// Comando de movimento ainda não executado no fim da simulação.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PendingActuationCommand {
    pub action: Action,
    pub fault_id: String,
    pub source_command_tick: u32,
    pub due_tick: u32,
}

/// Resultado físico da passagem de um comando pelo atuador.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActuationOutcome {
    pub tick: u32,
    pub requested_action: Action,
    pub realized_action: Action,
    pub behavior: ActuatorBehavior,
    /// Passo do comando que produziu `realized_action`.
    pub source_command_tick: u32,
    pub fault_events: Vec<ActuatorFaultEvent>,
    /// Só é preenchido no último passo da simulação.
    pub pending_commands_at_termination: Vec<PendingActuationCommand>,
}

/// Modelo de atuador executado uma vez por cada passo, de 1 até `steps`.
pub struct ActuatorModel {
    steps: u32,
    next_tick: u32,
    faults: Vec<ActuatorFault>,
    rng: XorShift64Star,
    pending: Vec<PendingActuationCommand>,
    last_realized_movement: Option<(Action, u32)>,
}

impl ActuatorModel {
    /// Cria o modelo e combina a semente da missão com a semente do atuador.
    pub fn new(config: ActuatorConfig, mission_seed: u64, steps: u32) -> Result<Self, String> {
        config.validate(steps)?;
        let mut faults = config.faults;
        faults.sort_by(|left, right| left.id().cmp(right.id()));
        Ok(Self {
            steps,
            next_tick: 1,
            faults,
            rng: XorShift64Star::new(combine_seeds(mission_seed, config.seed)),
            pending: Vec::new(),
            last_realized_movement: None,
        })
    }

    /// Aplica um comando no próximo passo lógico.
    ///
    /// Os passos têm de ser fornecidos exatamente uma vez e pela ordem
    /// `1..=steps`; uma chamada fora de ordem indica um erro de integração e
    /// provoca uma asserção em vez de produzir uma trajetória ambígua.
    pub fn apply(&mut self, tick: u32, requested_action: Action) -> ActuationOutcome {
        assert_eq!(
            tick, self.next_tick,
            "actuator ticks must be applied sequentially from 1"
        );
        assert!(tick <= self.steps, "actuator tick exceeds configured steps");

        let mut fault_events = Vec::new();
        let active_fault_ids = self.active_fault_ids(tick);
        for fault_id in &active_fault_ids {
            fault_events.push(ActuatorFaultEvent {
                tick,
                fault_id: fault_id.clone(),
                kind: ActuatorEventKind::FaultWindowActive,
                source_command_tick: tick,
                due_tick: None,
                execution_tick: None,
            });
        }

        let mut realized_action = requested_action;
        let mut behavior = ActuatorBehavior::Identity;
        let mut source_command_tick = tick;
        let movement_requested = is_movement(requested_action);

        if movement_requested {
            if let Some(fault) = self.selected_fault(&active_fault_ids, FaultSelection::Lost) {
                let fault_id = fault.id().to_owned();
                realized_action = Action::hold();
                behavior = ActuatorBehavior::Lost;
                fault_events.push(event(
                    tick,
                    &fault_id,
                    ActuatorEventKind::CommandLost,
                    tick,
                    None,
                    None,
                ));
            } else if let Some(fault) =
                self.selected_fault(&active_fault_ids, FaultSelection::Delayed)
            {
                let fault_id = fault.id().to_owned();
                let ActuatorFault::DelayedMovement { delay_ticks, .. } = fault else {
                    unreachable!("selection returns only delayed movement faults")
                };
                let due_tick = tick + delay_ticks;
                self.pending.push(PendingActuationCommand {
                    action: requested_action,
                    fault_id: fault_id.clone(),
                    source_command_tick: tick,
                    due_tick,
                });
                self.sort_pending();
                realized_action = Action::hold();
                behavior = ActuatorBehavior::Delayed;
                fault_events.push(event(
                    tick,
                    &fault_id,
                    ActuatorEventKind::CommandDelayed,
                    tick,
                    Some(due_tick),
                    None,
                ));
            }
        } else if is_stop(requested_action)
            && let Some(fault) = self.selected_fault(&active_fault_ids, FaultSelection::Continued)
            && let Some((previous_movement, previous_source_tick)) = self.last_realized_movement
        {
            let fault_id = fault.id().to_owned();
            realized_action = previous_movement;
            source_command_tick = previous_source_tick;
            behavior = ActuatorBehavior::ContinuedMovement;
            fault_events.push(event(
                tick,
                &fault_id,
                ActuatorEventKind::ContinuedMovement,
                previous_source_tick,
                None,
                None,
            ));
        }

        // A due stale command executes after the current request. This explicit
        // order makes a delayed movement capable of defeating a later stop.
        if let Some(index) = self
            .pending
            .iter()
            .position(|pending| pending.due_tick <= tick)
        {
            let pending = self.pending.remove(index);
            fault_events.push(event(
                tick,
                &pending.fault_id,
                ActuatorEventKind::CurrentCommandIgnoredByDelayedCommand,
                tick,
                Some(pending.due_tick),
                Some(tick),
            ));
            realized_action = pending.action;
            source_command_tick = pending.source_command_tick;
            behavior = ActuatorBehavior::DelayedExecution;
            fault_events.push(event(
                tick,
                &pending.fault_id,
                ActuatorEventKind::DelayedCommandExecuted,
                pending.source_command_tick,
                Some(pending.due_tick),
                Some(tick),
            ));
        }

        if is_movement(realized_action) {
            self.last_realized_movement = Some((realized_action, source_command_tick));
        }

        let pending_commands_at_termination = if tick == self.steps {
            for pending in &self.pending {
                fault_events.push(event(
                    tick,
                    &pending.fault_id,
                    ActuatorEventKind::PendingAtTermination,
                    pending.source_command_tick,
                    Some(pending.due_tick),
                    None,
                ));
            }
            self.pending.clone()
        } else {
            Vec::new()
        };
        self.next_tick += 1;

        ActuationOutcome {
            tick,
            requested_action,
            realized_action,
            behavior,
            source_command_tick,
            fault_events,
            pending_commands_at_termination,
        }
    }

    /// Devolve os comandos que ainda aguardam execução.
    pub fn pending_commands(&self) -> &[PendingActuationCommand] {
        &self.pending
    }

    fn active_fault_ids(&mut self, tick: u32) -> Vec<String> {
        let mut active = Vec::new();
        for fault in &self.faults {
            if fault.is_active_at(tick) && self.rng.chance_permille(fault.probability_permille()) {
                active.push(fault.id().to_owned());
            }
        }
        active
    }

    fn selected_fault(
        &self,
        active_ids: &[String],
        selection: FaultSelection,
    ) -> Option<ActuatorFault> {
        self.faults
            .iter()
            .find(|fault| {
                active_ids
                    .iter()
                    .any(|active_id| active_id.as_str() == fault.id())
                    && selection.matches(fault)
            })
            .cloned()
    }

    fn sort_pending(&mut self) {
        self.pending.sort_by_key(|pending| {
            (
                pending.due_tick,
                pending.source_command_tick,
                pending.fault_id.clone(),
            )
        });
    }
}

#[derive(Clone, Copy)]
enum FaultSelection {
    Lost,
    Delayed,
    Continued,
}

impl FaultSelection {
    fn matches(self, fault: &ActuatorFault) -> bool {
        matches!(
            (self, fault),
            (Self::Lost, ActuatorFault::LostMovement { .. })
                | (Self::Delayed, ActuatorFault::DelayedMovement { .. })
                | (Self::Continued, ActuatorFault::ContinuedMovement { .. })
        )
    }
}

fn is_movement(action: Action) -> bool {
    action.heading.is_some() && action.distance_mm > 0
}

fn is_stop(action: Action) -> bool {
    action.heading.is_none() && action.distance_mm == 0
}

fn event(
    tick: u32,
    fault_id: &str,
    kind: ActuatorEventKind,
    source_command_tick: u32,
    due_tick: Option<u32>,
    execution_tick: Option<u32>,
) -> ActuatorFaultEvent {
    ActuatorFaultEvent {
        tick,
        fault_id: fault_id.to_owned(),
        kind,
        source_command_tick,
        due_tick,
        execution_tick,
    }
}

fn combine_seeds(mission_seed: u64, actuator_seed: u64) -> u64 {
    splitmix64(mission_seed ^ splitmix64(actuator_seed ^ ACTUATOR_SEED_DOMAIN))
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Heading;

    fn window(start_tick: u32, end_tick: u32) -> ActuatorFaultWindow {
        ActuatorFaultWindow {
            start_tick,
            end_tick,
        }
    }

    fn lost(id: &str, start_tick: u32, end_tick: u32, probability_permille: u16) -> ActuatorFault {
        ActuatorFault::LostMovement {
            id: id.into(),
            windows: vec![window(start_tick, end_tick)],
            probability_permille,
        }
    }

    fn delayed(id: &str, start_tick: u32, end_tick: u32, delay_ticks: u32) -> ActuatorFault {
        ActuatorFault::DelayedMovement {
            id: id.into(),
            windows: vec![window(start_tick, end_tick)],
            probability_permille: 1000,
            delay_ticks,
        }
    }

    fn continued(id: &str, start_tick: u32, end_tick: u32) -> ActuatorFault {
        ActuatorFault::ContinuedMovement {
            id: id.into(),
            windows: vec![window(start_tick, end_tick)],
            probability_permille: 1000,
        }
    }

    fn config(faults: Vec<ActuatorFault>) -> ActuatorConfig {
        ActuatorConfig {
            schema_version: ACTUATOR_SCHEMA_VERSION,
            seed: 55,
            faults,
        }
    }

    fn movement() -> Action {
        Action::step(Heading::East, 10)
    }

    fn apply(model: &mut ActuatorModel, tick: u32, action: Action) -> ActuationOutcome {
        model.apply(tick, action)
    }

    #[test]
    fn default_configuration_is_identity_behavior() {
        let mut model = ActuatorModel::new(ActuatorConfig::default(), 99, 2).unwrap();
        let first = apply(&mut model, 1, movement());
        let second = apply(&mut model, 2, Action::hold());
        assert_eq!(first.realized_action, movement());
        assert_eq!(first.behavior, ActuatorBehavior::Identity);
        assert_eq!(first.source_command_tick, 1);
        assert_eq!(first.fault_events, Vec::new());
        assert_eq!(second.realized_action, Action::hold());
        assert_eq!(second.behavior, ActuatorBehavior::Identity);
    }

    #[test]
    fn fault_windows_include_both_endpoints() {
        let mut model = ActuatorModel::new(config(vec![lost("loss", 2, 3, 1000)]), 99, 4).unwrap();
        assert_eq!(apply(&mut model, 1, movement()).realized_action, movement());
        assert_eq!(
            apply(&mut model, 2, movement()).behavior,
            ActuatorBehavior::Lost
        );
        assert_eq!(
            apply(&mut model, 3, movement()).behavior,
            ActuatorBehavior::Lost
        );
        assert_eq!(apply(&mut model, 4, movement()).realized_action, movement());
    }

    #[test]
    fn delayed_command_executes_after_later_stop_and_preserves_source_tick() {
        let mut model = ActuatorModel::new(config(vec![delayed("delay", 1, 1, 2)]), 7, 4).unwrap();
        let scheduled = apply(&mut model, 1, movement());
        assert_eq!(scheduled.behavior, ActuatorBehavior::Delayed);
        assert_eq!(scheduled.realized_action, Action::hold());
        assert_eq!(scheduled.fault_events[1].due_tick, Some(3));
        assert_eq!(
            apply(&mut model, 2, Action::hold()).realized_action,
            Action::hold()
        );

        let executed = apply(&mut model, 3, Action::hold());
        assert_eq!(executed.requested_action, Action::hold());
        assert_eq!(executed.realized_action, movement());
        assert_eq!(executed.behavior, ActuatorBehavior::DelayedExecution);
        assert_eq!(executed.source_command_tick, 1);
        assert_eq!(
            executed.fault_events[0].kind,
            ActuatorEventKind::CurrentCommandIgnoredByDelayedCommand
        );
        assert_eq!(executed.fault_events[0].source_command_tick, 3);
        assert_eq!(executed.fault_events[0].execution_tick, Some(3));
        assert_eq!(
            executed.fault_events[1].kind,
            ActuatorEventKind::DelayedCommandExecuted
        );
        assert_eq!(executed.fault_events[1].source_command_tick, 1);
        assert_eq!(executed.fault_events[1].due_tick, Some(3));
        assert_eq!(executed.fault_events[1].execution_tick, Some(3));
    }

    #[test]
    fn continued_actuation_reuses_last_realized_movement_after_stop() {
        let mut model = ActuatorModel::new(config(vec![continued("stuck", 2, 3)]), 9, 3).unwrap();
        assert_eq!(apply(&mut model, 1, movement()).realized_action, movement());
        let stopped = apply(&mut model, 2, Action::hold());
        assert_eq!(stopped.behavior, ActuatorBehavior::ContinuedMovement);
        assert_eq!(stopped.realized_action, movement());
        assert_eq!(stopped.source_command_tick, 1);
        assert_eq!(
            stopped.fault_events[1].kind,
            ActuatorEventKind::ContinuedMovement
        );
        assert_eq!(stopped.fault_events[1].source_command_tick, 1);
        assert_eq!(
            apply(&mut model, 3, Action::hold()).realized_action,
            movement()
        );
    }

    #[test]
    fn simultaneous_faults_have_stable_order_and_lost_precedes_delayed() {
        let mut first = ActuatorModel::new(
            config(vec![
                delayed("z-delay", 1, 1, 1),
                lost("a-loss", 1, 1, 1000),
            ]),
            11,
            2,
        )
        .unwrap();
        let mut reordered = ActuatorModel::new(
            config(vec![
                lost("a-loss", 1, 1, 1000),
                delayed("z-delay", 1, 1, 1),
            ]),
            11,
            2,
        )
        .unwrap();
        let a = apply(&mut first, 1, movement());
        let b = apply(&mut reordered, 1, movement());
        assert_eq!(a, b);
        assert_eq!(a.behavior, ActuatorBehavior::Lost);
        assert_eq!(a.fault_events[0].fault_id, "a-loss");
        assert_eq!(a.fault_events[1].fault_id, "z-delay");
        assert_eq!(first.pending_commands(), &[]);
    }

    #[test]
    fn overlapping_windows_of_one_fault_activate_once_per_tick() {
        let fault = ActuatorFault::LostMovement {
            id: "loss".into(),
            windows: vec![window(1, 2), window(2, 3)],
            probability_permille: 1000,
        };
        let mut model = ActuatorModel::new(config(vec![fault]), 1, 3).unwrap();
        let _ = apply(&mut model, 1, movement());
        let result = apply(&mut model, 2, movement());
        assert_eq!(
            result
                .fault_events
                .iter()
                .filter(|event| event.kind == ActuatorEventKind::FaultWindowActive)
                .count(),
            1
        );
    }

    #[test]
    fn fault_probabilities_and_combined_seed_are_reproducible() {
        let config = config(vec![lost("intermittent", 1, 100, 425)]);
        let mut first = ActuatorModel::new(config.clone(), 17, 100).unwrap();
        let mut repeat = ActuatorModel::new(config.clone(), 17, 100).unwrap();
        let mut other_mission_seed = ActuatorModel::new(config, 18, 100).unwrap();
        let first_trace = (1..=100)
            .map(|tick| apply(&mut first, tick, movement()))
            .collect::<Vec<_>>();
        let repeat_trace = (1..=100)
            .map(|tick| apply(&mut repeat, tick, movement()))
            .collect::<Vec<_>>();
        let other_trace = (1..=100)
            .map(|tick| apply(&mut other_mission_seed, tick, movement()))
            .collect::<Vec<_>>();
        assert_eq!(first_trace, repeat_trace);
        assert_ne!(first_trace, other_trace);
        let lost_count = first_trace
            .iter()
            .filter(|outcome| outcome.behavior == ActuatorBehavior::Lost)
            .count();
        assert!((20..=65).contains(&lost_count));
    }

    #[test]
    fn delayed_commands_due_together_execute_in_source_order_one_per_tick() {
        let faults = vec![delayed("delay", 1, 2, 2)];
        let mut model = ActuatorModel::new(config(faults), 4, 5).unwrap();
        let first_action = Action::step(Heading::East, 10);
        let second_action = Action::step(Heading::North, 20);
        assert_eq!(
            apply(&mut model, 1, first_action).behavior,
            ActuatorBehavior::Delayed
        );
        assert_eq!(
            apply(&mut model, 2, second_action).behavior,
            ActuatorBehavior::Delayed
        );

        let first_due = apply(&mut model, 3, Action::hold());
        assert_eq!(first_due.realized_action, first_action);
        assert_eq!(first_due.source_command_tick, 1);
        let second_due = apply(&mut model, 4, Action::hold());
        assert_eq!(second_due.realized_action, second_action);
        assert_eq!(second_due.source_command_tick, 2);
    }

    #[test]
    fn pending_command_is_reported_when_due_after_termination() {
        let mut model = ActuatorModel::new(config(vec![delayed("late", 2, 2, 3)]), 2, 3).unwrap();
        apply(&mut model, 1, Action::hold());
        apply(&mut model, 2, movement());
        let final_outcome = apply(&mut model, 3, Action::hold());
        assert_eq!(final_outcome.pending_commands_at_termination.len(), 1);
        assert_eq!(
            final_outcome.pending_commands_at_termination[0].source_command_tick,
            2
        );
        assert_eq!(final_outcome.pending_commands_at_termination[0].due_tick, 5);
        assert_eq!(
            final_outcome.fault_events[0].kind,
            ActuatorEventKind::PendingAtTermination
        );
    }

    #[test]
    fn due_on_final_tick_executes_before_termination_report() {
        let mut model = ActuatorModel::new(config(vec![delayed("final", 1, 1, 2)]), 2, 3).unwrap();
        apply(&mut model, 1, movement());
        apply(&mut model, 2, Action::hold());
        let final_outcome = apply(&mut model, 3, Action::hold());
        assert_eq!(final_outcome.realized_action, movement());
        assert!(final_outcome.pending_commands_at_termination.is_empty());
        assert!(model.pending_commands().is_empty());
    }

    #[test]
    fn rejects_unknown_fields_bad_versions_and_invalid_windows() {
        assert!(ActuatorConfig::from_json(&" ".repeat(MAX_CONFIG_BYTES + 1)).is_err());
        assert!(
            ActuatorConfig::from_json(
                r#"{"schema_version":1,"seed":1,"faults":[],"arbitrary":true}"#
            )
            .is_err()
        );
        assert!(ActuatorConfig::from_json(r#"{"schema_version":2,"seed":1,"faults":[]}"#).is_err());
        assert!(ActuatorConfig::from_json(
            r#"{"schema_version":1,"seed":1,"faults":[{"kind":"lost_movement","id":"x","windows":[{"start_tick":0,"end_tick":1}],"probability_permille":1000}]}"#
        )
        .is_err());
        assert!(ActuatorConfig::from_json(
            r#"{"schema_version":1,"seed":1,"faults":[{"kind":"lost_movement","id":"x","windows":[{"start_tick":1,"end_tick":1}],"probability_permille":1000,"expression":"true"}]}"#
        )
        .is_err());
        assert!(ActuatorConfig::from_json(
            r#"{"schema_version":1,"seed":1,"faults":[{"kind":"lost_movement","id":"x","windows":[{"start_tick":1,"end_tick":1}]}]}"#
        )
        .is_err());
        assert!(ActuatorConfig::from_json(
            r#"{"schema_version":1,"seed":1,"faults":[{"kind":"lost_movement","id":"x","windows":[{"start_tick":1,"end_tick":1}],"probability_permille":1001}]}"#
        )
        .is_err());
        assert!(ActuatorConfig::from_json(
            r#"{"schema_version":1,"seed":1,"faults":[{"kind":"lost_movement","id":"x","windows":[{"start_tick":1,"end_tick":1}],"probability_permille":1000}]}"#
        )
        .is_ok());
    }

    #[test]
    fn validates_fault_windows_against_mission_length_and_rejects_duplicate_ids() {
        assert!(config(vec![lost("loss", 1, 4, 1000)]).validate(3).is_err());
        assert!(
            config(vec![lost("same", 1, 1, 1000), lost("same", 2, 2, 1000)])
                .validate(2)
                .is_err()
        );
        assert!(ActuatorConfig::default().validate(0).is_err());
    }

    #[test]
    #[should_panic(expected = "actuator ticks must be applied sequentially from 1")]
    fn rejects_out_of_order_tick_application() {
        let mut model = ActuatorModel::new(ActuatorConfig::default(), 1, 2).unwrap();
        let _ = model.apply(2, Action::hold());
    }
}
