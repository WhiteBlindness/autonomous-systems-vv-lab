#!/usr/bin/env python3
"""Compara campanhas e reduções de falhas entre Linux e Windows."""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path
from typing import Any

from m2_campaign import M2CampaignError, fixture_checksums as m2_fixture_checksums, load_m2_manifest
from m3_campaign import M3CampaignError, load_m3_manifest, m3_fixture_checksums
from run_campaign import (
    CampaignError,
    EXPECTED_STATUS,
    ROOT,
    load_canonical_output_hashes,
    verify_fixture_checksums,
)


def find_fingerprint(root: Path, platform: str) -> Path:
    matches = []
    for path in root.rglob("fingerprints.json"):
        relative_parts = [part.lower() for part in path.relative_to(root).parts]
        if any(part.startswith(platform.lower()) for part in relative_parts):
            matches.append(path)
    if len(matches) != 1:
        raise CampaignError(
            f"expected one {platform} fingerprints.json under {root}, found {len(matches)}"
        )
    return matches[0]


def find_fingerprints(root: Path, platform: str) -> list[Path]:
    matches = []
    for path in root.rglob("fingerprints.json"):
        relative_parts = [part.lower() for part in path.relative_to(root).parts]
        if any(part.startswith(platform.lower()) for part in relative_parts):
            matches.append(path)
    if not matches:
        raise CampaignError(f"no {platform} fingerprints.json found below {root}")
    return sorted(matches)


def load_fingerprints(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise CampaignError(f"cannot read fingerprints at {path}: {error}") from error
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise CampaignError(f"unsupported fingerprints file at {path}")
    return value


def merge_fingerprints(paths: list[Path]) -> dict[str, Any]:
    """Combine separately budgeted suite fingerprints for one platform."""
    merged: dict[str, Any] = {
        "schema_version": 1,
        "suite": "all",
        "suites": [],
        "outputs": [],
        "fixture_checksums_sha256": {},
    }
    seen_outputs: set[tuple[str, str, str, int]] = set()
    for path in paths:
        value = load_fingerprints(path)
        suites = value.get("suites", [value.get("suite")])
        outputs = value.get("outputs")
        checksums = value.get("fixture_checksums_sha256")
        if (
            not isinstance(suites, list)
            or any(not isinstance(suite, str) for suite in suites)
            or not isinstance(outputs, list)
            or not isinstance(checksums, dict)
        ):
            raise CampaignError(f"fingerprints file is incomplete: {path}")
        for suite in suites:
            if suite in merged["suites"]:
                raise CampaignError(f"duplicate {suite} campaign fingerprints for one platform")
            merged["suites"].append(suite)
        for name, digest in checksums.items():
            if name in merged["fixture_checksums_sha256"]:
                raise CampaignError(f"duplicate fixture checksum in {path}: {name}")
            merged["fixture_checksums_sha256"][name] = digest
        for entry in outputs:
            if not isinstance(entry, dict):
                raise CampaignError(f"fingerprint output is malformed in {path}")
            identity = (
                entry.get("suite"),
                entry.get("scenario"),
                entry.get("variant", "default"),
                entry.get("seed"),
            )
            if (
                not isinstance(identity[0], str)
                or not isinstance(identity[1], str)
                or not isinstance(identity[2], str)
                or type(identity[3]) is not int
                or identity in seen_outputs
            ):
                raise CampaignError(f"duplicate or invalid fingerprint identity in {path}")
            seen_outputs.add(identity)
            merged["outputs"].append(entry)
        for field in (
            "variants",
            "m2_scenarios",
            "m3_scenarios",
            "m3_contract_sets",
            "m3_fault_combinations",
        ):
            if field in value:
                if field in merged:
                    raise CampaignError(f"duplicate {field} selection in {path}")
                merged[field] = value[field]
    merged["suites"].sort()
    return merged


def find_minimized_outputs(root: Path, platform: str) -> dict[str, str]:
    matches = []
    for scenario_path in root.rglob("minimized.json"):
        if scenario_path.parent.name != "scenarios":
            continue
        relative_parts = [part.lower() for part in scenario_path.relative_to(root).parts]
        if not any(part.startswith(platform.lower()) for part in relative_parts):
            continue
        output_root = scenario_path.parent.parent
        final_root = output_root / "final"
        files = {
            "scenario_sha256": scenario_path,
            "events_sha256": final_root / "events.json",
            "report_sha256": final_root / "report.json",
            "mission_sha256": final_root / "mission.json",
        }
        if all(path.is_file() for path in files.values()):
            matches.append(files)
    if len(matches) != 1:
        raise CampaignError(
            f"expected one complete minimized artifact set for {platform} below {root}, found {len(matches)}"
        )
    try:
        return {
            field: hashlib.sha256(path.read_bytes()).hexdigest()
            for field, path in matches[0].items()
        }
    except OSError as error:
        raise CampaignError(f"cannot read minimized platform artifacts: {error}") from error


def find_m3_minimized_outputs(root: Path, platform: str) -> dict[str, str]:
    matches = []
    for summary_path in root.rglob("summary.json"):
        relative_parts = [part.lower() for part in summary_path.relative_to(root).parts]
        if not any(part.startswith(platform.lower()) for part in relative_parts):
            continue
        try:
            summary = json.loads(summary_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise CampaignError(f"cannot read M3 minimization summary: {error}") from error
        if not isinstance(summary, dict) or summary.get("suite") != "m3_minimization":
            continue
        output_root = summary_path.parent
        files = {
            "original_events_sha256": output_root / "original" / "events.json",
            "original_report_sha256": output_root / "original" / "report.json",
            "original_mission_sha256": output_root / "original" / "mission.json",
            "original_verification_sha256": output_root / "original" / "verification.json",
            "reduced_scenario_sha256": output_root / "reduced" / "scenario.json",
            "reduced_actuator_sha256": output_root / "reduced" / "actuator.json",
            "reduced_events_sha256": output_root / "reduced" / "events.json",
            "reduced_report_sha256": output_root / "reduced" / "report.json",
            "reduced_mission_sha256": output_root / "reduced" / "mission.json",
            "reduced_verification_sha256": output_root / "reduced" / "verification.json",
        }
        if all(path.is_file() for path in files.values()):
            matches.append(files)
    if len(matches) != 1:
        raise CampaignError(
            f"expected one complete M3 minimized artifact set for {platform} below {root}, found {len(matches)}"
        )
    try:
        return {
            field: hashlib.sha256(path.read_bytes()).hexdigest()
            for field, path in matches[0].items()
        }
    except OSError as error:
        raise CampaignError(f"cannot read M3 minimized platform artifacts: {error}") from error


def seed42_outputs(value: dict[str, Any]) -> dict[str, dict[str, str]]:
    outputs = value.get("outputs")
    if not isinstance(outputs, list):
        raise CampaignError("fingerprints file has no outputs list")
    selected: dict[str, dict[str, str]] = {}
    for entry in outputs:
        if not isinstance(entry, dict) or entry.get("seed") != 42:
            continue
        suite = entry.get("suite", "m1")
        scenario = entry.get("scenario")
        variant = entry.get("variant", "default")
        if not isinstance(scenario, str) or not isinstance(variant, str):
            raise CampaignError("fingerprint identity is incomplete")
        identity = f"{suite}:{scenario}:{variant}"
        fields = ("events_sha256", "report_sha256")
        if suite == "m2":
            fields = ("scenario_sha256", "events_sha256", "report_sha256", "mission_sha256")
        elif suite == "m3":
            fields = (
                "scenario_sha256",
                "properties_sha256",
                "actuator_config_sha256",
                "events_sha256",
                "report_sha256",
                "mission_sha256",
                "verification_sha256",
            )
        hashes = {field: entry.get(field) for field in fields}
        if any(not isinstance(digest, str) for digest in hashes.values()):
            raise CampaignError(f"fingerprint hashes are incomplete for {identity}")
        if identity in selected:
            raise CampaignError(f"duplicate seed 42 fingerprint: {identity}")
        selected[identity] = hashes
    return selected


def compare_platforms(root: Path) -> dict[str, Any]:
    linux_paths = find_fingerprints(root, "linux-campaign")
    windows_paths = find_fingerprints(root, "windows-campaign")
    linux = merge_fingerprints(linux_paths)
    windows = merge_fingerprints(windows_paths)
    linux_minimized = find_minimized_outputs(root, "linux-campaign")
    windows_minimized = find_minimized_outputs(root, "windows-campaign")
    if linux_minimized != windows_minimized:
        raise CampaignError("minimized natural-fault artifacts differ between Linux and Windows")
    if linux.get("fixture_checksums_sha256") != windows.get("fixture_checksums_sha256"):
        raise CampaignError("fixture checksums differ across operating systems")
    linux_outputs = seed42_outputs(linux)
    windows_outputs = seed42_outputs(windows)
    if linux_outputs != windows_outputs:
        raise CampaignError("canonical run outputs differ between Linux and Windows")
    linux_suites = set(linux.get("suites", [linux.get("suite", "m1")]))
    windows_suites = set(windows.get("suites", [windows.get("suite", "m1")]))
    if linux_suites != windows_suites:
        raise CampaignError("campaign suites differ across operating systems")
    expected_fixture_hashes: dict[str, str] = {}
    if "m1" in linux_suites:
        expected_fixture_hashes.update(
            {f"m1/{name}.json": digest for name, digest in verify_fixture_checksums().items()}
        )
    if "m2" in linux_suites:
        try:
            manifest = load_m2_manifest()
            expected_fixture_hashes.update(m2_fixture_checksums(manifest))
        except M2CampaignError as error:
            raise CampaignError(str(error)) from error
    if "m3" in linux_suites:
        linux_m3_minimized = find_m3_minimized_outputs(root, "linux-campaign")
        windows_m3_minimized = find_m3_minimized_outputs(root, "windows-campaign")
        if linux_m3_minimized != windows_m3_minimized:
            raise CampaignError("M3 temporal counterexamples differ between Linux and Windows")
    else:
        linux_m3_minimized = {}

    if "m1" in linux_suites:
        canonical = load_canonical_output_hashes()
        linux_m1 = {
            identity.split(":", 2)[1]: hashes
            for identity, hashes in linux_outputs.items()
            if identity.startswith("m1:")
        }
        if set(linux_m1) != set(EXPECTED_STATUS):
            raise CampaignError("seed 42 fingerprints do not include every M1 scenario")
        if linux_m1 != canonical:
            raise CampaignError("M1 outputs differ from the pinned canonical hashes")
    elif any(identity.startswith("m1:") for identity in linux_outputs):
        raise CampaignError("M1 outputs are present but the fingerprint omits the M1 suite")

    if "m2" in linux_suites:
        selected_variants = linux.get("variants")
        selected_scenarios = linux.get("m2_scenarios")
        if not isinstance(selected_variants, list) or not isinstance(selected_scenarios, list):
            raise CampaignError("M2 fingerprint is missing selected variants or scenarios")
        expected_m2_identities = {
            f"m2:{entry['name']}:{variant}"
            for entry in manifest["scenarios"]
            if entry["name"] in selected_scenarios
            for variant in selected_variants
            if variant in entry["variants"]
        }
        actual_m2_identities = {
            identity for identity in linux_outputs if identity.startswith("m2:")
        }
        if actual_m2_identities != expected_m2_identities:
            raise CampaignError("seed 42 fingerprints do not cover the selected M2 matrix")
    elif any(identity.startswith("m2:") for identity in linux_outputs):
        raise CampaignError("M2 outputs are present but the fingerprint omits the M2 suite")
    if "m3" in linux_suites:
        try:
            manifest = load_m3_manifest()
            expected_fixture_hashes.update(
                m3_fixture_checksums(manifest, ROOT / "scenarios" / "m3" / "expectations.json")
            )
        except M3CampaignError as error:
            raise CampaignError(str(error)) from error
        selected_scenarios = linux.get("m3_scenarios")
        selected_contract_sets = linux.get("m3_contract_sets")
        selected_faults = linux.get("m3_fault_combinations")
        if not all(isinstance(value, list) for value in (selected_scenarios, selected_contract_sets, selected_faults)):
            raise CampaignError("M3 fingerprints are missing selected scenarios, contract sets or faults")
        expected_m3_identities = {
            f"m3:{entry['id']}:{entry['contract_set']}+{entry['fault_combination']}"
            for entry in manifest["cases"]
            if entry["id"] in selected_scenarios
            and entry["contract_set"] in selected_contract_sets
            and entry["fault_combination"] in selected_faults
        }
        actual_m3_identities = {
            identity for identity in linux_outputs if identity.startswith("m3:")
        }
        if actual_m3_identities != expected_m3_identities:
            raise CampaignError("seed 42 fingerprints do not cover the selected M3 matrix")
    elif any(identity.startswith("m3:") for identity in linux_outputs):
        raise CampaignError("M3 outputs are present but the fingerprint omits the M3 suite")
    if linux.get("fixture_checksums_sha256") != expected_fixture_hashes:
        raise CampaignError("fingerprints do not match the checked-in scenario fixtures")
    return {
        "status": "passed",
        "scenario_count": len(linux_outputs),
        "seed": 42,
        "fixture_checksums_sha256": linux["fixture_checksums_sha256"],
        "output_checksums": linux_outputs,
        "minimized_output_checksums": linux_minimized,
        "m3_minimized_output_checksums": linux_m3_minimized,
        "fingerprint_files": {
            "linux": [str(path) for path in linux_paths],
            "windows": [str(path) for path in windows_paths],
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    args = parser.parse_args()
    root = args.root if args.root.is_absolute() else ROOT / args.root
    try:
        result = compare_platforms(root.resolve())
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0
    except CampaignError as error:
        print(json.dumps({"status": "failed", "error": str(error)}, indent=2))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
