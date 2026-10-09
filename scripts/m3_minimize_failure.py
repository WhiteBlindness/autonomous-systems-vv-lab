#!/usr/bin/env python3
"""Reduce an M3 scenario horizon while preserving its temporal failure."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

from m3_failure_signature import (
    M3FailureSignatureError,
    counterexample_actuator_outcomes,
    same_temporal_failure,
    temporal_failure_signature,
)
from process_limits import ProcessLimitError, ensure_tree_size, run_bounded_process


ROOT = Path(__file__).resolve().parents[1]
MAX_CANDIDATES = 32
MAX_OUTPUT_BYTES = 16 * 1024 * 1024
MAX_TEMP_BYTES = 32 * 1024 * 1024
MAX_TIME_SECONDS = 60
MAX_INPUT_BYTES = 2 * 1024 * 1024
RUN_ARTIFACTS = ("events.json", "report.json", "mission.json", "verification.json")


class M3MinimizationError(ValueError):
    """Invalid input or a temporal counterexample that could not be reduced."""


def parse_seed(value: str) -> int:
    if not re.fullmatch(r"[0-9]{1,20}", value):
        raise argparse.ArgumentTypeError("seed must be an unsigned 64-bit integer")
    try:
        seed = int(value, 10)
    except ValueError as error:
        raise argparse.ArgumentTypeError("seed must be an unsigned 64-bit integer") from error
    if not value or value.strip() != value or seed < 0 or seed > 2**64 - 1:
        raise argparse.ArgumentTypeError("seed must be an unsigned 64-bit integer")
    return seed


def parse_positive_int(value: str) -> int:
    try:
        parsed = int(value, 10)
    except ValueError as error:
        raise argparse.ArgumentTypeError("value must be a positive integer") from error
    if parsed <= 0:
        raise argparse.ArgumentTypeError("value must be a positive integer")
    return parsed


def parse_positive_float(value: str) -> float:
    try:
        parsed = float(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("time budget must be finite and positive") from error
    if not math.isfinite(parsed) or parsed <= 0 or parsed > MAX_TIME_SECONDS:
        raise argparse.ArgumentTypeError(f"time budget must be in (0, {MAX_TIME_SECONDS:g}]")
    return parsed


def _read_json(path: Path, label: str) -> tuple[dict[str, Any], bytes]:
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise M3MinimizationError(f"cannot read {label}: {error}") from error
    if len(raw) > MAX_INPUT_BYTES:
        raise M3MinimizationError(f"{label} exceeds the {MAX_INPUT_BYTES}-byte input limit")
    try:
        parsed = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise M3MinimizationError(f"{label} is not valid JSON: {error}") from error
    if not isinstance(parsed, dict):
        raise M3MinimizationError(f"{label} must be a JSON object")
    return parsed, raw


def _load_run(
    output: Path, max_output_bytes: int = MAX_OUTPUT_BYTES
) -> tuple[dict[str, bytes], dict[str, dict[str, Any]]]:
    try:
        total = sum((output / name).stat().st_size for name in RUN_ARTIFACTS)
    except OSError as error:
        raise M3MinimizationError(f"run is missing an expected artifact: {error}") from error
    if total > max_output_bytes:
        raise M3MinimizationError("run artifacts exceed the M3 output limit")
    raw: dict[str, bytes] = {}
    parsed: dict[str, dict[str, Any]] = {}
    for name in RUN_ARTIFACTS:
        path = output / name
        try:
            contents = path.read_bytes()
            value = json.loads(contents)
        except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
            raise M3MinimizationError(f"run did not produce valid {name}: {error}") from error
        if not isinstance(value, dict):
            raise M3MinimizationError(f"{name} must contain a JSON object")
        raw[name] = contents
        parsed[name.removesuffix(".json")] = value
    artifact = parsed["events"]
    if artifact.get("expected_report") != parsed["report"]:
        raise M3MinimizationError("events.json expected_report differs from report.json")
    if artifact.get("final_hash") != parsed["report"].get("final_hash"):
        raise M3MinimizationError("events.json final hash differs from report.json")
    if parsed["verification"].get("source_artifact_sha256") != artifact.get("artifact_sha256"):
        raise M3MinimizationError("verification.json references a different events artifact")
    return raw, parsed


def _failed_contract(
    verification: dict[str, Any], contract_id: str
) -> tuple[dict[str, Any], dict[str, Any], list[dict[str, Any]]]:
    results = verification.get("contract_results")
    failures = verification.get("failures")
    if not isinstance(results, list) or not isinstance(failures, list):
        raise M3MinimizationError("verification.json is missing contract results or failures")
    matches = [
        result
        for result in results
        if isinstance(result, dict) and result.get("contract_id") == contract_id
    ]
    failure_matches = [
        failure
        for failure in failures
        if isinstance(failure, dict) and failure.get("contract_id") == contract_id
    ]
    if len(matches) != 1 or len(failure_matches) != 1 or matches[0].get("outcome") != "FAIL":
        raise M3MinimizationError(f"contract {contract_id!r} has no unique failed evidence")
    result = matches[0]
    failure = failure_matches[0]
    normalized = {
        **result,
        "deadline_tick": result.get("deadline_tick")
        if result.get("deadline_tick") is not None
        else failure.get("applicable_deadline_tick"),
        "first_trigger_tick": result.get("first_trigger_tick")
        if result.get("first_trigger_tick") is not None
        else failure.get("first_trigger_tick"),
        "first_violation_tick": result.get("first_violation_tick")
        if result.get("first_violation_tick") is not None
        else failure.get("first_violation_tick"),
    }
    try:
        observed = counterexample_actuator_outcomes(failure)
    except M3FailureSignatureError as error:
        raise M3MinimizationError(str(error)) from error
    return normalized, failure, observed


def _signature(verification: dict[str, Any], contract_id: str) -> tuple[dict[str, Any], dict[str, Any]]:
    result, failure, actuator_rows = _failed_contract(verification, contract_id)
    try:
        signature = temporal_failure_signature(result, actuator_rows)
    except M3FailureSignatureError as error:
        raise M3MinimizationError(str(error)) from error
    return signature, failure


def _command_run(
    binary: Path,
    scenario: Path,
    contracts: Path,
    actuator: Path,
    seed: int,
    output: Path,
    max_output_bytes: int,
) -> list[str]:
    return [
        str(binary),
        "run",
        str(scenario),
        "--seed",
        str(seed),
        "--output",
        str(output),
        "--contracts",
        str(contracts),
        "--actuator",
        str(actuator),
        "--max-output-bytes",
        str(max_output_bytes),
    ]


def _execute(
    command: list[str],
    *,
    deadline: float,
    max_stdout_bytes: int = 64 * 1024,
    label: str,
):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise ProcessLimitError("M3 minimizer exceeded its time budget")
    return run_bounded_process(
        command,
        cwd=ROOT,
        timeout_seconds=min(30.0, remaining),
        max_output_bytes=max_stdout_bytes,
        description=f"M3 minimizer {label}",
    )


def _run_candidate(
    *,
    binary: Path,
    scenario: dict[str, Any],
    contracts: Path,
    actuator: Path,
    seed: int,
    output: Path,
    contract_id: str,
    max_output_bytes: int,
    deadline: float,
    temp_root: Path,
) -> tuple[dict[str, bytes], dict[str, dict[str, Any]], dict[str, Any], dict[str, Any]]:
    output.mkdir()
    candidate_path = temp_root / "candidate-scenario.json"
    candidate_path.write_text(
        json.dumps(scenario, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    run = _execute(
        _command_run(binary, candidate_path, contracts, actuator, seed, output, max_output_bytes),
        deadline=deadline,
        label="run",
    )
    if run.returncode not in (0, 2):
        raise M3MinimizationError(f"vv-lab run returned unexpected status {run.returncode}")
    raw, parsed = _load_run(output, max_output_bytes)
    signature, failure = _signature(parsed["verification"], contract_id)
    replay = _execute(
        [str(binary), "replay", str(output / "events.json")],
        deadline=deadline,
        label="replay",
    )
    if replay.returncode != 0:
        raise M3MinimizationError(f"vv-lab replay returned {replay.returncode}")
    return raw, parsed, signature, failure


def _tree_hashes(directory: Path) -> dict[str, str]:
    result = {}
    for path in sorted(item for item in directory.rglob("*") if item.is_file()):
        result[path.relative_to(directory).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        description="Reduce an M3 scenario horizon while preserving a temporal failure."
    )
    result.add_argument("scenario", type=Path)
    result.add_argument("--contracts", required=True, type=Path)
    result.add_argument("--actuator", required=True, type=Path)
    result.add_argument("--contract-id", default="actuator_stop")
    result.add_argument("--seed", required=True, type=parse_seed)
    result.add_argument("--output", required=True, type=Path)
    result.add_argument("--binary", type=Path)
    result.add_argument("--max-candidates", type=parse_positive_int, default=8)
    result.add_argument("--time-budget-seconds", type=parse_positive_float, default=30.0)
    result.add_argument("--max-output-bytes", type=parse_positive_int, default=MAX_OUTPUT_BYTES)
    result.add_argument("--max-temp-bytes", type=parse_positive_int, default=MAX_TEMP_BYTES)
    return result


def execute(args: argparse.Namespace) -> int:
    if args.max_candidates > MAX_CANDIDATES:
        raise M3MinimizationError(f"--max-candidates cannot exceed {MAX_CANDIDATES}")
    if args.max_output_bytes > MAX_OUTPUT_BYTES:
        raise M3MinimizationError(f"--max-output-bytes cannot exceed {MAX_OUTPUT_BYTES}")
    if args.max_temp_bytes > MAX_TEMP_BYTES:
        raise M3MinimizationError(f"--max-temp-bytes cannot exceed {MAX_TEMP_BYTES}")

    started = time.monotonic()
    deadline = started + args.time_budget_seconds
    if args.binary is not None:
        binary = args.binary
    elif os.environ.get("VV_LAB_BINARY"):
        binary = Path(os.environ["VV_LAB_BINARY"])
    else:
        names = ("vv-lab.exe", "vv-lab") if os.name == "nt" else ("vv-lab", "vv-lab.exe")
        binary = next(
            (
                ROOT / "target" / configuration / name
                for configuration in ("release", "debug")
                for name in names
                if (ROOT / "target" / configuration / name).is_file()
            ),
            ROOT / "target" / "release" / names[0],
        )
    binary = binary.expanduser().resolve()
    scenario_path = args.scenario.expanduser().resolve()
    contracts = args.contracts.expanduser().resolve()
    actuator = args.actuator.expanduser().resolve()
    for label, path in (("binary", binary), ("scenario", scenario_path), ("contracts", contracts), ("actuator", actuator)):
        if not path.is_file():
            raise M3MinimizationError(f"{label} file does not exist: {path.name}")
    scenario, scenario_raw = _read_json(scenario_path, "scenario")
    _contract_set, contracts_raw = _read_json(contracts, "contract set")
    _actuator_config, actuator_raw = _read_json(actuator, "actuator config")
    steps = scenario.get("steps")
    if type(steps) is not int or steps < 1:
        raise M3MinimizationError("scenario steps must be a positive integer")

    output_root = args.output.expanduser().resolve()
    if output_root.exists() and (not output_root.is_dir() or any(output_root.iterdir())):
        raise M3MinimizationError("output directory must be empty")
    output_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="m3-minimize-", dir=output_root) as temporary:
        temp_root = Path(temporary)
        original_run_dir = temp_root / "original-run"
        original_scenario = json.loads(scenario_raw)
        original_raw, original_parsed, original_signature, original_failure = _run_candidate(
            binary=binary,
            scenario=original_scenario,
            contracts=contracts,
            actuator=actuator,
            seed=args.seed,
            output=original_run_dir,
            contract_id=args.contract_id,
            max_output_bytes=args.max_output_bytes,
            deadline=deadline,
            temp_root=temp_root,
        )
        del original_raw
        ensure_tree_size(temp_root, args.max_temp_bytes, "temporary M3 minimization data")

        violation = original_signature["violation_tick"]
        candidate_horizons = range(max(1, violation), steps)
        attempted = 0
        selected = None
        for horizon in candidate_horizons:
            if attempted >= args.max_candidates:
                break
            attempted += 1
            candidate = json.loads(scenario_raw)
            candidate["steps"] = horizon
            candidate_dir = temp_root / f"candidate-{attempted}"
            try:
                candidate_raw, candidate_parsed, candidate_signature, candidate_failure = _run_candidate(
                    binary=binary,
                    scenario=candidate,
                    contracts=contracts,
                    actuator=actuator,
                    seed=args.seed,
                    output=candidate_dir,
                    contract_id=args.contract_id,
                    max_output_bytes=args.max_output_bytes,
                    deadline=deadline,
                    temp_root=temp_root,
                )
            except M3MinimizationError:
                ensure_tree_size(temp_root, args.max_temp_bytes, "temporary M3 minimization data")
                continue
            ensure_tree_size(temp_root, args.max_temp_bytes, "temporary M3 minimization data")
            try:
                normalized_result, _evidence, observed_actuation = _failed_contract(
                    candidate_parsed["verification"], args.contract_id
                )
            except M3MinimizationError:
                continue
            if not same_temporal_failure(original_signature, normalized_result, observed_actuation):
                continue
            selected = (candidate, candidate_raw, candidate_parsed, candidate_signature, candidate_failure)
            break

        if selected is None:
            raise M3MinimizationError(
                f"no reduced horizon preserved {args.contract_id} within {attempted} candidates"
            )

        reduced_scenario, reduced_raw, reduced_parsed, reduced_signature, reduced_failure = selected
        original_dir = output_root / "original"
        reduced_dir = output_root / "reduced"
        original_dir.mkdir()
        reduced_dir.mkdir()
        for name, contents in _load_run(original_run_dir)[0].items():
            (original_dir / name).write_bytes(contents)
        for name, contents in reduced_raw.items():
            (reduced_dir / name).write_bytes(contents)
        (original_dir / "scenario.json").write_bytes(scenario_raw)
        (original_dir / "contracts.json").write_bytes(contracts_raw)
        (original_dir / "actuator.json").write_bytes(actuator_raw)
        reduced_scenario_bytes = (
            json.dumps(reduced_scenario, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
        ).encode("utf-8")
        (reduced_dir / "scenario.json").write_bytes(reduced_scenario_bytes)
        (reduced_dir / "contracts.json").write_bytes(contracts_raw)
        (reduced_dir / "actuator.json").write_bytes(actuator_raw)

        summary = {
            "schema_version": 1,
            "suite": "m3_minimization",
            "status": "reduced",
            "contract_id": args.contract_id,
            "seed": args.seed,
            "original_steps": steps,
            "reduced_steps": reduced_scenario["steps"],
            "candidates_attempted": attempted,
            "max_candidates": args.max_candidates,
            "time_budget_seconds": args.time_budget_seconds,
            "elapsed_seconds": round(time.monotonic() - started, 4),
            "max_output_bytes": args.max_output_bytes,
            "max_temp_bytes": args.max_temp_bytes,
            "original_signature": original_signature,
            "reduced_signature": reduced_signature,
            "original_failure_evidence": original_failure,
            "reduced_failure_evidence": reduced_failure,
            "original_artifact_sha256": _tree_hashes(original_dir),
            "reduced_artifact_sha256": _tree_hashes(reduced_dir),
            "source_hashes": {
                "scenario_sha256": hashlib.sha256(scenario_raw).hexdigest(),
                "contracts_sha256": hashlib.sha256(contracts_raw).hexdigest(),
                "actuator_sha256": hashlib.sha256(actuator_raw).hexdigest(),
            },
            "reproduction_commands": {
                "original_run": _command_run(
                    binary, scenario_path, contracts, actuator, args.seed,
                    output_root / "original-replay", args.max_output_bytes,
                ),
                "original_replay": [str(binary), "replay", str(output_root / "original" / "events.json")],
                "reduced_run": _command_run(
                    binary, output_root / "reduced" / "scenario.json", contracts, actuator,
                    args.seed, output_root / "reduced-replay", args.max_output_bytes,
                ),
                "reduced_replay": [str(binary), "replay", str(output_root / "reduced" / "events.json")],
            },
            "source_artifact_sha256": original_parsed["events"].get("artifact_sha256"),
            "reduced_source_artifact_sha256": reduced_parsed["events"].get("artifact_sha256"),
            "first_violation_tick": original_signature["violation_tick"],
            "deadline_tick": original_signature["deadline_tick"],
            "trigger_tick": original_signature["trigger_tick"],
            "minimum_evidence": {
                "original_ticks": [tick["tick"] for tick in original_failure["counterexample_ticks"]],
                "reduced_ticks": [tick["tick"] for tick in reduced_failure["counterexample_ticks"]],
            },
        }
        (output_root / "summary.json").write_text(
            json.dumps(summary, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    ensure_tree_size(output_root, args.max_output_bytes, "retained M3 minimization artifacts")
    print(
        f"M3 temporal counterexample reduced: {steps} to {summary['reduced_steps']} steps; "
        f"{attempted} candidates."
    )
    return 0


def main() -> int:
    args = parser().parse_args()
    try:
        return execute(args)
    except (M3MinimizationError, M3FailureSignatureError, ProcessLimitError, OSError) as error:
        print(f"M3 minimization failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
