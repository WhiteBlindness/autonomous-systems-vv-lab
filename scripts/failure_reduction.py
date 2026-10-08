"""Lógica limitada de avaliação e redução de falhas determinísticas."""

from __future__ import annotations

import copy
import json
import math
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Iterable

from process_limits import ProcessLimitError, ensure_tree_size, run_bounded_process
from failure_signature import (
    INVARIANTS,
    MISSION_FAILURES,
    condition_category,
    events_after_sample,
    failed_invariant_names,
    find_event,
    get_mission_property,
    hold_reason_class,
    invariant_failure_signature,
    point_in_or_on_polygon,
    point_on_segment,
    point_relation,
    same_property,
    sample_mechanisms,
    stall_window_mechanisms,
    target_signature,
)


ROOT = Path(__file__).resolve().parents[1]
ARTIFACT_NAMES = ("events.json", "report.json", "mission.json")
MAX_SCENARIO_BYTES = 2 * 1024 * 1024
MAX_ANALYSIS_BYTES = 1024 * 1024
MAX_PROCESS_OUTPUT_BYTES = 64 * 1024
MAX_OUTPUT_LIMIT_BYTES = 128 * 1024 * 1024
MAX_TEMP_LIMIT_BYTES = 256 * 1024 * 1024
MAX_CANDIDATE_LIMIT = 10_000
MAX_TIME_LIMIT_SECONDS = 3600.0


class MinimizationError(RuntimeError):
    """Erro que impede uma minimização fiável."""


class BudgetExpired(MinimizationError):
    """O limite de tempo terminou durante a avaliação de um candidato."""


class CandidateRejected(MinimizationError):
    """Um candidato não produziu artefactos válidos e reproduzíveis."""


@dataclass
class Evaluation:
    """Artefactos e resultados verificados de uma execução."""

    artifact: dict[str, Any]
    report: dict[str, Any]
    mission: dict[str, Any]
    files: dict[str, bytes]


@dataclass
class SearchState:
    """Estado limitado da pesquisa determinística."""

    scenario: dict[str, Any]
    evaluation: Evaluation
    attempted: int = 0
    accepted: int = 0
    rejected_invalid: int = 0
    rejected_property: int = 0
    steps: list[dict[str, Any]] | None = None
    stop_status: str | None = None
    seen: set[bytes] | None = None

    def __post_init__(self) -> None:
        self.steps = [] if self.steps is None else self.steps
        self.seen = set() if self.seen is None else self.seen


def compact_json(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, indent=2, separators=(",", ": ")).encode("utf-8") + b"\n"


def process_command(
    command: list[str], *, deadline: float, description: str
) -> Any:
    remaining = deadline - time.monotonic()
    if not math.isfinite(remaining) or remaining <= 0:
        raise BudgetExpired("o limite global de tempo terminou")
    try:
        return run_bounded_process(
            command,
            cwd=ROOT,
            timeout_seconds=remaining,
            max_output_bytes=MAX_PROCESS_OUTPUT_BYTES,
            description=description,
        )
    except ProcessLimitError as error:
        message = str(error)
        if "timeout" in message or "time budget" in message or deadline - time.monotonic() <= 0:
            raise BudgetExpired("o limite global de tempo terminou durante a avaliação") from error
        raise CandidateRejected(message) from error


def evaluate_scenario(
    scenario: dict[str, Any],
    *,
    binary: Path,
    seed: int,
    stall_window_ticks: int,
    min_progress_mm: int,
    max_output_bytes: int,
    max_temp_bytes: int,
    deadline: float,
) -> Evaluation:
    scenario_bytes = compact_json(scenario)
    if len(scenario_bytes) > MAX_SCENARIO_BYTES:
        raise CandidateRejected("o cenário candidato excede o limite de 2 MiB")
    run_limit = min(max_output_bytes, max_temp_bytes - len(scenario_bytes) - MAX_ANALYSIS_BYTES)
    if run_limit <= 0:
        raise CandidateRejected("o limite temporário não deixa espaço para executar e analisar o cenário")

    with tempfile.TemporaryDirectory(prefix="vv-lab-minimize-", dir=ROOT) as temporary:
        workspace = Path(temporary)
        scenario_path = workspace / "scenario.json"
        output_path = workspace / "run"
        analysis_path = workspace / "analysis.json"
        scenario_path.write_bytes(scenario_bytes)
        command = [
            str(binary),
            "run",
            str(scenario_path),
            "--seed",
            str(seed),
            "--output",
            str(output_path),
            "--stall-window-ticks",
            str(stall_window_ticks),
            "--min-progress-mm",
            str(min_progress_mm),
            "--max-output-bytes",
            str(run_limit),
        ]
        run = process_command(command, deadline=deadline, description="execução do cenário")
        if run.returncode not in (0, 2):
            raise CandidateRejected("a simulação rejeitou a configuração ou o limite de artefactos")
        try:
            artifact_bytes = {name: (output_path / name).read_bytes() for name in ARTIFACT_NAMES}
            if sum(map(len, artifact_bytes.values())) > run_limit:
                raise CandidateRejected("os artefactos excederam o limite de saída da simulação")
            artifact = json.loads(artifact_bytes["events.json"])
            report = json.loads(artifact_bytes["report.json"])
            mission = json.loads(artifact_bytes["mission.json"])
        except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
            raise CandidateRejected(f"a simulação não produziu artefactos JSON válidos: {error}") from error

        failed_names = sorted(
            item["name"] for item in report.get("invariants", []) if item.get("passed") is False
        )
        expected_code = 2 if failed_names else 0
        if run.returncode != expected_code or artifact.get("expected_report") != report:
            raise CandidateRejected("o código de saída e os relatórios da simulação não coincidem")
        ensure_tree_size(workspace, max_temp_bytes, "artefactos temporários")

        replay = process_command(
            [str(binary), "replay", str(output_path / "events.json")],
            deadline=deadline,
            description="reprodução dos eventos",
        )
        if replay.returncode != 0:
            raise CandidateRejected("a reprodução dos eventos falhou")

        analyze = process_command(
            [
                str(binary),
                "analyze",
                str(output_path / "events.json"),
                "--output",
                str(analysis_path),
                "--stall-window-ticks",
                str(stall_window_ticks),
                "--min-progress-mm",
                str(min_progress_mm),
                "--max-output-bytes",
                str(MAX_ANALYSIS_BYTES),
            ],
            deadline=deadline,
            description="análise do progresso",
        )
        if analyze.returncode != 0:
            raise CandidateRejected("a análise do progresso falhou")
        try:
            recomputed_mission = analysis_path.read_bytes()
        except OSError as error:
            raise CandidateRejected(f"a análise não criou mission.json: {error}") from error
        if recomputed_mission != artifact_bytes["mission.json"]:
            raise CandidateRejected("mission.json não coincide byte a byte com a análise reproduzida")
        ensure_tree_size(workspace, max_temp_bytes, "artefactos temporários")
        return Evaluation(artifact, report, mission, artifact_bytes)



def select_property(report: dict[str, Any], mission: dict[str, Any], requested: str | None) -> str:
    if requested:
        kind, name = requested.split(":", 1)
        if kind == "invariant":
            result = next((item for item in report.get("invariants", []) if item.get("name") == name), None)
            if result is None or result.get("passed") is not False or not result.get("failures"):
                raise MinimizationError(f"a propriedade {requested} não falha no cenário original")
        elif mission.get("outcome") != name:
            raise MinimizationError(f"a propriedade {requested} não ocorre no cenário original")
        return requested

    for item in report.get("invariants", []):
        if item.get("passed") is False and item.get("failures"):
            return f"invariant:{item['name']}"
    mission_property = get_mission_property(mission)
    if mission_property:
        return mission_property
    raise MinimizationError("o cenário original não apresenta uma falha minimizável")



def complexity_metrics(scenario: dict[str, Any]) -> dict[str, int]:
    gps = scenario["sensors"]["gps"]
    communication = scenario["sensors"]["communication"]
    windows = [*gps["dropout_windows"], *communication["packet_loss_windows"]]
    window_ticks = sum(window["end_tick"] - window["start_tick"] + 1 for window in windows)
    score = (
        scenario["steps"]
        + len(windows) * 100
        + window_ticks * 10
        + gps["noise_max_mm"]
        + gps["dropout_permille"] * 100
        + communication["packet_loss_permille"] * 100
        + communication["delay_min_ticks"] * 10
        + communication["delay_max_ticks"] * 10
    )
    return {
        "score": score,
        "steps": scenario["steps"],
        "fault_window_count": len(windows),
        "fault_window_ticks": window_ticks,
        "gps_noise_max_mm": gps["noise_max_mm"],
        "gps_dropout_permille": gps["dropout_permille"],
        "packet_loss_permille": communication["packet_loss_permille"],
        "delay_min_ticks": communication["delay_min_ticks"],
        "delay_max_ticks": communication["delay_max_ticks"],
    }


def changed_fields(original: dict[str, Any], candidate: dict[str, Any]) -> list[str]:
    changes: list[str] = []
    if original["steps"] != candidate["steps"]:
        changes.append("steps")
    for group, field in (
        ("gps", "dropout_windows"),
        ("communication", "packet_loss_windows"),
    ):
        if original["sensors"][group][field] != candidate["sensors"][group][field]:
            changes.append(f"sensors.{group}.{field}")
    for group, fields in (
        ("gps", ("noise_max_mm", "dropout_permille")),
        ("communication", ("packet_loss_permille", "delay_min_ticks", "delay_max_ticks")),
    ):
        for field in fields:
            if original["sensors"][group][field] != candidate["sensors"][group][field]:
                changes.append(f"sensors.{group}.{field}")
    return changes


def canonical_key(scenario: dict[str, Any]) -> bytes:
    return json.dumps(scenario, sort_keys=True, separators=(",", ":")).encode("utf-8")


def propose_window_removals(scenario: dict[str, Any]) -> Iterable[tuple[dict[str, Any], str]]:
    for group, field in (("gps", "dropout_windows"), ("communication", "packet_loss_windows")):
        windows = scenario["sensors"][group][field]
        for index in range(len(windows)):
            candidate = copy.deepcopy(scenario)
            del candidate["sensors"][group][field][index]
            yield candidate, f"remove {group} window {index}"


def propose_window_shortenings(scenario: dict[str, Any]) -> Iterable[tuple[dict[str, Any], str]]:
    for group, field in (("gps", "dropout_windows"), ("communication", "packet_loss_windows")):
        windows = scenario["sensors"][group][field]
        for index, window in enumerate(windows):
            start = window["start_tick"]
            end = window["end_tick"]
            span = end - start + 1
            if span <= 1:
                continue
            start_targets = sorted({start + 1, start + max(1, span // 2), end})
            for target in start_targets:
                if target <= end:
                    candidate = copy.deepcopy(scenario)
                    candidate["sensors"][group][field][index]["start_tick"] = target
                    yield candidate, f"shorten {group} window {index} start"
            end_targets = sorted({start, end - 1, start + (span - 1) // 2})
            for target in end_targets:
                if target >= start and target < end:
                    candidate = copy.deepcopy(scenario)
                    candidate["sensors"][group][field][index]["end_tick"] = target
                    yield candidate, f"shorten {group} window {index} end"


def reduce_value_targets(value: int) -> list[int]:
    targets = {0}
    current = value
    while current > 1:
        current //= 2
        targets.add(current)
    return sorted((target for target in targets if target < value), reverse=True)


def propose_parameter_reductions(scenario: dict[str, Any]) -> Iterable[tuple[dict[str, Any], str]]:
    definitions = (
        ("gps", "noise_max_mm"),
        ("gps", "dropout_permille"),
        ("communication", "packet_loss_permille"),
        ("communication", "delay_min_ticks"),
        ("communication", "delay_max_ticks"),
    )
    for group, field in definitions:
        value = scenario["sensors"][group][field]
        for target in reduce_value_targets(value):
            candidate = copy.deepcopy(scenario)
            candidate["sensors"][group][field] = target
            if group == "communication" and target > candidate["sensors"][group]["delay_max_ticks"]:
                continue
            yield candidate, f"reduce sensors.{group}.{field}"


def clip_windows(scenario: dict[str, Any], steps: int) -> None:
    for group, field in (("gps", "dropout_windows"), ("communication", "packet_loss_windows")):
        clipped = []
        for window in scenario["sensors"][group][field]:
            if window["start_tick"] <= steps:
                clipped.append({"start_tick": window["start_tick"], "end_tick": min(window["end_tick"], steps)})
        scenario["sensors"][group][field] = clipped


def failure_tick(property_name: str, evaluation: Evaluation) -> int:
    if property_name.startswith("invariant:"):
        invariant_name = property_name.split(":", 1)[1]
        result = next(item for item in evaluation.report["invariants"] if item["name"] == invariant_name)
        return min(evidence["tick"] for evidence in result["failures"])
    mission = evaluation.mission
    return mission.get("stall_detected_tick") or mission.get("completion_tick") or evaluation.report["ticks"]


def propose_horizons(
    scenario: dict[str, Any], property_name: str, evaluation: Evaluation
) -> Iterable[tuple[dict[str, Any], str]]:
    # Um horizonte menor altera por si só o significado de uma missão incompleta.
    if property_name == "mission:incomplete":
        return
    current = scenario["steps"]
    if current <= 1:
        return
    targets = {1, max(1, current // 2), current - 1, min(current - 1, failure_tick(property_name, evaluation))}
    for target in sorted(value for value in targets if 0 < value < current):
        candidate = copy.deepcopy(scenario)
        candidate["steps"] = target
        clip_windows(candidate, target)
        yield candidate, f"reduce horizon to {target} ticks"


def candidate_status(state: SearchState, maximum: int, deadline: float) -> str | None:
    if state.attempted >= maximum:
        return "candidate_limit_reached"
    if deadline - time.monotonic() <= 0:
        return "time_budget_reached"
    return None


def try_candidate(
    candidate: dict[str, Any],
    description: str,
    *,
    state: SearchState,
    property_name: str,
    original_signature: dict[str, Any],
    binary: Path,
    seed: int,
    stall_window_ticks: int,
    min_progress_mm: int,
    max_output_bytes: int,
    max_temp_bytes: int,
    max_candidates: int,
    deadline: float,
) -> bool:
    stop = candidate_status(state, max_candidates, deadline)
    if stop:
        state.stop_status = stop
        return False
    key = canonical_key(candidate)
    assert state.seen is not None and state.steps is not None
    if key in state.seen:
        return False
    state.seen.add(key)
    before = complexity_metrics(state.scenario)
    after = complexity_metrics(candidate)
    if after["score"] >= before["score"]:
        return False

    state.attempted += 1
    try:
        evaluation = evaluate_scenario(
            candidate,
            binary=binary,
            seed=seed,
            stall_window_ticks=stall_window_ticks,
            min_progress_mm=min_progress_mm,
            max_output_bytes=max_output_bytes,
            max_temp_bytes=max_temp_bytes,
            deadline=deadline,
        )
    except BudgetExpired:
        state.rejected_invalid += 1
        state.stop_status = "time_budget_reached"
        return False
    except CandidateRejected:
        state.rejected_invalid += 1
        return False

    if not same_property(property_name, original_signature, evaluation):
        state.rejected_property += 1
        return False

    state.steps.append(
        {
            "number": state.accepted + 1,
            "change": description,
            "changed_fields": changed_fields(state.scenario, candidate),
            "complexity_before": before,
            "complexity_after": after,
            "property_signature": target_signature(property_name, evaluation),
        }
    )
    state.scenario = candidate
    state.evaluation = evaluation
    state.accepted += 1
    return True


def run_phase(
    proposal_factory: Callable[[dict[str, Any], Evaluation], Iterable[tuple[dict[str, Any], str]]],
    *,
    state: SearchState,
    property_name: str,
    original_signature: dict[str, Any],
    binary: Path,
    seed: int,
    stall_window_ticks: int,
    min_progress_mm: int,
    max_output_bytes: int,
    max_temp_bytes: int,
    max_candidates: int,
    deadline: float,
) -> None:
    while state.stop_status is None:
        accepted = False
        proposals = proposal_factory(state.scenario, state.evaluation)
        for candidate, description in proposals:
            accepted = try_candidate(
                candidate,
                description,
                state=state,
                property_name=property_name,
                original_signature=original_signature,
                binary=binary,
                seed=seed,
                stall_window_ticks=stall_window_ticks,
                min_progress_mm=min_progress_mm,
                max_output_bytes=max_output_bytes,
                max_temp_bytes=max_temp_bytes,
                max_candidates=max_candidates,
                deadline=deadline,
            )
            if state.stop_status or accepted:
                break
        if not accepted:
            return
