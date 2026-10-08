#!/usr/bin/env python3
"""Run a bounded, replay-checked scenario campaign using only the Python stdlib."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


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
            raise CampaignError(f"cannot read scenario fixture {scenario_path}: {error}") from error
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
        expected = case.get(
            "expected_invariants", EXPECTED_INVARIANTS[case["scenario"]]
        )
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
        raise CampaignError(f"cannot read {description} at {path}: {error}") from error
    if not isinstance(value, dict):
        raise CampaignError(f"{description} at {path} must be a JSON object")
    return value


def run_process(command: list[str], description: str) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            command,
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise CampaignError(f"{description} could not complete: {error}") from error


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
) -> dict[str, Any]:
    started = time.monotonic()
    expected = EXPECTED_STATUS[scenario_name]
    fixture_path = ROOT / "scenarios" / f"{scenario_name}.json"
    case_dir = campaign_dir / scenario_name / f"seed-{seed}"
    first_dir = case_dir / "first"
    second_dir = case_dir / "repeat"
    case_dir.mkdir(parents=True, exist_ok=False)

    command = [
        str(binary),
        "run",
        str(fixture_path),
        "--seed",
        str(seed),
        "--output",
        str(first_dir),
    ]
    first = run_process(command, f"run {scenario_name} with seed {seed}")
    report_path = first_dir / "report.json"
    artifact_path = first_dir / "events.json"
    report = load_json(report_path, "run report")
    artifact = load_json(artifact_path, "event artifact")
    expected_invariants = EXPECTED_INVARIANTS[scenario_name]
    errors = check_run_result(
        first.returncode, report, artifact, expected, expected_invariants
    )

    repeat_command = [
        str(binary),
        "run",
        str(fixture_path),
        "--seed",
        str(seed),
        "--output",
        str(second_dir),
    ]
    repeated = run_process(repeat_command, f"repeat {scenario_name} with seed {seed}")
    repeated_report_path = second_dir / "report.json"
    repeated_artifact_path = second_dir / "events.json"
    repeated_report = load_json(repeated_report_path, "repeat report")
    repeated_artifact = load_json(repeated_artifact_path, "repeat event artifact")
    errors.extend(
        check_run_result(
            repeated.returncode,
            repeated_report,
            repeated_artifact,
            expected,
            expected_invariants,
        )
    )
    deterministic = (
        report_path.read_bytes() == repeated_report_path.read_bytes()
        and artifact_path.read_bytes() == repeated_artifact_path.read_bytes()
    )
    if not deterministic:
        errors.append("same scenario and seed produced different canonical bytes")
    output_hashes = {
        "events_sha256": hashlib.sha256(artifact_path.read_bytes()).hexdigest(),
        "report_sha256": hashlib.sha256(report_path.read_bytes()).hexdigest(),
    }
    if seed == 42 and output_hashes != canonical_hashes[scenario_name]:
        errors.append("seed 42 output differs from the pinned canonical hashes")

    replayed = run_process(
        [str(binary), "replay", str(artifact_path)],
        f"replay {scenario_name} with seed {seed}",
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
        "scenario": scenario_name,
        "seed": seed,
        "expected_status": expected,
        "actual_status": report.get("status"),
        "expected_invariants": expected_invariants,
        "invariants": actual_invariants,
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
            "events": str(artifact_path),
            "report": str(report_path),
            "repeat_events": str(repeated_artifact_path),
            "repeat_report": str(repeated_report_path),
        },
    }


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=binary_default())
    parser.add_argument("--seeds", type=parse_seeds, default=DEFAULT_SEEDS)
    parser.add_argument("--output-root", type=Path, default=ROOT / "runs")
    return parser


def main() -> int:
    args = make_parser().parse_args()
    campaign_started = time.monotonic()
    binary = args.binary if args.binary.is_absolute() else ROOT / args.binary
    binary = binary.resolve()
    try:
        if not binary.is_file():
            raise CampaignError(f"compiled vv-lab binary not found: {binary}")
        checksums = verify_fixture_checksums()
        canonical_hashes = load_canonical_output_hashes()
        output_root = args.output_root if args.output_root.is_absolute() else ROOT / args.output_root
        campaign_dir = unique_campaign_dir(output_root.resolve())
        cases = []
        for seed in args.seeds:
            for name in EXPECTED_STATUS:
                try:
                    case_started = time.monotonic()
                    case = execute_case(
                        binary, campaign_dir, name, seed, canonical_hashes
                    )
                    cases.append(case)
                except CampaignError as error:
                    cases.append(
                        {
                            "scenario": name,
                            "seed": seed,
                            "expected_status": EXPECTED_STATUS[name],
                            "result": "failed",
                            "errors": [str(error)],
                            "elapsed_seconds": round(time.monotonic() - case_started, 4),
                        }
                    )
        failed = sum(case["result"] != "passed" for case in cases)
        invariant_counts, failures_by_invariant = summarize_invariants(cases)
        output_fingerprints = [
            {
                "scenario": case["scenario"],
                "seed": case["seed"],
                **case["canonical_output_sha256"],
            }
            for case in cases
            if "canonical_output_sha256" in case
        ]
        fingerprints = {
            "schema_version": 1,
            "fixture_checksums_sha256": checksums,
            "outputs": output_fingerprints,
        }
        (campaign_dir / "fingerprints.json").write_text(
            json.dumps(fingerprints, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        summary = {
            "status": "failed" if failed else "passed",
            "binary": str(binary),
            "seeds": list(args.seeds),
            "case_count": len(cases),
            "passed_cases": len(cases) - failed,
            "failed_cases": failed,
            "elapsed_seconds": round(time.monotonic() - campaign_started, 4),
            "campaign_directory": str(campaign_dir),
            "fixture_checksums_sha256": checksums,
            "invariant_check_counts": invariant_counts,
            "failures_by_invariant": failures_by_invariant,
            "cases": cases,
        }
        summary_path = campaign_dir / "summary.json"
        summary_path.write_text(
            json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        print(json.dumps(summary, indent=2, sort_keys=True))
        return 1 if failed else 0
    except CampaignError as error:
        print(json.dumps({"status": "failed", "error": str(error)}, indent=2))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
