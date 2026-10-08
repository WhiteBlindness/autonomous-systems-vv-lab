"""Planeamento e execução limitada das matrizes M1 e M2."""

from __future__ import annotations

import json
import time
from pathlib import Path
from typing import Any

from campaign_report import CampaignReportError, build_summary, write_campaign_outputs
from process_limits import CampaignArtifactBudget, ProcessLimitError
from m2_campaign import (
    M2CampaignError,
    fixture_checksums as m2_fixture_checksums,
    load_m2_manifest,
    matched_geometry_errors,
    select_variant,
)


def planned_matrix_counts(
    *,
    entries: list[dict[str, Any]],
    variants: tuple[str, ...],
    seeds: tuple[int, ...],
    include_m1: bool,
    include_m2: bool,
) -> tuple[int, int, int]:
    from run_campaign import EXPECTED_STATUS

    m1_cases = len(seeds) * len(EXPECTED_STATUS) if include_m1 else 0
    m2_cases_per_seed = 0
    if include_m2:
        for entry in entries:
            for variant_name in variants:
                if variant_name not in entry["variants"]:
                    continue
                for seed in seeds:
                    select_variant(entry, variant_name, seed)
                m2_cases_per_seed += 1
    m2_cases = len(seeds) * m2_cases_per_seed
    planned_invocations = m1_cases * 3 + m2_cases * 4
    return m1_cases, m2_cases, planned_invocations


def run_campaign(args: Any) -> int:
    from run_campaign import (
        EXPECTED_STATUS,
        ROOT,
        CampaignError,
        execute_case,
        execute_m2_case,
        load_canonical_output_hashes,
        portable_path,
        unique_campaign_dir,
        verify_fixture_checksums,
    )

    campaign_started = time.monotonic()
    deadline = campaign_started + args.time_budget_seconds
    binary = args.binary if args.binary.is_absolute() else ROOT / args.binary
    binary = binary.resolve()
    suite = args.suite
    include_m1 = suite in {"m1", "all"}
    include_m2 = suite in {"m2", "all"}
    try:
        if not binary.is_file():
            raise CampaignError(f"compiled vv-lab binary not found: {binary.name}")
        if args.max_artifact_bytes < 65536:
            raise CampaignError("--max-artifact-bytes must be at least 65536")

        m1_checksums = verify_fixture_checksums() if include_m1 else {}
        canonical_hashes = load_canonical_output_hashes() if include_m1 else {}
        m2_manifest = load_m2_manifest() if include_m2 else {"scenarios": []}
        m2_entries = m2_manifest["scenarios"]
        if args.m2_scenarios and include_m2:
            selected_names = tuple(args.m2_scenarios)
            by_name = {entry["name"]: entry for entry in m2_entries}
            missing = [name for name in selected_names if name not in by_name]
            if missing:
                raise CampaignError(f"unknown M2 scenarios: {', '.join(missing)}")
            m2_entries = [by_name[name] for name in selected_names]
        if include_m2:
            by_name = {entry["name"]: entry for entry in m2_manifest["scenarios"]}
            for entry in m2_entries:
                errors = matched_geometry_errors(entry, by_name)
                if errors:
                    raise CampaignError("; ".join(errors))
                if entry.get("matched_baseline"):
                    fixture = json.loads(
                        (ROOT / "scenarios" / "m2" / entry["file"]).read_text(encoding="utf-8")
                    )
                    if fixture.get("validation_mutants"):
                        raise CampaignError(
                            f"natural fault fixture {entry['name']} must not use validation mutants"
                        )
            selected_variant_count = sum(
                variant in entry["variants"]
                for entry in m2_entries
                for variant in args.variants
            )
            if selected_variant_count == 0:
                raise CampaignError("no selected M2 variants apply to the chosen scenarios")
        else:
            selected_variant_count = 0

        m1_cases, m2_cases, planned_simulator_invocations = planned_matrix_counts(
            entries=m2_entries,
            variants=args.variants,
            seeds=args.seeds,
            include_m1=include_m1,
            include_m2=include_m2,
        )
        planned_cases = m1_cases + m2_cases
        if planned_cases > args.max_cases:
            raise CampaignError(
                f"planned matrix has {planned_cases} cases, above --max-cases {args.max_cases}"
            )
        if planned_simulator_invocations > args.max_runs:
            raise CampaignError(
                f"planned matrix needs {planned_simulator_invocations} simulator invocations, above --max-runs {args.max_runs}"
            )
        summary_reserve = min(
            args.max_artifact_bytes // 4,
            max(65536, planned_cases * 4096),
        )
        case_artifact_budget = args.max_artifact_bytes - summary_reserve
        if case_artifact_budget < 1:
            raise CampaignError("artifact limit leaves no room for simulation outputs")

        checksums = {
            **{f"m1/{name}.json": digest for name, digest in m1_checksums.items()},
            **(m2_fixture_checksums(m2_manifest) if include_m2 else {}),
        }
        output_root = args.output_root if args.output_root.is_absolute() else ROOT / args.output_root
        campaign_dir = unique_campaign_dir(output_root.resolve())
        try:
            artifact_budget = CampaignArtifactBudget(campaign_dir, case_artifact_budget)
        except ProcessLimitError as error:
            raise CampaignError(str(error)) from error

        def account_failed_case(case_dir: Path) -> None:
            try:
                artifact_budget.refresh(case_dir)
            except ProcessLimitError as error:
                raise CampaignError(str(error)) from error

        command_counts = {
            name: {"attempted": 0, "completed": 0}
            for name in ("run", "replay", "analyze")
        }
        cases: list[dict[str, Any]] = []
        should_stop = False
        for seed in args.seeds:
            if include_m1:
                for name in EXPECTED_STATUS:
                    case_started = time.monotonic()
                    try:
                        case = execute_case(
                            binary,
                            campaign_dir,
                            name,
                            seed,
                            canonical_hashes,
                            deadline=deadline,
                            artifact_budget=artifact_budget,
                            command_counts=command_counts,
                        )
                    except CampaignError as error:
                        account_failed_case(campaign_dir / name / f"seed-{seed}")
                        case = {
                            "suite": "m1",
                            "scenario": name,
                            "variant": "default",
                            "seed": seed,
                            "expected_status": EXPECTED_STATUS[name],
                            "result": "failed",
                            "errors": [str(error)],
                            "elapsed_seconds": round(time.monotonic() - case_started, 4),
                        }
                    cases.append(case)
                    if time.monotonic() >= deadline:
                        should_stop = True
                        break
                if should_stop:
                    break
            if include_m2:
                for entry in m2_entries:
                    for variant_name in args.variants:
                        if variant_name not in entry["variants"]:
                            continue
                        policy, expected_outcome, overrides = select_variant(
                            entry, variant_name, seed
                        )
                        case_started = time.monotonic()
                        try:
                            case = execute_m2_case(
                                binary,
                                campaign_dir,
                                entry,
                                variant_name,
                                seed,
                                policy,
                                expected_outcome,
                                overrides,
                                deadline=deadline,
                                artifact_budget=artifact_budget,
                                command_counts=command_counts,
                            )
                        except CampaignError as error:
                            account_failed_case(
                                campaign_dir / entry["name"] / variant_name / f"seed-{seed}"
                            )
                            case = {
                                "suite": "m2",
                                "scenario": entry["name"],
                                "variant": variant_name,
                                "seed": seed,
                                "policy": policy,
                                "expected_status": entry["expected_status"],
                                "expected_outcome": expected_outcome,
                                "expected_invariants": entry["expected_invariants"],
                                "result": "failed",
                                "errors": [str(error)],
                                "elapsed_seconds": round(time.monotonic() - case_started, 4),
                            }
                        cases.append(case)
                        if time.monotonic() >= deadline:
                            should_stop = True
                            break
                    if should_stop:
                        break
                if should_stop:
                    break

        output_fingerprints = [
            {
                "suite": case["suite"],
                "scenario": case["scenario"],
                "variant": case["variant"],
                "seed": case["seed"],
                **case["canonical_output_sha256"],
            }
            for case in cases
            if "canonical_output_sha256" in case
        ]
        fingerprints = {
            "schema_version": 1,
            "suite": suite,
            "suites": ["m1", "m2"] if suite == "all" else [suite],
            "variants": list(args.variants) if include_m2 else [],
            "m2_scenarios": [entry["name"] for entry in m2_entries] if include_m2 else [],
            "fixture_checksums_sha256": checksums,
            "outputs": output_fingerprints,
        }
        summary = build_summary(
            suite=suite,
            cases=cases,
            command_counts=command_counts,
            campaign_started=campaign_started,
            campaign_directory=portable_path(campaign_dir, ROOT),
            checksums=checksums,
            seeds=args.seeds,
            variants=args.variants,
            binary_name=binary.name,
            max_artifact_bytes=args.max_artifact_bytes,
            max_runs=args.max_runs,
            max_cases=args.max_cases,
            planned_cases=planned_cases,
            time_budget_seconds=args.time_budget_seconds,
        )
        try:
            write_campaign_outputs(campaign_dir, fingerprints, summary, args.max_artifact_bytes)
        except (OSError, CampaignReportError) as error:
            raise CampaignError(f"campaign output could not be retained: {error}") from error
        print(
            json.dumps(
                {
                    "status": summary["status"],
                    "suite": suite,
                    "cases": summary["case_count"],
                    "simulator_invocations": summary["execution_counts"]["simulator_invocations"],
                    "campaign_directory": portable_path(campaign_dir, ROOT),
                    "summary": portable_path(campaign_dir / "summary.json", ROOT),
                },
                sort_keys=True,
            )
        )
        return 1 if summary["status"] != "passed" else 0
    except (CampaignError, M2CampaignError) as error:
        print(json.dumps({"status": "failed", "error": str(error)}, indent=2))
        return 1
