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


def load_fingerprints(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise CampaignError(f"cannot read fingerprints at {path}: {error}") from error
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise CampaignError(f"unsupported fingerprints file at {path}")
    return value


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
        hashes = {field: entry.get(field) for field in fields}
        if any(not isinstance(digest, str) for digest in hashes.values()):
            raise CampaignError(f"fingerprint hashes are incomplete for {identity}")
        if identity in selected:
            raise CampaignError(f"duplicate seed 42 fingerprint: {identity}")
        selected[identity] = hashes
    return selected


def compare_platforms(root: Path) -> dict[str, Any]:
    linux_path = find_fingerprint(root, "linux-campaign")
    windows_path = find_fingerprint(root, "windows-campaign")
    linux = load_fingerprints(linux_path)
    windows = load_fingerprints(windows_path)
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
    if linux.get("fixture_checksums_sha256") != expected_fixture_hashes:
        raise CampaignError("fingerprints do not match the checked-in scenario fixtures")

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
    return {
        "status": "passed",
        "scenario_count": len(linux_outputs),
        "seed": 42,
        "fixture_checksums_sha256": linux["fixture_checksums_sha256"],
        "output_checksums": linux_outputs,
        "minimized_output_checksums": linux_minimized,
        "fingerprint_files": {"linux": str(linux_path), "windows": str(windows_path)},
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
