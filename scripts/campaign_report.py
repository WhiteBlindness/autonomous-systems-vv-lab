"""Construção dos resumos JSON e Markdown da campanha."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from process_limits import ensure_tree_size


class CampaignReportError(RuntimeError):
    """Não foi possível guardar um resumo dentro do limite definido."""


def campaign_markdown(summary: dict[str, Any]) -> str:
    elapsed = f"{summary['elapsed_seconds']:.4f}".replace(".", ",")
    lines = [
        "# Resumo da campanha",
        "",
        f"Estado: **{summary['status']}**.",
        f"Conjuntos: {', '.join(summary['suites'])}.",
        f"Casos: {summary['passed_cases']} aprovados e {summary['failed_cases']} com divergências, em {summary['case_count']} executados de {summary['planned_case_count']} planeados; limite `--max-cases`: {summary['max_cases']}.",
        f"Execuções do simulador: {summary['execution_counts']['simulator_invocations']} chamadas ({summary['execution_counts']['run']['completed']} `run`, {summary['execution_counts']['replay']['completed']} `replay`, {summary['execution_counts']['analyze']['completed']} `analyze`); limite `--max-runs`: {summary['max_runs']}.",
        f"Repetições idênticas: {summary['repeat_checks']['passed']}/{summary['repeat_checks']['checked']}; reproduções válidas: {summary['replay_checks']['passed']}/{summary['replay_checks']['checked']}; análises coincidentes: {summary['analyze_checks']['passed']}/{summary['analyze_checks']['checked']}.",
        f"Tempo real medido: {elapsed} s. Artefactos retidos: {summary['retained_artifact_bytes']} bytes.",
    ]
    if summary["mission_outcome_counts"]["observed"]:
        lines.extend(
            [
                "",
                "## Resultados das missões",
                "",
                "| Resultado | Casos |",
                "| --- | ---: |",
            ]
        )
        for outcome in sorted(summary["mission_outcome_counts"]["actual"]):
            count = summary["mission_outcome_counts"]["actual"][outcome]
            lines.append(f"| `{outcome}` | {count} |")
        lines.append(
            f"Divergências face às expectativas: {summary['mission_outcome_counts']['unexpected']} de {summary['mission_outcome_counts']['expected_checked']} casos com expectativa declarada."
        )
    lines.extend(
        [
            "",
            "## Invariantes",
            "",
            "| Invariante | Verificações | PASS | FAIL esperado | FAIL inesperado |",
            "| --- | ---: | ---: | ---: | ---: |",
        ]
    )
    for invariant, counts in summary["invariant_check_counts"].items():
        lines.append(
            f"| `{invariant}` | {counts['checks']} | {counts['passed']} | {counts['expected_failures']} | {counts['unexpected_failures']} |"
        )
    lines.extend(
        [
            "",
            "Os resultados e as métricas por caso estão em `summary.json`; os hashes de comparação estão em `fingerprints.json`.",
            "",
        ]
    )
    return "\n".join(lines)


def build_summary(
    *,
    suite: str,
    cases: list[dict[str, Any]],
    command_counts: dict[str, dict[str, int]],
    campaign_started: float,
    campaign_directory: str,
    checksums: dict[str, str],
    seeds: tuple[int, ...],
    variants: tuple[str, ...],
    binary_name: str,
    max_artifact_bytes: int,
    max_runs: int,
    max_cases: int,
    planned_cases: int,
    time_budget_seconds: float,
) -> dict[str, Any]:
    import time

    failed = sum(case.get("result") != "passed" for case in cases)
    invariant_counts = {
        name: {
            "checks": 0,
            "passed": 0,
            "failed": 0,
            "expected_failures": 0,
            "unexpected_failures": 0,
        }
        for name in ("restricted_zone", "world_bounds", "safe_fallback", "valid_state_transitions")
    }
    failures_by_invariant: dict[str, list[dict[str, Any]]] = {
        name: [] for name in invariant_counts
    }
    for case in cases:
        actual = case.get("invariants", {})
        expected = case.get("expected_invariants", {})
        for name in invariant_counts:
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
                        "scenario": case.get("scenario"),
                        "variant": case.get("variant"),
                        "seed": case.get("seed"),
                        "expected_failure": expected_failure,
                        "evidence": case.get("invariant_failure_evidence", {}).get(name, []),
                        "report_path": case.get("output_paths", {}).get("report"),
                    }
                )

    expected_outcomes: dict[str, int] = {}
    actual_outcomes: dict[str, int] = {}
    unexpected_outcomes = []
    outcome_observed = 0
    outcome_expected_checked = 0
    for case in cases:
        metrics = case.get("mission_metrics")
        if not isinstance(metrics, dict):
            continue
        expected = case.get("expected_outcome")
        actual = metrics.get("outcome")
        outcome_observed += 1
        if isinstance(expected, str):
            expected_outcomes[expected] = expected_outcomes.get(expected, 0) + 1
            outcome_expected_checked += 1
        if isinstance(actual, str):
            actual_outcomes[actual] = actual_outcomes.get(actual, 0) + 1
        if isinstance(expected, str) and expected != actual:
            unexpected_outcomes.append(
                {
                    "scenario": case.get("scenario"),
                    "variant": case.get("variant"),
                    "seed": case.get("seed"),
                    "expected": expected,
                    "actual": actual,
                    "mission_path": case.get("output_paths", {}).get("mission"),
                }
            )

    repeat_cases = [case for case in cases if "deterministic" in case]
    replay_cases = [case for case in cases if "replay_exit_code" in case]
    analyze_cases = [
        case
        for case in cases
        if case.get("suite") == "m2" and "analysis_matches_run" in case
    ]
    simulator_invocations = sum(
        command_counts[name]["attempted"] for name in ("run", "replay", "analyze")
    )
    suites = ["m1", "m2"] if suite == "all" else [suite]
    return {
        "status": "failed" if failed or len(cases) != planned_cases else "passed",
        "suites": suites,
        "binary": binary_name,
        "seeds": list(seeds),
        "variants": list(variants) if "m2" in suites else [],
        "case_count": len(cases),
        "planned_case_count": planned_cases,
        "passed_cases": len(cases) - failed,
        "failed_cases": failed,
        "mission_outcome_counts": {
            "checked": outcome_expected_checked,
            "expected_checked": outcome_expected_checked,
            "observed": outcome_observed,
            "expected": expected_outcomes,
            "actual": actual_outcomes,
            "unexpected": len(unexpected_outcomes),
            "unexpected_cases": unexpected_outcomes,
        },
        "invariant_failure_count": sum(value["failed"] for value in invariant_counts.values()),
        "invariant_check_counts": invariant_counts,
        "failures_by_invariant": failures_by_invariant,
        "execution_counts": {
            **command_counts,
            "simulator_invocations": simulator_invocations,
        },
        "repeat_checks": {
            "checked": len(repeat_cases),
            "passed": sum(case.get("deterministic") is True for case in repeat_cases),
            "failed": sum(case.get("deterministic") is False for case in repeat_cases),
        },
        "replay_checks": {
            "checked": len(replay_cases),
            "passed": sum(case.get("replay_exit_code") == 0 for case in replay_cases),
            "failed": sum(
                case.get("replay_exit_code") not in (None, 0) for case in replay_cases
            ),
        },
        "analyze_checks": {
            "checked": len(analyze_cases),
            "passed": sum(case.get("analysis_matches_run") is True for case in analyze_cases),
            "failed": sum(case.get("analysis_matches_run") is False for case in analyze_cases),
        },
        "elapsed_seconds": round(time.monotonic() - campaign_started, 4),
        "time_budget_seconds": time_budget_seconds,
        "max_artifact_bytes": max_artifact_bytes,
        "max_runs": max_runs,
        "max_cases": max_cases,
        "retained_artifact_bytes": 0,
        "campaign_directory": campaign_directory,
        "fixture_checksums_sha256": checksums,
        "cases": cases,
    }


def write_campaign_outputs(
    campaign_dir: Path,
    fingerprints: dict[str, Any],
    summary: dict[str, Any],
    max_artifact_bytes: int,
) -> None:
    fingerprints_text = json.dumps(fingerprints, indent=2, sort_keys=True) + "\n"
    fingerprint_path = campaign_dir / "fingerprints.json"
    current_size = ensure_tree_size(campaign_dir, max_artifact_bytes, "campaign artifacts")
    if current_size + len(fingerprints_text.encode("utf-8")) > max_artifact_bytes:
        raise CampaignReportError("campaign fingerprints exceed the artifact byte limit")
    fingerprint_path.write_bytes(fingerprints_text.encode("utf-8"))

    summary_path = campaign_dir / "summary.json"
    markdown_path = campaign_dir / "campaign-report.md"
    base_size = ensure_tree_size(campaign_dir, max_artifact_bytes, "campaign artifacts")
    for _ in range(10):
        summary_text = json.dumps(summary, indent=2, sort_keys=True) + "\n"
        markdown_text = campaign_markdown(summary)
        planned_total = base_size + len(summary_text.encode("utf-8")) + len(
            markdown_text.encode("utf-8")
        )
        if planned_total > max_artifact_bytes:
            raise CampaignReportError("campaign summary exceeds the artifact byte limit")
        if summary["retained_artifact_bytes"] == planned_total:
            break
        summary["retained_artifact_bytes"] = planned_total
    else:
        raise CampaignReportError("campaign summary size did not stabilize")
    summary_text = json.dumps(summary, indent=2, sort_keys=True) + "\n"
    markdown_text = campaign_markdown(summary)
    total = base_size + len(summary_text.encode("utf-8")) + len(markdown_text.encode("utf-8"))
    if total > max_artifact_bytes or total != summary["retained_artifact_bytes"]:
        raise CampaignReportError("campaign summary exceeds the artifact byte limit")
    summary_path.write_bytes(summary_text.encode("utf-8"))
    markdown_path.write_bytes(markdown_text.encode("utf-8"))
    if ensure_tree_size(campaign_dir, max_artifact_bytes, "campaign artifacts") != total:
        raise CampaignReportError("retained artifact byte count does not match the written files")
