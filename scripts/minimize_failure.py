"""Interface de linha de comandos para reduzir cenários que reproduzem falhas."""

from __future__ import annotations

import argparse
import copy
import json
import math
import os
import re
import sys
import time
from pathlib import Path
from typing import Any

from process_limits import ProcessLimitError, ensure_tree_size
from failure_reduction import (
    MAX_CANDIDATE_LIMIT,
    MAX_OUTPUT_LIMIT_BYTES,
    MAX_SCENARIO_BYTES,
    MAX_TIME_LIMIT_SECONDS,
    MAX_TEMP_LIMIT_BYTES,
    INVARIANTS,
    MISSION_FAILURES,
    BudgetExpired,
    CandidateRejected,
    Evaluation,
    MinimizationError,
    ROOT,
    SearchState,
    compact_json,
    complexity_metrics,
    evaluate_scenario,
    propose_horizons,
    propose_parameter_reductions,
    propose_window_removals,
    propose_window_shortenings,
    run_phase,
    select_property,
    target_signature,
)

def parse_seed(value: str) -> int:
    if not re.fullmatch(r"[0-9]+", value) or len(value) > 20:
        raise argparse.ArgumentTypeError("a semente tem de ser um inteiro entre 0 e 2^64-1")
    parsed = int(value)
    if parsed > 2**64 - 1:
        raise argparse.ArgumentTypeError("a semente excede o limite de 64 bits")
    return parsed


def parse_positive_int(value: str) -> int:
    if not re.fullmatch(r"[0-9]+", value) or len(value) > 10:
        raise argparse.ArgumentTypeError("indique um inteiro positivo")
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("indique um inteiro positivo")
    return parsed


def parse_finite_positive_float(value: str) -> float:
    try:
        parsed = float(value)
    except (OverflowError, ValueError) as error:
        raise argparse.ArgumentTypeError("indique um limite de tempo finito e positivo") from error
    if not math.isfinite(parsed) or parsed <= 0 or parsed > MAX_TIME_LIMIT_SECONDS:
        raise argparse.ArgumentTypeError(
            f"o limite de tempo tem de ser finito, positivo e não superior a {MAX_TIME_LIMIT_SECONDS:g} segundos"
        )
    return parsed


def parse_property(value: str) -> str:
    kind, separator, name = value.partition(":")
    if not separator or not name:
        raise argparse.ArgumentTypeError("use invariant:<nome> ou mission:<resultado>")
    if kind == "invariant" and name in INVARIANTS:
        return value
    if kind == "mission" and name in MISSION_FAILURES:
        return value
    raise argparse.ArgumentTypeError("propriedade desconhecida ou que não representa uma falha")


def resolve_binary(value: str | None) -> Path:
    requested = value or os.environ.get("VV_LAB_BINARY")
    if requested:
        binary = Path(requested).expanduser().resolve()
    else:
        names = ("vv-lab.exe", "vv-lab") if os.name == "nt" else ("vv-lab", "vv-lab.exe")
        binary = next(
            (ROOT / "target" / configuration / name for configuration in ("release", "debug") for name in names if (ROOT / "target" / configuration / name).is_file()),
            ROOT / "target" / "debug" / names[0],
        )
    if not binary.is_file():
        raise MinimizationError(
            "não encontrei o executável vv-lab; indique-o com --binary ou VV_LAB_BINARY"
        )
    return binary


def load_scenario(path: Path) -> tuple[dict[str, Any], bytes]:
    try:
        with path.open("rb") as source:
            raw = source.read(MAX_SCENARIO_BYTES + 1)
    except OSError as error:
        raise MinimizationError(f"não foi possível ler o cenário: {error}") from error
    if len(raw) > MAX_SCENARIO_BYTES:
        raise MinimizationError("o cenário excede o limite de 2 MiB")
    try:
        scenario = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise MinimizationError(f"o cenário não contém JSON válido: {error}") from error
    if not isinstance(scenario, dict):
        raise MinimizationError("o cenário tem de ser um objeto JSON")
    try:
        gps = scenario["sensors"]["gps"]
        communication = scenario["sensors"]["communication"]
        if not isinstance(gps, dict) or not isinstance(communication, dict):
            raise TypeError
        if not isinstance(gps["dropout_windows"], list):
            raise TypeError
        if not isinstance(communication["packet_loss_windows"], list):
            raise TypeError
        for field in ("noise_max_mm", "dropout_permille"):
            if type(gps[field]) is not int:
                raise TypeError
        for field in ("packet_loss_permille", "delay_min_ticks", "delay_max_ticks"):
            if type(communication[field]) is not int:
                raise TypeError
        if type(scenario["steps"]) is not int:
            raise TypeError
    except (KeyError, TypeError) as error:
        raise MinimizationError("o cenário não respeita os campos de falhas do contrato M2") from error
    return scenario, raw


def make_summary(
    *,
    state: SearchState,
    property_name: str,
    original_scenario: dict[str, Any],
    original_evaluation: Evaluation,
    seed: int,
    stall_window_ticks: int,
    min_progress_mm: int,
    candidate_limit: int,
    elapsed_seconds: float,
    time_budget_seconds: float,
    max_output_bytes: int,
    max_temp_bytes: int,
) -> dict[str, Any]:
    stop_status = state.stop_status
    status = stop_status or ("reduced" if state.accepted else "no_reduction")
    original_complexity = complexity_metrics(original_scenario)
    final_complexity = complexity_metrics(state.scenario)
    ratio = 0.0 if original_complexity["score"] == 0 else 1 - final_complexity["score"] / original_complexity["score"]
    return {
        "schema_version": 1,
        "status": status,
        "property": property_name,
        "seed": seed,
        "policy": {"stall_window_ticks": stall_window_ticks, "min_progress_mm": min_progress_mm},
        "candidate_limit": candidate_limit,
        "time_budget_seconds": time_budget_seconds,
        "elapsed_seconds": round(elapsed_seconds, 6),
        "max_output_bytes": max_output_bytes,
        "max_temp_bytes": max_temp_bytes,
        "candidate_counts": {
            "attempted": state.attempted,
            "validated": state.attempted - state.rejected_invalid,
            "accepted": state.accepted,
            "rejected_invalid_or_unreproducible": state.rejected_invalid,
            "rejected_property_mismatch": state.rejected_property,
        },
        "original_signature": target_signature(property_name, original_evaluation),
        "final_signature": target_signature(property_name, state.evaluation),
        "complexity": {
            "original": original_complexity,
            "final": final_complexity,
            "reduction_ratio": ratio,
        },
        "reduction_steps": state.steps,
        "reproducible_commands_working_directory": "minimization output directory",
        "reproducible_commands": [
            f"vv-lab run scenarios/original.json --seed {seed} --output reproduced-original --stall-window-ticks {stall_window_ticks} --min-progress-mm {min_progress_mm} --max-output-bytes {max_output_bytes}",
            "vv-lab replay reproduced-original/events.json",
            f"vv-lab analyze reproduced-original/events.json --output reproduced-original/mission-replayed.json --stall-window-ticks {stall_window_ticks} --min-progress-mm {min_progress_mm}",
            f"vv-lab run scenarios/minimized.json --seed {seed} --output reproduced-minimized --stall-window-ticks {stall_window_ticks} --min-progress-mm {min_progress_mm} --max-output-bytes {max_output_bytes}",
            "vv-lab replay reproduced-minimized/events.json",
            f"vv-lab analyze reproduced-minimized/events.json --output reproduced-minimized/mission-replayed.json --stall-window-ticks {stall_window_ticks} --min-progress-mm {min_progress_mm}",
        ],
    }


def summary_markdown(summary: dict[str, Any]) -> bytes:
    complexity = summary["complexity"]
    counts = summary["candidate_counts"]
    lines = [
        "# Resultado da minimização",
        "",
        f"Estado: `{summary['status']}`.",
        f"Propriedade preservada: `{summary['property']}` com a semente `{summary['seed']}`.",
        f"Candidatos: {counts['attempted']} avaliados, {counts['accepted']} aceites, {counts['rejected_property_mismatch']} rejeitados por alteração da propriedade e {counts['rejected_invalid_or_unreproducible']} inválidos ou sem reprodução confirmada.",
        f"Complexidade: {complexity['original']['score']} antes e {complexity['final']['score']} depois; redução de {complexity['reduction_ratio']:.1%}.",
        f"Tempo decorrido: {summary['elapsed_seconds']:.3f} s, com limite de {summary['time_budget_seconds']:g} s.",
        "",
        "## Assinatura da falha",
        "",
        "```json",
        json.dumps(summary["final_signature"], ensure_ascii=False, indent=2, sort_keys=True),
        "```",
        "",
        "## Comandos de reprodução",
        "",
        "Execute os comandos seguintes a partir desta pasta de resultados, com `vv-lab` disponível no PATH:",
        "",
    ]
    lines.extend(f"- `{command}`" for command in summary["reproducible_commands"])
    if summary["reduction_steps"]:
        lines.extend(["", "## Reduções aceites", ""])
        lines.extend(
            f"{step['number']}. {step['change']}: `{step['complexity_before']['score']}` para `{step['complexity_after']['score']}`."
            for step in summary["reduction_steps"]
        )
    lines.append("")
    return "\n".join(lines).encode("utf-8")


def prepare_output_directory(output: Path) -> Path:
    resolved = output.expanduser().resolve()
    if resolved.exists() and any(resolved.iterdir()):
        raise MinimizationError("a pasta de saída tem de estar vazia")
    return resolved


def write_results(
    output: Path,
    *,
    original_input: bytes,
    minimized_input: bytes,
    original: Evaluation,
    final: Evaluation,
    summary: dict[str, Any],
    max_output_bytes: int,
) -> None:
    summary_json = (json.dumps(summary, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode("utf-8")
    summary_md = summary_markdown(summary)
    planned = (
        len(original_input)
        + len(minimized_input)
        + len(summary_json)
        + len(summary_md)
        + sum(len(value) for value in original.files.values())
        + sum(len(value) for value in final.files.values())
    )
    if planned > max_output_bytes:
        raise MinimizationError(
            f"os artefactos retidos somariam {planned} bytes, acima do limite de {max_output_bytes} bytes"
        )

    output.mkdir(parents=True, exist_ok=True)
    for label, evaluation in (("original", original), ("final", final)):
        folder = output / label
        folder.mkdir()
        for name, contents in evaluation.files.items():
            (folder / name).write_bytes(contents)
    scenarios = output / "scenarios"
    scenarios.mkdir()
    (scenarios / "original.json").write_bytes(original_input)
    (scenarios / "minimized.json").write_bytes(minimized_input)
    (output / "summary.json").write_bytes(summary_json)
    (output / "summary.md").write_bytes(summary_md)
    actual = ensure_tree_size(output, max_output_bytes, "artefactos retidos")
    if actual != planned:
        raise MinimizationError("o tamanho retido diverge da pré-contagem dos artefactos")
    for label, evaluation in (("original", original), ("final", final)):
        for name, expected in evaluation.files.items():
            if (output / label / name).read_bytes() != expected:
                raise MinimizationError(f"o artefacto retido {label}/{name} diverge da execução verificada")


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        description="Reduz um cenário mantendo a mesma falha e a mesma semente."
    )
    result.add_argument("scenario", type=Path, help="cenário JSON de entrada")
    result.add_argument("--seed", required=True, type=parse_seed, help="semente fixa entre 0 e 2^64-1")
    result.add_argument("--output", required=True, type=Path, help="pasta de saída vazia")
    result.add_argument("--binary", help="caminho do executável vv-lab")
    result.add_argument("--max-candidates", type=parse_positive_int, default=80)
    result.add_argument("--time-budget-seconds", type=parse_finite_positive_float, default=30.0)
    result.add_argument("--max-output-bytes", type=parse_positive_int, default=16 * 1024 * 1024)
    result.add_argument("--max-temp-bytes", type=parse_positive_int, default=32 * 1024 * 1024)
    result.add_argument("--property", type=parse_property, help="invariant:<nome> ou mission:<resultado>")
    result.add_argument("--stall-window-ticks", type=parse_positive_int, default=8)
    result.add_argument("--min-progress-mm", type=parse_positive_int, default=1)
    return result


def execute(arguments: argparse.Namespace) -> int:
    if arguments.max_candidates > MAX_CANDIDATE_LIMIT:
        raise MinimizationError(f"--max-candidates não pode exceder {MAX_CANDIDATE_LIMIT}")
    if arguments.max_output_bytes > MAX_OUTPUT_LIMIT_BYTES:
        raise MinimizationError(f"--max-output-bytes não pode exceder {MAX_OUTPUT_LIMIT_BYTES}")
    if arguments.max_temp_bytes > MAX_TEMP_LIMIT_BYTES:
        raise MinimizationError(f"--max-temp-bytes não pode exceder {MAX_TEMP_LIMIT_BYTES}")
    if arguments.stall_window_ticks > 10_000:
        raise MinimizationError("--stall-window-ticks não pode exceder 10000")
    if arguments.min_progress_mm > 40_000_000:
        raise MinimizationError("--min-progress-mm não pode exceder 40000000")
    started = time.monotonic()
    binary = resolve_binary(arguments.binary)
    original_scenario, original_input = load_scenario(arguments.scenario)
    output = prepare_output_directory(arguments.output)
    deadline = started + arguments.time_budget_seconds

    original_evaluation = evaluate_scenario(
        original_scenario,
        binary=binary,
        seed=arguments.seed,
        stall_window_ticks=arguments.stall_window_ticks,
        min_progress_mm=arguments.min_progress_mm,
        max_output_bytes=arguments.max_output_bytes,
        max_temp_bytes=arguments.max_temp_bytes,
        deadline=deadline,
    )
    property_name = select_property(
        original_evaluation.report, original_evaluation.mission, arguments.property
    )
    original_signature = target_signature(property_name, original_evaluation)
    state = SearchState(copy.deepcopy(original_scenario), original_evaluation)

    common = {
        "state": state,
        "property_name": property_name,
        "original_signature": original_signature,
        "binary": binary,
        "seed": arguments.seed,
        "stall_window_ticks": arguments.stall_window_ticks,
        "min_progress_mm": arguments.min_progress_mm,
        "max_output_bytes": arguments.max_output_bytes,
        "max_temp_bytes": arguments.max_temp_bytes,
        "max_candidates": arguments.max_candidates,
        "deadline": deadline,
    }
    run_phase(lambda current, _evaluation: propose_window_removals(current), **common)
    if state.stop_status is None:
        run_phase(lambda current, _evaluation: propose_window_shortenings(current), **common)
    if state.stop_status is None:
        run_phase(lambda current, _evaluation: propose_parameter_reductions(current), **common)
    if state.stop_status is None:
        run_phase(
            lambda current, evaluation: propose_horizons(current, property_name, evaluation),
            **common,
        )

    summary = make_summary(
        state=state,
        property_name=property_name,
        original_scenario=original_scenario,
        original_evaluation=original_evaluation,
        seed=arguments.seed,
        stall_window_ticks=arguments.stall_window_ticks,
        min_progress_mm=arguments.min_progress_mm,
        candidate_limit=arguments.max_candidates,
        elapsed_seconds=time.monotonic() - started,
        time_budget_seconds=arguments.time_budget_seconds,
        max_output_bytes=arguments.max_output_bytes,
        max_temp_bytes=arguments.max_temp_bytes,
    )
    write_results(
        output,
        original_input=original_input,
        minimized_input=compact_json(state.scenario),
        original=original_evaluation,
        final=state.evaluation,
        summary=summary,
        max_output_bytes=arguments.max_output_bytes,
    )
    print(
        f"Minimização: {summary['status']}; propriedade {property_name}; "
        f"{state.accepted} reduções aceites em {state.attempted} candidatos."
    )
    return 0


def main(argv: list[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    try:
        return execute(arguments)
    except (MinimizationError, CandidateRejected, ProcessLimitError, OSError) as error:
        print(f"erro: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
