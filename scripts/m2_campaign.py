"""Gestão de expectativas e validação da análise das missões M2."""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
M2_ROOT = ROOT / "scenarios" / "m2"
M2_INVARIANT_NAMES = (
    "restricted_zone",
    "world_bounds",
    "safe_fallback",
    "valid_state_transitions",
)
MISSION_FIELDS = {
    "schema_version",
    "source_final_hash",
    "policy",
    "outcome",
    "reason",
    "waypoint_count",
    "controller_waypoint_index",
    "waypoints_completed",
    "waypoints_remaining",
    "last_meaningful_progress_tick",
    "ticks_since_meaningful_progress",
    "stall_detected_tick",
    "fallback_ticks",
    "hold_reason_counts",
    "completion_tick",
    "safety_status",
    "distance_to_next_waypoint_mm",
    "best_distance_to_next_waypoint_mm",
}
VALID_OUTCOMES = {"completed", "incomplete", "stalled", "invalid_terminated"}
ALLOWED_OVERRIDES = {
    "sensors.gps.dropout_permille",
    "sensors.gps.noise_max_mm",
    "sensors.communication.packet_loss_permille",
    "sensors.communication.delay_min_ticks",
    "sensors.communication.delay_max_ticks",
}


class M2CampaignError(RuntimeError):
    """Erro de configuração que impede validar a campanha M2."""


def load_m2_manifest(path: Path | None = None) -> dict[str, Any]:
    manifest_path = path or M2_ROOT / "expectations.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise M2CampaignError(f"não foi possível ler as expectativas M2: {error}") from error
    if not isinstance(manifest, dict) or manifest.get("schema_version") != 1:
        raise M2CampaignError("versão inválida do manifesto de expectativas M2")
    entries = manifest.get("scenarios")
    if not isinstance(entries, list) or not entries:
        raise M2CampaignError("o manifesto M2 não contém cenários")
    names: set[str] = set()
    for entry in entries:
        if not isinstance(entry, dict):
            raise M2CampaignError("cada expectativa M2 tem de ser um objeto")
        name = entry.get("name")
        filename = entry.get("file")
        if not isinstance(name, str) or name in names:
            raise M2CampaignError("nomes de cenário M2 ausentes ou repetidos")
        if not isinstance(filename, str) or Path(filename).name != filename:
            raise M2CampaignError(f"ficheiro M2 inválido para {name}")
        names.add(name)
        if entry.get("expected_status") not in {"passed", "invariant_failed"}:
            raise M2CampaignError(f"estado esperado inválido para {name}")
        expected_invariants = entry.get("expected_invariants")
        if (
            not isinstance(expected_invariants, dict)
            or set(expected_invariants) != set(M2_INVARIANT_NAMES)
            or any(type(value) is not bool for value in expected_invariants.values())
        ):
            raise M2CampaignError(f"expectativas dos invariantes inválidas para {name}")
        variants = entry.get("variants")
        if not isinstance(variants, dict) or not variants:
            raise M2CampaignError(f"variantes ausentes para {name}")
        for variant_name, variant in variants.items():
            if not isinstance(variant_name, str) or not isinstance(variant, dict):
                raise M2CampaignError(f"variante inválida para {name}")
            policy = variant.get("policy")
            if not isinstance(policy, dict) or set(policy) != {
                "stall_window_ticks",
                "min_progress_mm",
            }:
                raise M2CampaignError(f"política inválida para {name}/{variant_name}")
            if any(type(value) is not int or value < 1 for value in policy.values()):
                raise M2CampaignError(f"limites da política inválidos para {name}/{variant_name}")
            outcomes = variant.get("expected_outcomes")
            if not isinstance(outcomes, dict) or not outcomes:
                raise M2CampaignError(f"resultados esperados ausentes para {name}/{variant_name}")
            if any(value not in VALID_OUTCOMES for value in outcomes.values()):
                raise M2CampaignError(f"resultado esperado inválido para {name}/{variant_name}")
            overrides = variant.get("scenario_overrides", {})
            if not isinstance(overrides, dict) or not set(overrides) <= ALLOWED_OVERRIDES:
                raise M2CampaignError(f"alterações de falhas inválidas para {name}/{variant_name}")
            if any(type(value) is not int or value < 0 for value in overrides.values()):
                raise M2CampaignError(f"valor de falha inválido para {name}/{variant_name}")
        scenario_path = M2_ROOT / filename
        if not scenario_path.is_file():
            raise M2CampaignError(f"cenário M2 em falta: {filename}")
        try:
            scenario = json.loads(scenario_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise M2CampaignError(f"não foi possível ler {filename}: {error}") from error
        if not isinstance(scenario, dict) or scenario.get("name") != f"m2-{name}":
            raise M2CampaignError(f"nome inválido no cenário {filename}")
        mutants = scenario.get("validation_mutants", [])
        if entry.get("requires_observation_discrepancy") and mutants:
            raise M2CampaignError(f"{name} não pode usar mutantes de validação")
        if entry.get("expected_primary_invariant") and not any(
            value is False for value in expected_invariants.values()
        ):
            raise M2CampaignError(f"{name} tem falha primária sem invariante esperado")
    return manifest


def fixture_checksums(manifest: dict[str, Any]) -> dict[str, str]:
    checksums: dict[str, str] = {}
    for entry in manifest["scenarios"]:
        path = M2_ROOT / entry["file"]
        contents = path.read_bytes().replace(b"\r\n", b"\n")
        checksums[f"m2/{entry['file']}"] = hashlib.sha256(contents).hexdigest()
    manifest_path = M2_ROOT / "expectations.json"
    contents = manifest_path.read_bytes().replace(b"\r\n", b"\n")
    checksums["m2/expectations.json"] = hashlib.sha256(contents).hexdigest()
    return checksums


def select_scenarios(
    manifest: dict[str, Any], scenario_names: tuple[str, ...] | None = None
) -> list[dict[str, Any]]:
    entries = manifest["scenarios"]
    if scenario_names is None:
        return entries
    by_name = {entry["name"]: entry for entry in entries}
    missing = [name for name in scenario_names if name not in by_name]
    if missing:
        raise M2CampaignError(f"variantes M2 desconhecidas: {', '.join(missing)}")
    return [by_name[name] for name in scenario_names]


def select_variant(
    entry: dict[str, Any], variant_name: str, seed: int
) -> tuple[dict[str, int], str, dict[str, int]]:
    variant = entry["variants"].get(variant_name)
    if not isinstance(variant, dict):
        raise M2CampaignError(f"variante desconhecida: {entry['name']}/{variant_name}")
    expected = variant.get("expected_outcomes", {}).get(str(seed))
    if expected not in VALID_OUTCOMES:
        raise M2CampaignError(
            f"não há resultado esperado explícito para {entry['name']}/{variant_name}, semente {seed}"
        )
    return variant["policy"], expected, variant.get("scenario_overrides", {})


def scenario_for_variant(entry: dict[str, Any], overrides: dict[str, int]) -> dict[str, Any]:
    try:
        scenario = json.loads((M2_ROOT / entry["file"]).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise M2CampaignError(f"não foi possível preparar {entry['file']}: {error}") from error
    for dotted_path, value in overrides.items():
        if dotted_path not in ALLOWED_OVERRIDES:
            raise M2CampaignError(f"alteração de falha não permitida: {dotted_path}")
        target = scenario
        segments = dotted_path.split(".")
        for segment in segments[:-1]:
            target = target[segment]
        target[segments[-1]] = value
    gps = scenario["sensors"]["gps"]
    communication = scenario["sensors"]["communication"]
    if gps["dropout_permille"] > 1000 or gps["noise_max_mm"] > 100000:
        raise M2CampaignError("a variante ultrapassa os limites do GPS")
    if communication["packet_loss_permille"] > 1000:
        raise M2CampaignError("a variante ultrapassa o limite de perda de pacotes")
    if (
        communication["delay_min_ticks"] > communication["delay_max_ticks"]
        or communication["delay_max_ticks"] > 10000
    ):
        raise M2CampaignError("a variante tem um intervalo de atraso inválido")
    return scenario


def matched_geometry_errors(
    entry: dict[str, Any], entries_by_name: dict[str, dict[str, Any]]
) -> list[str]:
    baseline_name = entry.get("matched_baseline")
    if baseline_name is None:
        return []
    baseline = entries_by_name.get(baseline_name)
    if baseline is None:
        return [f"cenário de referência inexistente: {baseline_name}"]
    try:
        left = json.loads((M2_ROOT / entry["file"]).read_text(encoding="utf-8"))
        right = json.loads((M2_ROOT / baseline["file"]).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return [f"não foi possível comparar a geometria de {entry['name']}: {error}"]
    left = copy.deepcopy(left)
    right = copy.deepcopy(right)
    left.pop("name", None)
    right.pop("name", None)
    for scenario in (left, right):
        try:
            scenario["sensors"]["gps"].pop("noise_max_mm")
        except (KeyError, AttributeError):
            return [f"configuração GPS incompleta em {entry['name']} ou {baseline_name}"]
    errors = []
    if left != right:
        errors.append(
            f"{entry['name']} e {baseline_name} não partilham a mesma geometria e missão"
        )
    return errors


def check_mission_sidecar(
    mission: dict[str, Any],
    artifact: dict[str, Any],
    report: dict[str, Any],
    expected_policy: dict[str, int],
    expected_outcome: str | None,
) -> list[str]:
    errors: list[str] = []
    if set(mission) != MISSION_FIELDS:
        missing = sorted(MISSION_FIELDS - set(mission))
        extra = sorted(set(mission) - MISSION_FIELDS)
        if missing:
            errors.append(f"mission.json não contém os campos: {', '.join(missing)}")
        if extra:
            errors.append(f"mission.json contém campos não reconhecidos: {', '.join(extra)}")
    if mission.get("schema_version") != 1:
        errors.append("mission.json tem uma versão não suportada")
    if mission.get("source_final_hash") != artifact.get("final_hash"):
        errors.append("mission.json não está associado ao hash final dos eventos")
    if mission.get("policy") != expected_policy:
        errors.append("mission.json não respeita a política de progresso selecionada")
    if expected_outcome is not None and mission.get("outcome") != expected_outcome:
        errors.append(
            f"esperava-se resultado de missão {expected_outcome!r}, recebido {mission.get('outcome')!r}"
        )
    if mission.get("safety_status") != report.get("status"):
        errors.append("mission.json não coincide com o estado de segurança do relatório")

    scenario = artifact.get("scenario", {})
    waypoint_count = len(scenario.get("mission", {}).get("waypoints", []))
    if mission.get("waypoint_count") != waypoint_count:
        errors.append("waypoint_count não coincide com a configuração da missão")
    for field in (
        "controller_waypoint_index",
        "waypoints_completed",
        "waypoints_remaining",
        "ticks_since_meaningful_progress",
        "fallback_ticks",
    ):
        value = mission.get(field)
        if type(value) is not int or value < 0:
            errors.append(f"mission.json tem um valor inválido em {field}")
    if type(mission.get("waypoints_completed")) is int and type(
        mission.get("waypoints_remaining")
    ) is int:
        if mission["waypoints_completed"] + mission["waypoints_remaining"] != waypoint_count:
            errors.append("waypoints_completed e waypoints_remaining são inconsistentes")
    if type(mission.get("controller_waypoint_index")) is int and mission[
        "controller_waypoint_index"
    ] > waypoint_count:
        errors.append("controller_waypoint_index excede o número de pontos de passagem")
    steps = scenario.get("steps")
    for field in (
        "last_meaningful_progress_tick",
        "stall_detected_tick",
        "completion_tick",
    ):
        value = mission.get(field)
        if value is not None and (type(value) is not int or value < 0 or value > steps):
            errors.append(f"mission.json tem um instante inválido em {field}")
    for field in ("distance_to_next_waypoint_mm", "best_distance_to_next_waypoint_mm"):
        value = mission.get(field)
        if value is not None and (type(value) is not int or value < 0):
            errors.append(f"mission.json tem uma distância inválida em {field}")

    holds = mission.get("hold_reason_counts")
    if not isinstance(holds, dict) or set(holds) != {
        "low_confidence",
        "fallback",
        "other",
    }:
        errors.append("hold_reason_counts não respeita o contrato M2")
    elif any(type(value) is not int or value < 0 for value in holds.values()):
        errors.append("hold_reason_counts contém uma contagem inválida")
    if mission.get("outcome") not in VALID_OUTCOMES:
        errors.append("mission.json contém um resultado de missão inválido")
    if mission.get("reason") is not None and not isinstance(mission.get("reason"), str):
        errors.append("mission.json contém uma causa inválida")
    return errors


def mission_metrics(mission: dict[str, Any]) -> dict[str, Any]:
    return {
        "outcome": mission.get("outcome"),
        "reason": mission.get("reason"),
        "policy": mission.get("policy"),
        "waypoint_count": mission.get("waypoint_count"),
        "controller_waypoint_index": mission.get("controller_waypoint_index"),
        "waypoints_completed": mission.get("waypoints_completed"),
        "waypoints_remaining": mission.get("waypoints_remaining"),
        "last_meaningful_progress_tick": mission.get("last_meaningful_progress_tick"),
        "ticks_since_meaningful_progress": mission.get("ticks_since_meaningful_progress"),
        "stall_detected_tick": mission.get("stall_detected_tick"),
        "distance_to_next_waypoint_mm": mission.get("distance_to_next_waypoint_mm"),
        "best_distance_to_next_waypoint_mm": mission.get("best_distance_to_next_waypoint_mm"),
        "fallback_ticks": mission.get("fallback_ticks"),
        "hold_reason_counts": mission.get("hold_reason_counts"),
        "completion_tick": mission.get("completion_tick"),
        "safety_status": mission.get("safety_status"),
    }


def observation_discrepancy(
    report: dict[str, Any], artifact: dict[str, Any], invariant_name: str
) -> bool:
    for result in report.get("invariants", []):
        if not isinstance(result, dict) or result.get("name") != invariant_name:
            continue
        failures = result.get("failures", [])
        if not failures:
            return False
        evidence = failures[0]
        observed = evidence.get("observed")
        if not isinstance(observed, dict):
            return False
        sample_tick = observed.get("sample_tick")
        if type(sample_tick) is not int:
            return False
        for record in artifact.get("events", []):
            if not isinstance(record, dict) or record.get("tick") != sample_tick:
                continue
            event = record.get("event")
            if not isinstance(event, dict) or event.get("kind") != "gps_sample":
                continue
            truth = event.get("truth_at_sample")
            position = event.get("observation")
            return isinstance(truth, dict) and isinstance(position, dict) and position != truth
        return False
    return False
