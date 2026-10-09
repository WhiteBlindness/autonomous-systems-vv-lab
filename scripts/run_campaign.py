#!/usr/bin/env python3
"""Run a bounded, replay-checked scenario campaign using only the Python stdlib."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from m2_campaign import (
    M2CampaignError,
    check_mission_sidecar,
    fixture_checksums as m2_fixture_checksums,
    load_m2_manifest,
    matched_geometry_errors,
    mission_metrics,
    observation_discrepancy,
    scenario_for_variant,
    select_variant,
)
from process_limits import (
    CampaignArtifactBudget,
    ProcessLimitError,
    run_bounded_process,
)


ROOT = Path(__file__).resolve().parents[1]
EXPECTED_STATUS = {
    "basic-mission": "passed",
    "gps-dropout": "passed",
    "communication-fault": "passed",
    "restricted-zone-failure": "invariant_failed",
    "restricted-zone-boundary": "invariant_failed",
    "safe-fallback-failure": "invariant_failed",
    "invalid-transition": "invariant_failed",
    "world-bounds-failure": "invariant_failed",
}
INVARIANT_NAMES = (
    "restricted_zone",
    "world_bounds",
    "safe_fallback",
    "valid_state_transitions",
)
EXPECTED_INVARIANTS = {
    "basic-mission": dict.fromkeys(INVARIANT_NAMES, True),
    "gps-dropout": dict.fromkeys(INVARIANT_NAMES, True),
    "communication-fault": dict.fromkeys(INVARIANT_NAMES, True),
    "restricted-zone-failure": {
        "restricted_zone": False,
        "world_bounds": True,
        "safe_fallback": True,
        "valid_state_transitions": True,
    },
    "restricted-zone-boundary": {
        "restricted_zone": False,
        "world_bounds": True,
        "safe_fallback": True,
        "valid_state_transitions": True,
    },
    "safe-fallback-failure": {
        "restricted_zone": True,
        "world_bounds": True,
        "safe_fallback": False,
        "valid_state_transitions": True,
    },
    "invalid-transition": {
        "restricted_zone": True,
        "world_bounds": True,
        "safe_fallback": True,
        "valid_state_transitions": False,
    },
    "world-bounds-failure": {
        "restricted_zone": True,
        "world_bounds": False,
        "safe_fallback": True,
        "valid_state_transitions": True,
    },
}
DEFAULT_SEEDS = (42, 1337, 2026)
DEFAULT_VARIANTS = ("standard", "short-stall", "fault-stress")
MAX_PROCESS_OUTPUT_BYTES = 64 * 1024


class CampaignError(Exception):
    """An infrastructure failure that prevents a reliable campaign result."""


def parse_seeds(value: str) -> tuple[int, ...]:
    try:
        seeds = tuple(int(part.strip(), 10) for part in value.split(","))
    except ValueError as error:
        raise argparse.ArgumentTypeError("seeds must be comma-separated integers") from error
    if not seeds or any(seed < 0 or seed > 2**64 - 1 for seed in seeds):
        raise argparse.ArgumentTypeError("seeds must be non-negative u64 values")
    if len(set(seeds)) != len(seeds):
        raise argparse.ArgumentTypeError("seeds must not contain duplicates")
    if len(seeds) > 32:
        raise argparse.ArgumentTypeError("campaigns are limited to 32 seeds")
    return seeds


def binary_default() -> Path:
    configured = (
        os.environ.get("VV_LAB_BINARY")
        or os.environ.get("CARGO_BIN_EXE_VV_LAB")
        or os.environ.get("CARGO_BIN_EXE_vv-lab")
    )
    if configured:
        return Path(configured)
    suffix = ".exe" if os.name == "nt" else ""
    return ROOT / "target" / "release" / f"vv-lab{suffix}"


def verify_fixture_checksums() -> dict[str, str]:
    manifest_path = ROOT / "scenarios" / "sha256.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise CampaignError(f"cannot read scenario checksum manifest: {error}") from error
    if not isinstance(manifest, dict) or set(manifest) != set(EXPECTED_STATUS):
        raise CampaignError("scenario checksum manifest does not match the campaign fixtures")

    actual: dict[str, str] = {}
    for name in EXPECTED_STATUS:
        scenario_path = ROOT / "scenarios" / f"{name}.json"
        try:
            contents = scenario_path.read_bytes().replace(b"\r\n", b"\n")
        except OSError as error:
            raise CampaignError(f"cannot read scenario fixture {scenario_path.name}: {error}") from error
        digest = hashlib.sha256(contents).hexdigest()
        actual[name] = digest
        if manifest[name] != digest:
            raise CampaignError(f"scenario checksum mismatch for {scenario_path.name}")
    return actual


def load_canonical_output_hashes() -> dict[str, dict[str, str]]:
    manifest_path = ROOT / "scenarios" / "canonical-seed-42.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise CampaignError(f"cannot read canonical output manifest: {error}") from error
    if not isinstance(manifest, dict) or set(manifest) != set(EXPECTED_STATUS):
        raise CampaignError("canonical output manifest does not match the campaign fixtures")
    for name, hashes in manifest.items():
        if set(hashes) != {"events_sha256", "report_sha256"}:
            raise CampaignError(f"canonical output hashes are incomplete for {name}")
    return manifest


def check_run_result(
    return_code: int | None,
    report: dict[str, Any],
    artifact: dict[str, Any],
    expected_status: str,
    expected_invariants: dict[str, bool],
) -> list[str]:
    errors: list[str] = []
    expected_code = 0 if expected_status == "passed" else 2
    if return_code == 1 or return_code is None:
        errors.append(f"run command failed with exit code {return_code}")
    elif return_code != expected_code:
        errors.append(f"expected exit code {expected_code}, received {return_code}")
    if report.get("status") != expected_status:
        errors.append(
            f"expected report status {expected_status!r}, received {report.get('status')!r}"
        )
    events = artifact.get("events")
    if not isinstance(events, list):
        errors.append("events artifact has no event list")
    else:
        if artifact.get("event_count") != len(events):
            errors.append("event_count does not match the events array")
        if report.get("event_count") != len(events):
            errors.append("report event_count does not match the events array")
        if artifact.get("expected_report") != report:
            errors.append("events artifact reference report differs from report.json")
    if artifact.get("final_hash") != report.get("final_hash"):
        errors.append("report final_hash differs from events artifact")
    invariant_results = report.get("invariants")
    if not isinstance(invariant_results, list):
        errors.append("report has no invariant results")
    else:
        actual_invariants = {
            result.get("name"): result.get("passed")
            for result in invariant_results
            if isinstance(result, dict)
        }
        if set(actual_invariants) != set(INVARIANT_NAMES):
            errors.append("report invariant names do not match the required set")
        for name, expected_passed in expected_invariants.items():
            if actual_invariants.get(name) is not expected_passed:
                errors.append(
                    f"invariant {name} expected passed={expected_passed}, "
                    f"received {actual_invariants.get(name)!r}"
                )
    return errors


def summarize_invariants(
    cases: list[dict[str, Any]],
) -> tuple[dict[str, dict[str, int]], dict[str, list[dict[str, Any]]]]:
    failures_by_invariant: dict[str, list[dict[str, Any]]] = {
        name: [] for name in INVARIANT_NAMES
    }
    invariant_counts = {
        name: {
            "checks": 0,
            "passed": 0,
            "failed": 0,
            "expected_failures": 0,
            "unexpected_failures": 0,
        }
        for name in INVARIANT_NAMES
    }
    for case in cases:
        actual = case.get("invariants", {})
        expected = case.get("expected_invariants")
        if expected is None:
            expected = EXPECTED_INVARIANTS.get(case["scenario"], {})
        for name in INVARIANT_NAMES:
            if name not in actual:
                continue
            passed = actual[name] is True
            counts = invariant_counts[name]
            counts["checks"] += 1
            counts["passed" if passed else "failed"] += 1
            expected_failure = expected.get(name) is False
            if not passed:
                counts[
                    "expected_failures" if expected_failure else "unexpected_failures"
                ] += 1
                failures_by_invariant[name].append(
                    {
                        "scenario": case["scenario"],
                        "variant": case.get("variant"),
                        "seed": case["seed"],
                        "expected_failure": expected_failure,
                        "evidence": case.get("invariant_failure_evidence", {}).get(name, []),
                        "report_path": case.get("output_paths", {}).get("report"),
                    }
                )
    return invariant_counts, failures_by_invariant


def load_json(path: Path, description: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise CampaignError(f"cannot read {description} at {path.name}: {error}") from error
    if not isinstance(value, dict):
        raise CampaignError(f"{description} at {path} must be a JSON object")
    return value


def run_process(
    command: list[str],
    description: str,
    *,
    timeout_seconds: float = 30,
    max_output_bytes: int = MAX_PROCESS_OUTPUT_BYTES,
) -> subprocess.CompletedProcess[str]:
    try:
        return run_bounded_process(
            command,
            cwd=ROOT,
            timeout_seconds=timeout_seconds,
            max_output_bytes=max_output_bytes,
            description=description,
        )
    except ProcessLimitError as error:
        raise CampaignError(f"{description} could not complete: {error}") from error


def counted_process(
    command: list[str],
    description: str,
    kind: str,
    counts: dict[str, dict[str, int]],
    *,
    timeout_seconds: float,
) -> subprocess.CompletedProcess[str]:
    counts[kind]["attempted"] += 1
    result = run_process(command, description, timeout_seconds=timeout_seconds)
    counts[kind]["completed"] += 1
    return result


def remaining_time(deadline: float, maximum: float = 30) -> float:
    return min(maximum, deadline - time.monotonic())


def campaign_size(budget: CampaignArtifactBudget, case_dir: Path) -> int:
    try:
        return budget.refresh(case_dir)
    except ProcessLimitError as error:
        raise CampaignError(str(error)) from error


def portable_path(path: Path, root: Path) -> str:
    try:
        return path.relative_to(root).as_posix()
    except ValueError:
        return path.name


def unique_campaign_dir(base: Path) -> Path:
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    for suffix in range(100):
        extra = f"-{suffix}" if suffix else ""
        candidate = base / f"campaign-{stamp}-{os.getpid()}{extra}"
        try:
            candidate.mkdir(parents=True, exist_ok=False)
            return candidate
        except FileExistsError:
            continue
    raise CampaignError(f"cannot create a unique campaign directory below {base}")


def execute_case(
    binary: Path,
    campaign_dir: Path,
    scenario_name: str,
    seed: int,
    canonical_hashes: dict[str, dict[str, str]],
    *,
    deadline: float,
    artifact_budget: CampaignArtifactBudget,
    command_counts: dict[str, dict[str, int]],
) -> dict[str, Any]:
    started = time.monotonic()
    expected = EXPECTED_STATUS[scenario_name]
    fixture_path = ROOT / "scenarios" / f"{scenario_name}.json"
    case_dir = campaign_dir / scenario_name / f"seed-{seed}"
    first_dir = case_dir / "first"
    second_dir = case_dir / "repeat"
    case_dir.mkdir(parents=True, exist_ok=False)

    def run_command(output_dir: Path) -> list[str]:
        remaining = artifact_budget.remaining(case_dir)
        if remaining < 1:
            raise CampaignError("campaign reached the artifact byte limit")
        return [
            str(binary),
            "run",
            str(fixture_path),
            "--seed",
            str(seed),
            "--output",
            str(output_dir),
            "--max-output-bytes",
            str(remaining),
        ]

    first = counted_process(
        run_command(first_dir),
        f"run {scenario_name} with seed {seed}",
        "run",
        command_counts,
        timeout_seconds=remaining_time(deadline),
    )
    campaign_size(artifact_budget, case_dir)
    report_path = first_dir / "report.json"
    artifact_path = first_dir / "events.json"
    mission_path = first_dir / "mission.json"
    report = load_json(report_path, "run report")
    artifact = load_json(artifact_path, "event artifact")
    mission = load_json(mission_path, "mission analysis")
    expected_invariants = EXPECTED_INVARIANTS[scenario_name]
    errors = check_run_result(
        first.returncode, report, artifact, expected, expected_invariants
    )
    mission_policy = {"stall_window_ticks": 8, "min_progress_mm": 1}
    errors.extend(check_mission_sidecar(mission, artifact, report, mission_policy, None))

    repeated = counted_process(
        run_command(second_dir),
        f"repeat {scenario_name} with seed {seed}",
        "run",
        command_counts,
        timeout_seconds=remaining_time(deadline),
    )
    campaign_size(artifact_budget, case_dir)
    repeated_report_path = second_dir / "report.json"
    repeated_artifact_path = second_dir / "events.json"
    repeated_mission_path = second_dir / "mission.json"
    repeated_report = load_json(repeated_report_path, "repeat report")
    repeated_artifact = load_json(repeated_artifact_path, "repeat event artifact")
    repeated_mission = load_json(repeated_mission_path, "repeat mission analysis")
    errors.extend(
        check_run_result(
            repeated.returncode,
            repeated_report,
            repeated_artifact,
            expected,
            expected_invariants,
        )
    )
    errors.extend(
        check_mission_sidecar(
            repeated_mission, repeated_artifact, repeated_report, mission_policy, None
        )
    )
    deterministic = (
        report_path.read_bytes() == repeated_report_path.read_bytes()
        and artifact_path.read_bytes() == repeated_artifact_path.read_bytes()
        and mission_path.read_bytes() == repeated_mission_path.read_bytes()
    )
    if not deterministic:
        errors.append("same scenario and seed produced different canonical bytes")
    output_hashes = {
        "events_sha256": hashlib.sha256(artifact_path.read_bytes()).hexdigest(),
        "report_sha256": hashlib.sha256(report_path.read_bytes()).hexdigest(),
    }
    if seed == 42 and output_hashes != canonical_hashes[scenario_name]:
        errors.append("seed 42 output differs from the pinned canonical hashes")

    replayed = counted_process(
        [str(binary), "replay", str(artifact_path)],
        f"replay {scenario_name} with seed {seed}",
        "replay",
        command_counts,
        timeout_seconds=remaining_time(deadline),
    )
    if replayed.returncode != 0:
        errors.append(f"replay returned exit code {replayed.returncode}")

    actual_invariants = {
        result["name"]: result["passed"] for result in report.get("invariants", [])
    }
    invariant_evidence = {
        result["name"]: result.get("failures", [])
        for result in report.get("invariants", [])
        if result.get("passed") is False
    }
    return {
        "suite": "m1",
        "scenario": scenario_name,
        "variant": "default",
        "seed": seed,
        "expected_status": expected,
        "actual_status": report.get("status"),
        "expected_invariants": expected_invariants,
        "invariants": actual_invariants,
        "mission_metrics": mission_metrics(mission),
        "expected_outcome": None,
        "invariant_failure_evidence": invariant_evidence,
        "canonical_output_sha256": output_hashes,
        "run_exit_code": first.returncode,
        "repeat_exit_code": repeated.returncode,
        "replay_exit_code": replayed.returncode,
        "deterministic": deterministic,
        "result": "passed" if not errors else "failed",
        "errors": errors,
        "elapsed_seconds": round(time.monotonic() - started, 4),
        "output_paths": {
            "events": portable_path(artifact_path, campaign_dir),
            "report": portable_path(report_path, campaign_dir),
            "mission": portable_path(mission_path, campaign_dir),
            "repeat_events": portable_path(repeated_artifact_path, campaign_dir),
            "repeat_report": portable_path(repeated_report_path, campaign_dir),
            "repeat_mission": portable_path(repeated_mission_path, campaign_dir),
        },
    }


def execute_m2_case(
    binary: Path,
    campaign_dir: Path,
    entry: dict[str, Any],
    variant_name: str,
    seed: int,
    policy: dict[str, int],
    expected_outcome: str,
    scenario_overrides: dict[str, int],
    *,
    deadline: float,
    artifact_budget: CampaignArtifactBudget,
    command_counts: dict[str, dict[str, int]],
) -> dict[str, Any]:
    started = time.monotonic()
    scenario_name = entry["name"]
    fixture_path = ROOT / "scenarios" / "m2" / entry["file"]
    case_dir = campaign_dir / scenario_name / variant_name / f"seed-{seed}"
    first_dir = case_dir / "first"
    second_dir = case_dir / "repeat"
    analysis_dir = case_dir / "analysis"
    case_dir.mkdir(parents=True, exist_ok=False)
    scenario_data = scenario_for_variant(entry, scenario_overrides)
    scenario_bytes = (json.dumps(scenario_data, indent=2, sort_keys=True) + "\n").encode(
        "utf-8"
    )
    scenario_copy = case_dir / "scenario.json"
    scenario_copy.write_bytes(scenario_bytes)
    policy_args = [
        "--stall-window-ticks",
        str(policy["stall_window_ticks"]),
        "--min-progress-mm",
        str(policy["min_progress_mm"]),
    ]

    def run_args(output_dir: Path) -> list[str]:
        remaining = artifact_budget.remaining(case_dir)
        if remaining < 1:
            raise CampaignError("campaign reached the artifact byte limit")
        return [
            str(binary),
            "run",
            str(scenario_copy),
            "--seed",
            str(seed),
            "--output",
            str(output_dir),
            *policy_args,
            "--max-output-bytes",
            str(remaining),
        ]

    first = counted_process(
        run_args(first_dir),
        f"run {scenario_name}/{variant_name}, seed {seed}",
        "run",
        command_counts,
        timeout_seconds=remaining_time(deadline),
    )
    campaign_size(artifact_budget, case_dir)
    report_path = first_dir / "report.json"
    artifact_path = first_dir / "events.json"
    mission_path = first_dir / "mission.json"
    report = load_json(report_path, "relatório M2")
    artifact = load_json(artifact_path, "artefacto de eventos M2")
    mission = load_json(mission_path, "análise da missão")

    from m2_campaign import M2_INVARIANT_NAMES

    errors = check_run_result(
        first.returncode,
        report,
        artifact,
        entry["expected_status"],
        entry["expected_invariants"],
    )
    errors.extend(
        check_mission_sidecar(mission, artifact, report, policy, expected_outcome)
    )
    actual_invariants = {
        result["name"]: result["passed"]
        for result in report.get("invariants", [])
        if isinstance(result, dict) and result.get("name") in M2_INVARIANT_NAMES
    }
    primary_invariant = entry.get("expected_primary_invariant")
    if primary_invariant:
        failed = [name for name, passed in actual_invariants.items() if not passed]
        if failed != [primary_invariant]:
            errors.append(
                f"expected only {primary_invariant} to fail, received {failed}"
            )
    if entry.get("requires_observation_discrepancy") and primary_invariant:
        if not observation_discrepancy(report, artifact, primary_invariant):
            errors.append(
                f"{primary_invariant} failure has no sensor-to-truth discrepancy evidence"
            )

    repeat = counted_process(
        run_args(second_dir),
        f"repeat {scenario_name}/{variant_name}, seed {seed}",
        "run",
        command_counts,
        timeout_seconds=remaining_time(deadline),
    )
    campaign_size(artifact_budget, case_dir)
    repeated_report_path = second_dir / "report.json"
    repeated_artifact_path = second_dir / "events.json"
    repeated_mission_path = second_dir / "mission.json"
    repeated_report = load_json(repeated_report_path, "relatório repetido M2")
    repeated_artifact = load_json(repeated_artifact_path, "eventos repetidos M2")
    repeated_mission = load_json(repeated_mission_path, "análise repetida da missão")
    errors.extend(
        check_run_result(
            repeat.returncode,
            repeated_report,
            repeated_artifact,
            entry["expected_status"],
            entry["expected_invariants"],
        )
    )
    errors.extend(
        check_mission_sidecar(
            repeated_mission,
            repeated_artifact,
            repeated_report,
            policy,
            expected_outcome,
        )
    )
    deterministic = (
        report_path.read_bytes() == repeated_report_path.read_bytes()
        and artifact_path.read_bytes() == repeated_artifact_path.read_bytes()
        and mission_path.read_bytes() == repeated_mission_path.read_bytes()
    )
    if not deterministic:
        errors.append("a repetição produziu bytes diferentes para a mesma entrada")

    replayed = counted_process(
        [str(binary), "replay", str(artifact_path)],
        f"replay {scenario_name}/{variant_name}, seed {seed}",
        "replay",
        command_counts,
        timeout_seconds=remaining_time(deadline),
    )
    if replayed.returncode != 0:
        errors.append(f"replay terminou com o código {replayed.returncode}")

    analysis_dir.mkdir(parents=True, exist_ok=False)
    analysis_path = analysis_dir / "mission.json"
    remaining = artifact_budget.remaining(case_dir)
    if remaining < 1:
        raise CampaignError("campaign reached the artifact byte limit")
    analyzed = counted_process(
        [
            str(binary),
            "analyze",
            str(artifact_path),
            "--output",
            str(analysis_path),
            *policy_args,
            "--max-output-bytes",
            str(remaining),
        ],
        f"analyze {scenario_name}/{variant_name}, seed {seed}",
        "analyze",
        command_counts,
        timeout_seconds=remaining_time(deadline),
    )
    campaign_size(artifact_budget, case_dir)
    if analyzed.returncode != 0:
        errors.append(f"analyze terminou com o código {analyzed.returncode}")
    analyzed_mission = load_json(analysis_path, "resultado de analyze")
    errors.extend(
        check_mission_sidecar(
            analyzed_mission,
            artifact,
            report,
            policy,
            expected_outcome,
        )
    )
    analysis_matches = mission_path.read_bytes() == analysis_path.read_bytes()
    if not analysis_matches:
        errors.append("analyze não reproduziu os mesmos bytes de mission.json")

    output_hashes = {
        "scenario_sha256": hashlib.sha256(scenario_bytes).hexdigest(),
        "events_sha256": hashlib.sha256(artifact_path.read_bytes()).hexdigest(),
        "report_sha256": hashlib.sha256(report_path.read_bytes()).hexdigest(),
        "mission_sha256": hashlib.sha256(mission_path.read_bytes()).hexdigest(),
    }
    invariant_evidence = {
        result["name"]: result.get("failures", [])
        for result in report.get("invariants", [])
        if isinstance(result, dict) and result.get("passed") is False
    }
    return {
        "suite": "m2",
        "scenario": scenario_name,
        "variant": variant_name,
        "seed": seed,
        "policy": policy,
        "expected_status": entry["expected_status"],
        "actual_status": report.get("status"),
        "expected_outcome": expected_outcome,
        "mission_metrics": mission_metrics(mission),
        "expected_invariants": entry["expected_invariants"],
        "invariants": actual_invariants,
        "invariant_failure_evidence": invariant_evidence,
        "canonical_output_sha256": output_hashes,
        "run_exit_code": first.returncode,
        "repeat_exit_code": repeat.returncode,
        "replay_exit_code": replayed.returncode,
        "analyze_exit_code": analyzed.returncode,
        "deterministic": deterministic,
        "analysis_matches_run": analysis_matches,
        "result": "passed" if not errors else "failed",
        "errors": errors,
        "elapsed_seconds": round(time.monotonic() - started, 4),
        "output_paths": {
            "scenario": portable_path(scenario_copy, campaign_dir),
            "events": portable_path(artifact_path, campaign_dir),
            "report": portable_path(report_path, campaign_dir),
            "mission": portable_path(mission_path, campaign_dir),
            "repeat_events": portable_path(repeated_artifact_path, campaign_dir),
            "repeat_report": portable_path(repeated_report_path, campaign_dir),
            "repeat_mission": portable_path(repeated_mission_path, campaign_dir),
            "analyzed_mission": portable_path(analysis_path, campaign_dir),
        },
    }
def parse_variants(value: str) -> tuple[str, ...]:
    variants = tuple(part.strip() for part in value.split(","))
    if not variants or any(not name for name in variants):
        raise argparse.ArgumentTypeError("variants must be comma-separated names")
    if len(set(variants)) != len(variants):
        raise argparse.ArgumentTypeError("variants must not contain duplicates")
    if len(variants) > 16:
        raise argparse.ArgumentTypeError("campaigns are limited to 16 variants")
    return variants


def parse_m3_selection(value: str) -> tuple[str, ...]:
    names = tuple(part.strip() for part in value.split(","))
    if not names or any(not name for name in names):
        raise argparse.ArgumentTypeError("M3 selections must be comma-separated names")
    if len(set(names)) != len(names):
        raise argparse.ArgumentTypeError("M3 selections must not contain duplicates")
    if len(names) > 80:
        raise argparse.ArgumentTypeError("M3 selections are limited to 80 names")
    return names


def parse_positive_int(value: str) -> int:
    try:
        parsed = int(value, 10)
    except ValueError as error:
        raise argparse.ArgumentTypeError("value must be an integer") from error
    if parsed < 1 or parsed > 10000:
        raise argparse.ArgumentTypeError("value must be between 1 and 10000")
    return parsed


def parse_positive_bytes(value: str) -> int:
    try:
        parsed = int(value, 10)
    except ValueError as error:
        raise argparse.ArgumentTypeError("artifact limit must be an integer") from error
    if parsed < 1 or parsed > 2 * 1024 * 1024 * 1024:
        raise argparse.ArgumentTypeError("artifact limit must be between 1 and 2 GiB")
    return parsed


def parse_time_budget(value: str) -> float:
    try:
        parsed = float(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("time budget must be a number") from error
    if not math.isfinite(parsed) or parsed <= 0 or parsed > 3600:
        raise argparse.ArgumentTypeError("time budget must be between 0 and 3600 seconds")
    return parsed


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=binary_default())
    parser.add_argument("--seeds", type=parse_seeds, default=DEFAULT_SEEDS)
    parser.add_argument("--suite", choices=("m1", "m2", "m3", "all"), default="all")
    parser.add_argument("--variants", type=parse_variants, default=DEFAULT_VARIANTS)
    parser.add_argument("--m2-scenarios", type=parse_variants)
    parser.add_argument("--m3-manifest", type=Path)
    parser.add_argument("--m3-contract-sets", type=parse_m3_selection)
    parser.add_argument("--m3-scenarios", type=parse_m3_selection)
    parser.add_argument("--m3-fault-combinations", type=parse_m3_selection)
    parser.add_argument("--max-runs", type=parse_positive_int, default=1000)
    parser.add_argument("--max-cases", type=parse_positive_int, default=256)
    parser.add_argument("--max-artifact-bytes", type=parse_positive_bytes, default=256 * 1024 * 1024)
    parser.add_argument("--time-budget-seconds", type=parse_time_budget, default=600)
    parser.add_argument("--output-root", type=Path, default=ROOT / "runs")
    return parser


def main() -> int:
    args = make_parser().parse_args()
    if args.suite == "m3":
        from m3_campaign import M3CampaignError, run_campaign as run_m3_campaign
        from process_limits import ProcessLimitError

        try:
            return run_m3_campaign(args)
        except (M3CampaignError, ProcessLimitError) as error:
            print(f"M3 campaign failed: {error}", file=sys.stderr)
            return 1
    from campaign_matrix import run_campaign

    return run_campaign(args)


if __name__ == "__main__":
    raise SystemExit(main())
