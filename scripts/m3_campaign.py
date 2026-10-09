"""Validate and plan a bounded M3 verification campaign."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath, PureWindowsPath
from typing import Any

from process_limits import CampaignArtifactBudget, ProcessLimitError, run_bounded_process


ROOT = Path(__file__).resolve().parents[1]
M3_ROOT = ROOT / "scenarios" / "m3"
DEFAULT_SEEDS = (42, 1337, 2026)
TEMPORAL_STATUSES = frozenset({"PASS", "FAIL", "INCONCLUSIVE"})
SAFETY_STATUSES = frozenset({"passed", "invariant_failed"})
PROGRESS_OUTCOMES = frozenset({"completed", "incomplete", "stalled", "invalid_terminated"})
MAX_SEEDS = 32
MAX_CASES = 80
MAX_SIMULATOR_RUNS = 256
MAX_ARTIFACT_BYTES = 64 * 1024 * 1024
MAX_TIME_BUDGET_SECONDS = 120
RUNS_PER_CASE = 3  # run, deterministic repeat, replay

_IDENTIFIER = re.compile(r"[A-Za-z][A-Za-z0-9_.-]{0,63}\Z")
_CONTRACT_ID = re.compile(r"[a-z0-9][a-z0-9_-]{0,63}\Z")
_SEED_TEXT = re.compile(r"(?:0|[1-9][0-9]*)\Z")
_ROOT_FIELDS = {
    "schema_version",
    "default_seeds",
    "contract_sets",
    "fault_combinations",
    "cases",
    "budgets",
}
_CASE_FIELDS = {
    "id",
    "scenario",
    "contract_set",
    "fault_combination",
    "expected_by_seed",
    "minimized_counterexample",
}
_EXPECTATION_FIELDS = {"temporal", "safety", "progress", "contracts"}
_BUDGET_FIELDS = {
    "max_cases",
    "max_runs",
    "max_artifact_bytes",
    "time_budget_seconds",
}


class M3CampaignError(ValueError):
    """Invalid M3 campaign configuration or resource plan."""


@dataclass(frozen=True)
class ExpectedOutcome:
    temporal: str
    safety: str
    progress: str
    contracts: dict[str, str]


@dataclass(frozen=True)
class PlannedCase:
    case_id: str
    scenario_path: str
    contract_set: str
    properties_path: str
    fault_combination: str
    actuator_config_path: str
    seed: int
    expected: ExpectedOutcome
    minimized_counterexample: dict[str, str] | None


@dataclass(frozen=True)
class CampaignPlan:
    cases: tuple[PlannedCase, ...]
    simulator_invocations: int
    max_cases: int
    max_runs: int
    max_artifact_bytes: int
    time_budget_seconds: float


@dataclass(frozen=True)
class M3Classification:
    temporal: str
    safety: str
    progress: str
    contracts: dict[str, str]
    errors: tuple[str, ...]


def _is_plain_int(value: Any) -> bool:
    return type(value) is int


def _require_int(value: Any, label: str, minimum: int, maximum: int) -> int:
    if not _is_plain_int(value) or not minimum <= value <= maximum:
        raise M3CampaignError(f"{label} must be an integer from {minimum} through {maximum}")
    return value


def _require_identifier(value: Any, label: str, *, contract: bool = False) -> str:
    pattern = _CONTRACT_ID if contract else _IDENTIFIER
    if not isinstance(value, str) or not pattern.fullmatch(value):
        raise M3CampaignError(f"{label} is missing or invalid")
    return value


def _require_relative_path(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value or len(value) > 256:
        raise M3CampaignError(f"{label} must be a non-empty relative path")
    if "\\" in value:
        raise M3CampaignError(f"{label} must use portable forward-slash separators")
    posix = PurePosixPath(value)
    windows = PureWindowsPath(value)
    parts = value.replace("\\", "/").split("/")
    if (
        posix.is_absolute()
        or windows.is_absolute()
        or windows.drive
        or any(part in {"", ".", ".."} for part in parts)
    ):
        raise M3CampaignError(f"{label} must stay inside the M3 campaign directory")
    return value


def _validate_seeds(value: Any, label: str) -> list[int]:
    if not isinstance(value, list) or not value or len(value) > MAX_SEEDS:
        raise M3CampaignError(f"{label} must contain 1 through {MAX_SEEDS} seeds")
    result = []
    for seed in value:
        result.append(_require_int(seed, label, 0, 2**64 - 1))
    if len(set(result)) != len(result):
        raise M3CampaignError(f"{label} must not contain duplicate seeds")
    return result


def _validate_named_paths(value: Any, label: str) -> dict[str, str]:
    if not isinstance(value, dict) or not value:
        raise M3CampaignError(f"{label} must be a non-empty object")
    return {
        _require_identifier(name, f"{label} name"): _require_relative_path(path, f"{label} path")
        for name, path in value.items()
    }


def _validate_outcome(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != _EXPECTATION_FIELDS:
        raise M3CampaignError(f"{label} must define temporal, safety, progress and contracts")
    temporal = value["temporal"]
    safety = value["safety"]
    progress = value["progress"]
    contracts = value["contracts"]
    if not isinstance(temporal, str) or temporal not in TEMPORAL_STATUSES:
        raise M3CampaignError(f"{label} has an unknown temporal outcome")
    if not isinstance(safety, str) or safety not in SAFETY_STATUSES:
        raise M3CampaignError(f"{label} has an unknown safety outcome")
    if not isinstance(progress, str) or progress not in PROGRESS_OUTCOMES:
        raise M3CampaignError(f"{label} has an unknown progress outcome")
    if not isinstance(contracts, dict) or not contracts:
        raise M3CampaignError(f"{label} must list expected outcomes for its contracts")
    checked_contracts: dict[str, str] = {}
    for contract_id, status in contracts.items():
        checked_id = _require_identifier(contract_id, f"{label} contract id", contract=True)
        if not isinstance(status, str) or status not in TEMPORAL_STATUSES:
            raise M3CampaignError(f"{label} has an unknown result for contract {checked_id}")
        checked_contracts[checked_id] = status
    contract_statuses = set(checked_contracts.values())
    aggregate = (
        "FAIL"
        if "FAIL" in contract_statuses
        else "INCONCLUSIVE"
        if "INCONCLUSIVE" in contract_statuses
        else "PASS"
    )
    if temporal != aggregate:
        raise M3CampaignError(f"{label} temporal summary disagrees with contract outcomes")
    return {
        "temporal": temporal,
        "safety": safety,
        "progress": progress,
        "contracts": checked_contracts,
    }


def _validate_counterexample(value: Any, label: str) -> dict[str, str] | None:
    if value is None:
        return None
    fields = {"original", "reduced", "summary"}
    if not isinstance(value, dict) or set(value) != fields:
        raise M3CampaignError(f"{label} must identify original, reduced and summary artifacts")
    return {
        key: _require_relative_path(value[key], f"{label}.{key}")
        for key in ("original", "reduced", "summary")
    }


def _validate_budgets(value: Any) -> dict[str, int]:
    if not isinstance(value, dict) or set(value) != _BUDGET_FIELDS:
        raise M3CampaignError("budgets must define max_cases, max_runs, max_artifact_bytes and time_budget_seconds")
    return {
        "max_cases": _require_int(value["max_cases"], "max_cases", 1, MAX_CASES),
        "max_runs": _require_int(value["max_runs"], "max_runs", 1, MAX_SIMULATOR_RUNS),
        "max_artifact_bytes": _require_int(
            value["max_artifact_bytes"], "max_artifact_bytes", 65_536, MAX_ARTIFACT_BYTES
        ),
        "time_budget_seconds": _require_int(
            value["time_budget_seconds"], "time_budget_seconds", 1, MAX_TIME_BUDGET_SECONDS
        ),
    }


def validate_m3_manifest(value: Any) -> dict[str, Any]:
    """Validate manifest shape and expectations without reading scenario files."""
    if not isinstance(value, dict) or set(value) != _ROOT_FIELDS:
        raise M3CampaignError("M3 manifest has missing or unknown fields")
    if value["schema_version"] != 1 or type(value["schema_version"]) is not int:
        raise M3CampaignError("unsupported M3 manifest schema version")
    seeds = _validate_seeds(value["default_seeds"], "default_seeds")
    contract_sets = _validate_named_paths(value["contract_sets"], "contract_sets")
    fault_combinations = _validate_named_paths(value["fault_combinations"], "fault_combinations")
    raw_cases = value["cases"]
    if not isinstance(raw_cases, list) or not raw_cases:
        raise M3CampaignError("M3 manifest must contain at least one case")

    case_ids: set[str] = set()
    cases: list[dict[str, Any]] = []
    for index, raw_case in enumerate(raw_cases):
        label = f"cases[{index}]"
        if not isinstance(raw_case, dict) or set(raw_case) - _CASE_FIELDS or not {
            "id",
            "scenario",
            "contract_set",
            "fault_combination",
            "expected_by_seed",
        } <= set(raw_case):
            raise M3CampaignError(f"{label} has missing or unknown fields")
        case_id = _require_identifier(raw_case["id"], f"{label}.id")
        if case_id in case_ids:
            raise M3CampaignError(f"duplicate M3 case id: {case_id}")
        case_ids.add(case_id)
        scenario_path = _require_relative_path(raw_case["scenario"], f"{label}.scenario")
        contract_set = _require_identifier(raw_case["contract_set"], f"{label}.contract_set")
        fault_combination = _require_identifier(
            raw_case["fault_combination"], f"{label}.fault_combination"
        )
        if contract_set not in contract_sets:
            raise M3CampaignError(f"{label} references an unknown contract set")
        if fault_combination not in fault_combinations:
            raise M3CampaignError(f"{label} references an unknown fault combination")

        raw_expected = raw_case["expected_by_seed"]
        if not isinstance(raw_expected, dict) or not raw_expected:
            raise M3CampaignError(f"{label}.expected_by_seed must be a non-empty object")
        expected_by_seed: dict[str, ExpectedOutcome] = {}
        contract_ids: set[str] | None = None
        for seed_text, outcome in raw_expected.items():
            if not isinstance(seed_text, str) or not _SEED_TEXT.fullmatch(seed_text):
                raise M3CampaignError(f"{label} contains a malformed expected seed")
            seed = int(seed_text)
            if seed > 2**64 - 1:
                raise M3CampaignError(f"{label} expected seed exceeds u64")
            checked_outcome = _validate_outcome(outcome, f"{label}, seed {seed}")
            current_contract_ids = set(checked_outcome["contracts"])
            if contract_ids is None:
                contract_ids = current_contract_ids
            elif contract_ids != current_contract_ids:
                raise M3CampaignError(f"{label} contract outcomes list differs between seeds")
            expected_by_seed[seed_text] = checked_outcome
        if not set(map(str, seeds)) <= set(expected_by_seed):
            raise M3CampaignError(f"{label} is missing an expected outcome for a default seed")
        cases.append(
            {
                "id": case_id,
                "scenario": scenario_path,
                "contract_set": contract_set,
                "fault_combination": fault_combination,
                "expected_by_seed": expected_by_seed,
                "minimized_counterexample": _validate_counterexample(
                    raw_case.get("minimized_counterexample"), f"{label}.minimized_counterexample"
                ),
            }
        )
    budgets = _validate_budgets(value["budgets"])
    return {
        "schema_version": 1,
        "default_seeds": seeds,
        "contract_sets": contract_sets,
        "fault_combinations": fault_combinations,
        "cases": cases,
        "budgets": budgets,
    }


def load_m3_manifest(path: Path | None = None) -> dict[str, Any]:
    """Load and validate a standalone M3 campaign manifest."""
    manifest_path = path or M3_ROOT / "expectations.json"
    try:
        value = json.loads(
            manifest_path.read_text(encoding="utf-8"),
            object_pairs_hook=_reject_duplicate_json_keys,
        )
    except (OSError, json.JSONDecodeError) as error:
        raise M3CampaignError(f"cannot read M3 campaign manifest: {error}") from error
    return validate_m3_manifest(value)


def _reject_duplicate_json_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise M3CampaignError(f"duplicate JSON field: {key}")
        result[key] = value
    return result


def parse_seeds(value: str) -> tuple[int, ...]:
    """Parse a bounded, unique comma-separated list of unsigned 64-bit seeds."""
    try:
        seeds = tuple(int(part.strip(), 10) for part in value.split(","))
    except ValueError as error:
        raise argparse.ArgumentTypeError("seeds must be comma-separated integers") from error
    if not seeds or len(seeds) > MAX_SEEDS or any(seed < 0 or seed > 2**64 - 1 for seed in seeds):
        raise argparse.ArgumentTypeError(f"seeds must contain 1 through {MAX_SEEDS} u64 values")
    if len(set(seeds)) != len(seeds):
        raise argparse.ArgumentTypeError("seeds must not contain duplicates")
    return seeds


def _selection(value: tuple[str, ...] | None, available: set[str], label: str) -> set[str]:
    if value is None:
        return set(available)
    if not value or len(set(value)) != len(value):
        raise M3CampaignError(f"{label} selection must be non-empty and contain no duplicates")
    missing = set(value) - available
    if missing:
        raise M3CampaignError(f"unknown {label}: {', '.join(sorted(missing))}")
    return set(value)


def _selected_seeds(value: tuple[int, ...] | None, defaults: list[int]) -> tuple[int, ...]:
    seeds = tuple(defaults) if value is None else value
    if (
        not seeds
        or len(seeds) > MAX_SEEDS
        or len(set(seeds)) != len(seeds)
        or any(not _is_plain_int(seed) or seed < 0 or seed > 2**64 - 1 for seed in seeds)
    ):
        raise M3CampaignError(f"seeds must contain 1 through {MAX_SEEDS} unique u64 values")
    return seeds


def plan_matrix(
    manifest: dict[str, Any],
    *,
    contract_sets: tuple[str, ...] | None = None,
    scenario_names: tuple[str, ...] | None = None,
    fault_combinations: tuple[str, ...] | None = None,
    seeds: tuple[int, ...] | None = None,
    max_cases: int | None = None,
    max_runs: int | None = None,
    max_artifact_bytes: int | None = None,
    time_budget_seconds: float | None = None,
) -> CampaignPlan:
    """Expand selected manifest cases and seeds before any process is started."""
    checked = validate_m3_manifest(manifest)
    selected_contract_sets = _selection(
        contract_sets, set(checked["contract_sets"]), "contract set"
    )
    selected_scenarios = _selection(
        scenario_names, {case["id"] for case in checked["cases"]}, "scenario"
    )
    selected_faults = _selection(
        fault_combinations, set(checked["fault_combinations"]), "fault combination"
    )
    selected_seeds = _selected_seeds(seeds, checked["default_seeds"])
    budgets = checked["budgets"]
    effective_cases = budgets["max_cases"] if max_cases is None else _require_int(
        max_cases, "max_cases", 1, MAX_CASES
    )
    effective_runs = budgets["max_runs"] if max_runs is None else _require_int(
        max_runs, "max_runs", 1, MAX_SIMULATOR_RUNS
    )
    effective_artifacts = budgets["max_artifact_bytes"] if max_artifact_bytes is None else _require_int(
        max_artifact_bytes, "max_artifact_bytes", 65_536, MAX_ARTIFACT_BYTES
    )
    if time_budget_seconds is None:
        effective_time = float(budgets["time_budget_seconds"])
    elif (
        isinstance(time_budget_seconds, bool)
        or not isinstance(time_budget_seconds, (int, float))
        or not math.isfinite(time_budget_seconds)
        or time_budget_seconds <= 0
    ):
        raise M3CampaignError("time_budget_seconds must be finite and greater than zero")
    else:
        effective_time = min(float(time_budget_seconds), float(MAX_TIME_BUDGET_SECONDS))
    effective_cases = min(effective_cases, budgets["max_cases"])
    effective_runs = min(effective_runs, budgets["max_runs"])
    effective_artifacts = min(effective_artifacts, budgets["max_artifact_bytes"])
    effective_time = min(effective_time, float(budgets["time_budget_seconds"]))

    cases: list[PlannedCase] = []
    for entry in checked["cases"]:
        if (
            entry["id"] not in selected_scenarios
            or entry["contract_set"] not in selected_contract_sets
            or entry["fault_combination"] not in selected_faults
        ):
            continue
        for seed in selected_seeds:
            outcome = entry["expected_by_seed"].get(str(seed))
            if outcome is None:
                raise M3CampaignError(
                    f"no expected result for {entry['id']} and seed {seed}"
                )
            cases.append(
                PlannedCase(
                    case_id=entry["id"],
                    scenario_path=entry["scenario"],
                    contract_set=entry["contract_set"],
                    properties_path=checked["contract_sets"][entry["contract_set"]],
                    fault_combination=entry["fault_combination"],
                    actuator_config_path=checked["fault_combinations"][entry["fault_combination"]],
                    seed=seed,
                    expected=ExpectedOutcome(**outcome),
                    minimized_counterexample=entry["minimized_counterexample"],
                )
            )
    if not cases:
        raise M3CampaignError("selected M3 campaign matrix contains no cases")
    invocation_count = len(cases) * RUNS_PER_CASE
    if len(cases) > effective_cases:
        raise M3CampaignError(
            f"planned matrix has {len(cases)} cases, above max_cases {effective_cases}"
        )
    if invocation_count > effective_runs:
        raise M3CampaignError(
            f"planned matrix needs {invocation_count} simulator invocations, above max_runs {effective_runs}"
        )
    return CampaignPlan(
        cases=tuple(cases),
        simulator_invocations=invocation_count,
        max_cases=effective_cases,
        max_runs=effective_runs,
        max_artifact_bytes=effective_artifacts,
        time_budget_seconds=effective_time,
    )


def classify_outputs(
    expected: ExpectedOutcome,
    report: dict[str, Any],
    mission: dict[str, Any],
    verification: dict[str, Any],
) -> M3Classification:
    """Classify Rust output without conflating temporal, safety or progress results."""
    if not isinstance(report, dict) or not isinstance(mission, dict) or not isinstance(
        verification, dict
    ):
        raise M3CampaignError("run outputs must be JSON objects")

    raw_results = verification.get("contract_results")
    if not isinstance(raw_results, list) or not raw_results:
        raise M3CampaignError("verification.json has no contract_results")
    contract_results: dict[str, str] = {}
    for index, result in enumerate(raw_results):
        if not isinstance(result, dict):
            raise M3CampaignError(f"contract_results[{index}] must be an object")
        contract_id = _require_identifier(
            result.get("contract_id"), f"contract_results[{index}].contract_id", contract=True
        )
        outcome = result.get("outcome")
        if not isinstance(outcome, str) or outcome not in TEMPORAL_STATUSES:
            raise M3CampaignError(f"contract_results[{index}] has an unknown outcome")
        if contract_id in contract_results:
            raise M3CampaignError(f"verification.json repeats contract {contract_id}")
        contract_results[contract_id] = outcome

    temporal = (
        "FAIL"
        if "FAIL" in contract_results.values()
        else "INCONCLUSIVE"
        if "INCONCLUSIVE" in contract_results.values()
        else "PASS"
    )
    safety = report.get("status")
    progress = mission.get("outcome")
    if not isinstance(safety, str) or safety not in SAFETY_STATUSES:
        raise M3CampaignError("report.json has an unknown safety status")
    if not isinstance(progress, str) or progress not in PROGRESS_OUTCOMES:
        raise M3CampaignError("mission.json has an unknown progress outcome")

    errors: list[str] = []
    if temporal != expected.temporal:
        errors.append(f"temporal outcome expected {expected.temporal}, received {temporal}")
    if safety != expected.safety:
        errors.append(f"safety outcome expected {expected.safety}, received {safety}")
    if progress != expected.progress:
        errors.append(f"progress outcome expected {expected.progress}, received {progress}")
    if contract_results != expected.contracts:
        errors.append("per-contract outcomes differ from expectations")
    return M3Classification(
        temporal=temporal,
        safety=safety,
        progress=progress,
        contracts=contract_results,
        errors=tuple(errors),
    )


def m3_fixture_checksums(
    manifest: dict[str, Any], manifest_path: Path | None = None
) -> dict[str, str]:
    """Hash every scenario, contract set, actuator config and manifest input."""
    checked = validate_m3_manifest(manifest)
    source = (manifest_path or M3_ROOT / "expectations.json").resolve()
    base = source.parent.resolve()
    paths = {
        source.name,
        *[case["scenario"] for case in checked["cases"]],
        *checked["contract_sets"].values(),
        *checked["fault_combinations"].values(),
    }
    checksums: dict[str, str] = {}
    for relative in sorted(paths):
        path = (base / relative).resolve()
        try:
            path.relative_to(base)
        except ValueError as error:
            raise M3CampaignError(f"M3 fixture escapes its manifest directory: {relative}") from error
        try:
            contents = path.read_bytes()
        except OSError as error:
            raise M3CampaignError(f"cannot read M3 fixture {relative}: {error}") from error
        checksums[f"m3/{relative.replace('\\', '/')}"] = hashlib.sha256(contents).hexdigest()
    return checksums


def parse_name_list(value: str) -> tuple[str, ...]:
    names = tuple(part.strip() for part in value.split(","))
    if not names or any(not name for name in names) or len(set(names)) != len(names):
        raise argparse.ArgumentTypeError("names must be comma-separated, non-empty and unique")
    return names


def _sha256(contents: bytes) -> str:
    return hashlib.sha256(contents).hexdigest()


def _load_json(path: Path, label: str) -> dict[str, Any]:
    try:
        result = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise M3CampaignError(f"cannot read {label}: {error}") from error
    if not isinstance(result, dict):
        raise M3CampaignError(f"{label} must be a JSON object")
    return result


def _unique_campaign_dir(base: Path) -> Path:
    root = base.expanduser().resolve()
    root.mkdir(parents=True, exist_ok=True)
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    for suffix in range(10_000):
        name = f"campaign-m3-{timestamp}" if suffix == 0 else f"campaign-m3-{timestamp}-{suffix}"
        destination = root / name
        try:
            destination.mkdir()
            return destination
        except FileExistsError:
            continue
    raise M3CampaignError("cannot choose a unique M3 campaign output directory")


def _portable_path(path: Path, root: Path) -> str:
    return path.resolve().relative_to(root.resolve()).as_posix()


def _remaining_time(deadline: float) -> float:
    remaining = deadline - time.monotonic()
    if not math.isfinite(remaining) or remaining <= 0:
        raise ProcessLimitError("M3 campaign exceeded its global time budget")
    return min(30.0, remaining)


def _counted_process(
    command: list[str],
    *,
    deadline: float,
    operation: str,
    command_counts: dict[str, dict[str, int]],
) -> Any:
    command_counts[operation]["attempted"] += 1
    result = run_bounded_process(
        command,
        cwd=ROOT,
        timeout_seconds=_remaining_time(deadline),
        max_output_bytes=64 * 1024,
        description=f"M3 {operation}",
    )
    command_counts[operation]["completed"] += 1
    return result


def _read_run_artifacts(output_dir: Path, max_output_bytes: int) -> tuple[dict[str, bytes], dict[str, Any]]:
    names = ("events.json", "report.json", "mission.json", "verification.json")
    artifacts: dict[str, bytes] = {}
    for name in names:
        try:
            artifacts[name] = (output_dir / name).read_bytes()
        except OSError as error:
            raise M3CampaignError(f"run did not produce {name}: {error}") from error
    if sum(map(len, artifacts.values())) > max_output_bytes:
        raise M3CampaignError("run artifacts exceed the requested output limit")
    try:
        parsed = {
            name.removesuffix(".json"): json.loads(contents)
            for name, contents in artifacts.items()
        }
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise M3CampaignError(f"run produced invalid JSON artifacts: {error}") from error
    if any(not isinstance(value, dict) for value in parsed.values()):
        raise M3CampaignError("run artifacts must contain JSON objects")
    return artifacts, parsed


def _check_artifact_consistency(parsed: dict[str, dict[str, Any]]) -> None:
    artifact = parsed["events"]
    report = parsed["report"]
    verification = parsed["verification"]
    events = artifact.get("events")
    if not isinstance(events, list) or artifact.get("event_count") != len(events):
        raise M3CampaignError("events.json has an inconsistent event count")
    if artifact.get("expected_report") != report:
        raise M3CampaignError("events.json expected_report differs from report.json")
    if artifact.get("final_hash") != report.get("final_hash"):
        raise M3CampaignError("events.json final hash differs from report.json")
    source_hash = verification.get("source_artifact_sha256")
    if source_hash != artifact.get("artifact_sha256"):
        raise M3CampaignError("verification.json references a different source artifact")


def _check_counterexample_reference(
    reference: dict[str, str] | None, repository_root: Path = ROOT
) -> tuple[dict[str, str] | None, bool]:
    if reference is None:
        return None, False
    resolved_paths: dict[str, str] = {}
    all_exist = True
    for kind, relative in reference.items():
        path = (repository_root / relative).resolve()
        try:
            path.relative_to(repository_root.resolve())
        except ValueError as error:
            raise M3CampaignError("minimized counterexample reference escapes the repository") from error
        all_exist = all_exist and path.is_file()
        resolved_paths[kind] = path.relative_to(repository_root.resolve()).as_posix()
    return (resolved_paths if all_exist else None), all_exist


def _execute_case(
    *,
    binary: Path,
    manifest_root: Path,
    case: PlannedCase,
    case_dir: Path,
    artifact_budget: CampaignArtifactBudget,
    deadline: float,
    command_counts: dict[str, dict[str, int]],
) -> dict[str, Any]:
    file_inputs = {
        "scenario": case.scenario_path,
        "properties": case.properties_path,
        "actuator_config": case.actuator_config_path,
    }
    resolved_inputs: dict[str, Path] = {}
    input_hashes: dict[str, str] = {}
    for label, relative in file_inputs.items():
        path = (manifest_root / relative).resolve()
        try:
            path.relative_to(manifest_root.resolve())
        except ValueError as error:
            raise M3CampaignError(f"{label} input escapes the M3 manifest directory") from error
        if not path.is_file():
            raise M3CampaignError(f"M3 {label} file does not exist: {relative}")
        resolved_inputs[label] = path
        input_hashes[f"{label}_sha256"] = _sha256(path.read_bytes())

    case_dir.mkdir(parents=True, exist_ok=False)
    first_dir = case_dir / "run"
    repeat_dir = case_dir / "repeat"
    first_dir.mkdir()
    repeat_dir.mkdir()
    outputs: list[dict[str, Any]] = []
    artifact_sets: list[dict[str, bytes]] = []
    for label, output_dir in (("run", first_dir), ("repeat", repeat_dir)):
        remaining_bytes = artifact_budget.remaining(case_dir.parent)
        run_output_limit = min(16 * 1024 * 1024, remaining_bytes // 2)
        if run_output_limit < 1:
            raise M3CampaignError("M3 campaign artifact budget is exhausted")
        command = [
            str(binary),
            "run",
            str(resolved_inputs["scenario"]),
            "--seed",
            str(case.seed),
            "--output",
            str(output_dir),
            "--contracts",
            str(resolved_inputs["properties"]),
            "--actuator",
            str(resolved_inputs["actuator_config"]),
            "--max-output-bytes",
            str(run_output_limit),
        ]
        run = _counted_process(
            command,
            deadline=deadline,
            operation="run",
            command_counts=command_counts,
        )
        artifacts, parsed = _read_run_artifacts(output_dir, run_output_limit)
        _check_artifact_consistency(parsed)
        expected_exit = 0 if parsed["report"].get("status") == "passed" else 2
        if run.returncode != expected_exit:
            raise M3CampaignError(
                f"{label} returned {run.returncode}, inconsistent with safety status"
            )
        artifact_sets.append(artifacts)
        outputs.append(parsed)
        artifact_budget.refresh(case_dir.parent)

    repeated = artifact_sets[0] == artifact_sets[1]
    if not repeated:
        raise M3CampaignError("deterministic repeat differs byte-for-byte from the first M3 run")

    replay = _counted_process(
        [str(binary), "replay", str(first_dir / "events.json")],
        deadline=deadline,
        operation="replay",
        command_counts=command_counts,
    )
    if replay.returncode != 0:
        raise M3CampaignError(f"replay returned {replay.returncode}")

    classification = classify_outputs(
        case.expected,
        outputs[0]["report"],
        outputs[0]["mission"],
        outputs[0]["verification"],
    )
    errors = list(classification.errors)
    reference, reference_verified = _check_counterexample_reference(
        case.minimized_counterexample
    )
    if case.minimized_counterexample and reference_verified:
        counterexample_reference = reference
    else:
        counterexample_reference = None

    first_artifacts = artifact_sets[0]
    temporal_failures = [
        result
        for result in outputs[0]["verification"].get("contract_results", [])
        if isinstance(result, dict) and result.get("outcome") == "FAIL"
    ]
    return {
        "suite": "m3",
        "case_id": case.case_id,
        "scenario": case.scenario_path,
        "contract_set": case.contract_set,
        "fault_combination": case.fault_combination,
        "seed": case.seed,
        "expected": {
            "temporal": case.expected.temporal,
            "safety": case.expected.safety,
            "progress": case.expected.progress,
            "contracts": case.expected.contracts,
        },
        "actual": {
            "temporal": classification.temporal,
            "safety": classification.safety,
            "progress": classification.progress,
            "contracts": classification.contracts,
        },
        "result": "passed" if not errors else "failed",
        "errors": errors,
        "deterministic": repeated,
        "replay_exit_code": replay.returncode,
        "temporal_failures": temporal_failures,
        "temporal_failure_count": len(temporal_failures),
        "minimized_counterexample": counterexample_reference,
        "minimized_counterexample_verified": reference_verified,
        "output_paths": {
            "events": _portable_path(first_dir / "events.json", ROOT),
            "report": _portable_path(first_dir / "report.json", ROOT),
            "mission": _portable_path(first_dir / "mission.json", ROOT),
            "verification": _portable_path(first_dir / "verification.json", ROOT),
        },
        "sha256": {
            **input_hashes,
            **{
                name.removesuffix(".json") + "_sha256": _sha256(contents)
                for name, contents in first_artifacts.items()
            },
        },
    }


def _campaign_summary(
    *,
    cases: list[dict[str, Any]],
    plan: CampaignPlan,
    campaign_dir: Path,
    started: float,
    command_counts: dict[str, dict[str, int]],
    fixture_checksums: dict[str, str],
    seeds: tuple[int, ...],
    manifest_path: Path,
) -> dict[str, Any]:
    temporal_counts = {outcome: 0 for outcome in sorted(TEMPORAL_STATUSES)}
    safety_counts = {outcome: 0 for outcome in sorted(SAFETY_STATUSES)}
    progress_counts = {outcome: 0 for outcome in sorted(PROGRESS_OUTCOMES)}
    contracts: dict[str, dict[str, int]] = {}
    for case in cases:
        actual = case.get("actual", {})
        if actual.get("temporal") in temporal_counts:
            temporal_counts[actual["temporal"]] += 1
        if actual.get("safety") in safety_counts:
            safety_counts[actual["safety"]] += 1
        if actual.get("progress") in progress_counts:
            progress_counts[actual["progress"]] += 1
        for contract_id, outcome in actual.get("contracts", {}).items():
            counts = contracts.setdefault(
                contract_id,
                {status: 0 for status in (*sorted(TEMPORAL_STATUSES), "expected_failures", "unexpected_failures")},
            )
            counts[outcome] += 1
            expected = case.get("expected", {}).get("contracts", {}).get(contract_id)
            if outcome == "FAIL":
                counts["expected_failures" if expected == "FAIL" else "unexpected_failures"] += 1

    failed = sum(case.get("result") != "passed" for case in cases)
    return {
        "schema_version": 1,
        "suite": "m3",
        "status": "passed" if failed == 0 and len(cases) == len(plan.cases) else "failed",
        "case_count": len(cases),
        "planned_case_count": len(plan.cases),
        "passed_cases": len(cases) - failed,
        "failed_cases": failed,
        "seeds": list(seeds),
        "manifest_path": _portable_path(manifest_path, ROOT),
        "campaign_directory": _portable_path(campaign_dir, ROOT),
        "elapsed_seconds": round(time.monotonic() - started, 4),
        "budgets": {
            "max_cases": plan.max_cases,
            "max_runs": plan.max_runs,
            "max_artifact_bytes": plan.max_artifact_bytes,
            "time_budget_seconds": plan.time_budget_seconds,
        },
        "execution_counts": {
            "simulator_invocations": sum(item["attempted"] for item in command_counts.values()),
            "run": command_counts["run"],
            "replay": command_counts["replay"],
        },
        "deterministic_repeats": {
            "checked": sum("deterministic" in case for case in cases),
            "passed": sum(case.get("deterministic") is True for case in cases),
        },
        "replays": {
            "checked": sum("replay_exit_code" in case for case in cases),
            "passed": sum(case.get("replay_exit_code") == 0 for case in cases),
        },
        "temporal_outcome_counts": temporal_counts,
        "safety_outcome_counts": safety_counts,
        "progress_outcome_counts": progress_counts,
        "contract_outcome_counts": contracts,
        "temporal_failure_counts": {
            contract_id: counts["FAIL"] for contract_id, counts in contracts.items() if counts["FAIL"]
        },
        "cases": cases,
        "fixture_checksums_sha256": fixture_checksums,
    }


def _write_campaign_outputs(
    campaign_dir: Path, summary: dict[str, Any], max_artifact_bytes: int
) -> None:
    fingerprints = {
        "schema_version": 1,
        "suite": "m3",
        "suites": ["m3"],
        "m3_scenarios": sorted({case["case_id"] for case in summary["cases"]}),
        "m3_contract_sets": sorted({case["contract_set"] for case in summary["cases"]}),
        "m3_fault_combinations": sorted({case["fault_combination"] for case in summary["cases"]}),
        "fixture_checksums_sha256": summary["fixture_checksums_sha256"],
        "outputs": [
            {
                "suite": "m3",
                "scenario": case["case_id"],
                "variant": f"{case['contract_set']}+{case['fault_combination']}",
                "contract_set": case["contract_set"],
                "fault_combination": case["fault_combination"],
                "seed": case["seed"],
                **case["sha256"],
            }
            for case in summary["cases"]
            if "sha256" in case
        ],
    }
    report = [
        "# Resumo da campanha M3",
        "",
        f"Estado: **{summary['status']}**.",
        f"Casos: {summary['passed_cases']} aprovados e {summary['failed_cases']} divergentes, de {summary['planned_case_count']} planeados.",
        f"Passagens do monitor temporal: {summary['temporal_outcome_counts']}.",
        f"Resultados de segurança: {summary['safety_outcome_counts']}.",
        f"Resultados de progresso: {summary['progress_outcome_counts']}.",
        f"Execuções: {summary['execution_counts']['simulator_invocations']} chamadas, incluindo repetição determinística e reprodução.",
        "",
        "Os resultados por caso estão em `summary.json`; os hashes comparáveis estão em `fingerprints.json`.",
        "",
    ]
    files = {
        "summary.json": json.dumps(summary, ensure_ascii=False, indent=2, sort_keys=True).encode("utf-8") + b"\n",
        "campaign-report.md": "\n".join(report).encode("utf-8"),
        "fingerprints.json": json.dumps(fingerprints, ensure_ascii=False, indent=2, sort_keys=True).encode("utf-8") + b"\n",
    }
    for name, contents in files.items():
        (campaign_dir / name).write_bytes(contents)
    total = sum(path.stat().st_size for path in campaign_dir.rglob("*") if path.is_file())
    if total > max_artifact_bytes:
        raise M3CampaignError(
            f"M3 campaign artifacts used {total} bytes, above {max_artifact_bytes} bytes"
        )


def run_campaign(args: Any) -> int:
    """Execute the M3 case/seed matrix with bounded runs, artifacts and time."""
    started = time.monotonic()
    manifest_path = Path(getattr(args, "m3_manifest", None) or M3_ROOT / "expectations.json")
    if not manifest_path.is_absolute():
        manifest_path = ROOT / manifest_path
    manifest_path = manifest_path.resolve()
    manifest = load_m3_manifest(manifest_path)
    plan = plan_matrix(
        manifest,
        contract_sets=getattr(args, "m3_contract_sets", None),
        scenario_names=getattr(args, "m3_scenarios", None),
        fault_combinations=getattr(args, "m3_fault_combinations", None),
        seeds=tuple(args.seeds) if getattr(args, "seeds", None) is not None else None,
        max_cases=min(args.max_cases, MAX_CASES),
        max_runs=min(args.max_runs, MAX_SIMULATOR_RUNS),
        max_artifact_bytes=min(args.max_artifact_bytes, MAX_ARTIFACT_BYTES),
        time_budget_seconds=args.time_budget_seconds,
    )
    binary = Path(args.binary)
    if not binary.is_absolute():
        binary = ROOT / binary
    binary = binary.resolve()
    if not binary.is_file():
        raise M3CampaignError(f"compiled vv-lab binary not found: {binary.name}")

    output_root = Path(args.output_root)
    if not output_root.is_absolute():
        output_root = ROOT / output_root
    campaign_dir = _unique_campaign_dir(output_root)
    summary_reserve = min(
        plan.max_artifact_bytes // 4,
        max(65_536, len(plan.cases) * 8192),
    )
    case_artifact_limit = plan.max_artifact_bytes - summary_reserve
    if case_artifact_limit < 1:
        raise M3CampaignError("artifact limit leaves no room for M3 run outputs")
    artifact_budget = CampaignArtifactBudget(campaign_dir, case_artifact_limit)
    command_counts = {name: {"attempted": 0, "completed": 0} for name in ("run", "replay")}
    fixture_checksums = m3_fixture_checksums(manifest, manifest_path)
    deadline = started + plan.time_budget_seconds
    cases: list[dict[str, Any]] = []
    for selected in plan.cases:
        case_started = time.monotonic()
        case_dir = campaign_dir / selected.case_id / f"seed-{selected.seed}"
        try:
            result = _execute_case(
                binary=binary,
                manifest_root=manifest_path.parent,
                case=selected,
                case_dir=case_dir,
                artifact_budget=artifact_budget,
                deadline=deadline,
                command_counts=command_counts,
            )
        except (M3CampaignError, ProcessLimitError, OSError) as error:
            result = {
                "suite": "m3",
                "case_id": selected.case_id,
                "scenario": selected.scenario_path,
                "contract_set": selected.contract_set,
                "fault_combination": selected.fault_combination,
                "seed": selected.seed,
                "expected": {
                    "temporal": selected.expected.temporal,
                    "safety": selected.expected.safety,
                    "progress": selected.expected.progress,
                    "contracts": selected.expected.contracts,
                },
                "result": "failed",
                "errors": [str(error)],
                "elapsed_seconds": round(time.monotonic() - case_started, 4),
            }
            if case_dir.exists():
                artifact_budget.refresh(case_dir.parent)
        result["elapsed_seconds"] = round(time.monotonic() - case_started, 4)
        cases.append(result)
        if time.monotonic() >= deadline:
            break

    summary = _campaign_summary(
        cases=cases,
        plan=plan,
        campaign_dir=campaign_dir,
        started=started,
        command_counts=command_counts,
        fixture_checksums=fixture_checksums,
        seeds=tuple(args.seeds) if getattr(args, "seeds", None) is not None else tuple(manifest["default_seeds"]),
        manifest_path=manifest_path,
    )
    _write_campaign_outputs(campaign_dir, summary, plan.max_artifact_bytes)
    print(
        f"Campanha M3 {summary['status']}: {summary['passed_cases']}/{summary['planned_case_count']} casos, "
        f"{summary['execution_counts']['simulator_invocations']} chamadas."
    )
    return 0 if summary["status"] == "passed" else 1
