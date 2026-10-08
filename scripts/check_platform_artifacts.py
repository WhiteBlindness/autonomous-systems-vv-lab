#!/usr/bin/env python3
"""Compare Linux and Windows campaign fingerprints for seed 42."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from run_campaign import CampaignError, EXPECTED_STATUS, ROOT, load_canonical_output_hashes


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


def seed42_outputs(value: dict[str, Any]) -> dict[str, dict[str, str]]:
    outputs = value.get("outputs")
    if not isinstance(outputs, list):
        raise CampaignError("fingerprints file has no outputs list")
    selected = {
        entry.get("scenario"): {
            "events_sha256": entry.get("events_sha256"),
            "report_sha256": entry.get("report_sha256"),
        }
        for entry in outputs
        if isinstance(entry, dict) and entry.get("seed") == 42
    }
    if set(selected) != set(EXPECTED_STATUS):
        raise CampaignError("seed 42 fingerprints do not include every campaign scenario")
    return selected


def compare_platforms(root: Path) -> dict[str, Any]:
    linux_path = find_fingerprint(root, "linux-campaign")
    windows_path = find_fingerprint(root, "windows-campaign")
    linux = load_fingerprints(linux_path)
    windows = load_fingerprints(windows_path)
    if linux.get("fixture_checksums_sha256") != windows.get("fixture_checksums_sha256"):
        raise CampaignError("scenario fixture checksums differ across operating systems")
    expected_outputs = load_canonical_output_hashes()
    linux_outputs = seed42_outputs(linux)
    windows_outputs = seed42_outputs(windows)
    if linux_outputs != windows_outputs:
        raise CampaignError("canonical run outputs differ between Linux and Windows")
    if linux_outputs != expected_outputs:
        raise CampaignError("cross-platform outputs differ from the pinned canonical hashes")
    return {
        "status": "passed",
        "scenario_count": len(EXPECTED_STATUS),
        "seed": 42,
        "fixture_checksums_sha256": linux["fixture_checksums_sha256"],
        "output_checksums": linux_outputs,
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
