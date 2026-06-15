#!/usr/bin/env python3
"""Build normalized benchmark raw exports, summaries, and dashboard Markdown."""

from __future__ import annotations

import argparse
import csv
import json
import math
import statistics
import sys
import tempfile
import time
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

SCHEMA_VERSION = 1
LEADERBOARD_ORDER = [
    "correctness",
    "startup",
    "idle",
    "pty",
    "parser",
    "render",
    "frame_pacing",
    "input_latency",
    "flood",
    "scrollback",
    "resize",
    "unicode",
    "graphics",
    "keyboard_mouse_paste_ime",
    "real_apps",
    "multiplexer",
    "remote",
    "tabs_splits",
    "power",
    "fault",
]
REQUIRED_ROW_FIELDS = {
    "schema_version",
    "run_id",
    "terminal",
    "profile",
    "platform",
    "benchmark",
    "metric",
    "value",
    "unit",
}
SUMMARY_FIELDS = [
    "category",
    "benchmark",
    "metric",
    "unit",
    "terminal",
    "profile",
    "platform",
    "count",
    "min",
    "p50",
    "p95",
    "p99",
    "max",
    "mean",
    "stddev",
    "ci95",
    "cv",
    "invalidated_runs",
    "raw_rows",
]


@dataclass(frozen=True, order=True)
class SummaryKey:
    category: str
    benchmark: str
    metric: str
    unit: str
    terminal: str
    profile: str
    platform: str


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="*", type=Path, help="input JSONL result files")
    parser.add_argument("--output-dir", type=Path, default=Path("artifacts/benchmark-dashboard"))
    parser.add_argument("--strict", action="store_true", help="fail on invalid rows")
    parser.add_argument("--self-test", action="store_true", help="run a built-in dashboard smoke test")
    return parser.parse_args(argv)


def read_rows(paths: Iterable[Path]) -> tuple[list[dict[str, Any]], list[str]]:
    rows: list[dict[str, Any]] = []
    errors: list[str] = []
    for path in paths:
        with path.open(encoding="utf-8") as handle:
            for line_number, line in enumerate(handle, start=1):
                if not line.strip():
                    continue
                try:
                    raw = json.loads(line)
                except json.JSONDecodeError as exc:
                    errors.append(f"{path}:{line_number}: invalid JSON: {exc}")
                    continue
                rows.extend(normalize_row(raw, path, line_number, errors))
    return rows, errors


def normalize_row(raw: dict[str, Any], path: Path, line_number: int, errors: list[str]) -> list[dict[str, Any]]:
    if "metric" in raw and "value" in raw:
        row = dict(raw)
        row.setdefault("schema_version", SCHEMA_VERSION)
        row.setdefault("raw_artifact", str(path))
        return [row]

    if raw.get("event") in {"metadata", "terminal_inventory", "terminal_correctness_gate"}:
        return []

    metrics = raw.get("metrics")
    if isinstance(metrics, list):
        normalized = []
        for metric_index, metric in enumerate(metrics):
            if not isinstance(metric, dict) or "metric" not in metric or "value" not in metric:
                errors.append(f"{path}:{line_number}: metric {metric_index} is not normalized")
                continue
            benchmark = (
                raw.get("benchmark")
                or raw.get("adapter")
                or raw.get("source")
                or raw.get("tool")
                or raw.get("event")
                or "unknown"
            )
            metric_benchmark = metric.get("benchmark")
            if metric_benchmark:
                benchmark = f"{benchmark}/{metric_benchmark}"
            normalized.append(
                {
                    "schema_version": SCHEMA_VERSION,
                    "run_id": raw.get("run_id") or f"{path.stem}:{line_number}:{metric_index}",
                    "terminal": raw.get("terminal") or raw.get("adapter") or raw.get("source") or "unknown",
                    "terminal_version": raw.get("terminal_version", ""),
                    "profile": raw.get("profile", "unspecified"),
                    "platform": raw.get("platform", "unspecified"),
                    "display_server": raw.get("display_server", ""),
                    "benchmark": benchmark,
                    "category": raw.get("category", infer_category(str(raw.get("category", "")), str(benchmark))),
                    "metric": metric["metric"],
                    "value": metric["value"],
                    "unit": metric.get("unit", "count"),
                    "status": raw.get("status", "pass"),
                    "correctness_status": raw.get("correctness_status", raw.get("status", "pass")),
                    "latency_method": raw.get("latency_method"),
                    "capture_method": raw.get("capture_method"),
                    "source_csv": raw.get("source_csv", ""),
                    "raw_artifact": str(path),
                }
            )
        return normalized

    errors.append(f"{path}:{line_number}: row has no metric/value or metrics[]")
    return []


def validate_rows(rows: list[dict[str, Any]]) -> list[str]:
    errors: list[str] = []
    for index, row in enumerate(rows):
        missing = sorted(field for field in REQUIRED_ROW_FIELDS if field not in row)
        if missing:
            errors.append(f"row {index}: missing {missing}")
            continue
        if row["schema_version"] != SCHEMA_VERSION:
            errors.append(f"row {index}: schema_version must be {SCHEMA_VERSION}")
        try:
            row["value"] = float(row["value"])
        except (TypeError, ValueError):
            errors.append(f"row {index}: value is not numeric")
    return errors


def infer_category(category: str, benchmark: str) -> str:
    if category:
        return category
    lower = benchmark.lower()
    mapping = [
        ("correct", "correctness"),
        ("startup", "startup"),
        ("idle", "idle"),
        ("pty", "pty"),
        ("parser", "parser"),
        ("render", "render"),
        ("frame", "frame_pacing"),
        ("latency", "input_latency"),
        ("flood", "flood"),
        ("scroll", "scrollback"),
        ("resize", "resize"),
        ("unicode", "unicode"),
        ("glyph", "unicode"),
        ("graphics", "graphics"),
        ("kitty", "graphics"),
        ("keyboard", "keyboard_mouse_paste_ime"),
        ("mouse", "keyboard_mouse_paste_ime"),
        ("paste", "keyboard_mouse_paste_ime"),
        ("real_app", "real_apps"),
        ("multiplexer", "multiplexer"),
        ("remote", "remote"),
        ("pane", "tabs_splits"),
        ("tab", "tabs_splits"),
        ("power", "power"),
        ("thermal", "power"),
        ("hostile", "fault"),
        ("fuzz", "fault"),
    ]
    for needle, mapped in mapping:
        if needle in lower:
            return mapped
    return "uncategorized"


def percentile(values: list[float], fraction: float) -> float:
    if not values:
        return math.nan
    if len(values) == 1:
        return values[0]
    position = (len(values) - 1) * fraction
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return values[int(position)]
    weight = position - lower
    return values[lower] * (1.0 - weight) + values[upper] * weight


def summarize(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    grouped: dict[SummaryKey, list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        category = infer_category(str(row.get("category", "")), str(row["benchmark"]))
        grouped[
            SummaryKey(
                category=category,
                benchmark=str(row["benchmark"]),
                metric=str(row["metric"]),
                unit=str(row["unit"]),
                terminal=str(row["terminal"]),
                profile=str(row["profile"]),
                platform=str(row["platform"]),
            )
        ].append(row)

    summaries = []
    for key, group_rows in sorted(grouped.items(), key=lambda item: item[0]):
        values = sorted(float(row["value"]) for row in group_rows)
        mean = statistics.fmean(values)
        stddev = statistics.stdev(values) if len(values) > 1 else 0.0
        ci95 = 1.96 * stddev / math.sqrt(len(values)) if values else math.nan
        cv = stddev / abs(mean) if mean else 0.0
        invalidated = sum(
            1
            for row in group_rows
            if row.get("correctness_status") in {"fail", "timeout", "invalidated"}
            or row.get("status") in {"fail", "timeout", "invalidated"}
        )
        summaries.append(
            {
                "category": key.category,
                "benchmark": key.benchmark,
                "metric": key.metric,
                "unit": key.unit,
                "terminal": key.terminal,
                "profile": key.profile,
                "platform": key.platform,
                "count": len(values),
                "min": values[0],
                "p50": percentile(values, 0.50),
                "p95": percentile(values, 0.95),
                "p99": percentile(values, 0.99),
                "max": values[-1],
                "mean": mean,
                "stddev": stddev,
                "ci95": ci95,
                "cv": cv,
                "invalidated_runs": invalidated,
                "raw_rows": len(group_rows),
            }
        )
    return summaries


def write_raw(rows: list[dict[str, Any]], output_dir: Path) -> Path:
    path = output_dir / "raw-normalized.jsonl"
    with path.open("w", encoding="utf-8") as handle:
        for row in rows:
            handle.write(json.dumps(row, sort_keys=True) + "\n")
    return path


def write_csv(summaries: list[dict[str, Any]], output_dir: Path) -> Path:
    path = output_dir / "summary.csv"
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=SUMMARY_FIELDS)
        writer.writeheader()
        writer.writerows(summaries)
    return path


def fmt(value: Any) -> str:
    if isinstance(value, float):
        return f"{value:.6g}"
    return str(value)


def write_dashboard(summaries: list[dict[str, Any]], output_dir: Path, elapsed_ms: float) -> Path:
    path = output_dir / "dashboard.md"
    by_category: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in summaries:
        by_category[row["category"]].append(row)

    categories = [category for category in LEADERBOARD_ORDER if category in by_category]
    categories.extend(sorted(set(by_category) - set(categories)))

    lines = [
        "# Bootty benchmark dashboard",
        "",
        "This dashboard preserves independent leaderboards. It intentionally does not",
        "publish a single aggregate fastest-terminal score.",
        "",
        f"Generated in {elapsed_ms:.1f} ms from {sum(row['raw_rows'] for row in summaries)} raw metric rows.",
        "",
        "Artifacts:",
        "",
        "- `raw-normalized.jsonl`: normalized raw metric rows",
        "- `summary.csv`: grouped p50/p95/p99/min/max/stddev/CI/CV summaries",
        "",
    ]
    for category in categories:
        lines.extend([f"## {category}", "", "| Benchmark | Metric | Terminal | Profile | Platform | n | p50 | p95 | p99 | max | Invalidated |", "| --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |"])
        for row in by_category[category]:
            lines.append(
                "| "
                + " | ".join(
                    [
                        str(row["benchmark"]),
                        f"{row['metric']} ({row['unit']})",
                        str(row["terminal"]),
                        str(row["profile"]),
                        str(row["platform"]),
                        str(row["count"]),
                        fmt(row["p50"]),
                        fmt(row["p95"]),
                        fmt(row["p99"]),
                        fmt(row["max"]),
                        str(row["invalidated_runs"]),
                    ]
                )
                + " |"
            )
        lines.append("")
    path.write_text("\n".join(lines), encoding="utf-8")
    return path


def row_is_valid(row: dict[str, Any]) -> bool:
    if any(field not in row for field in REQUIRED_ROW_FIELDS):
        return False
    try:
        row["value"] = float(row["value"])
    except (TypeError, ValueError):
        return False
    return row.get("schema_version") == SCHEMA_VERSION


def run_self_test() -> int:
    sample_rows = [
        {
            "schema_version": SCHEMA_VERSION,
            "run_id": "self-test",
            "terminal": "bootty",
            "profile": "normalized",
            "platform": "local",
            "benchmark": "render/fullscreen",
            "category": "render",
            "metric": "frame_time",
            "value": 8.0,
            "unit": "ms",
            "correctness_status": "pass",
        },
        {
            "schema_version": SCHEMA_VERSION,
            "run_id": "self-test",
            "terminal": "bootty",
            "profile": "normalized",
            "platform": "local",
            "benchmark": "render/fullscreen",
            "category": "render",
            "metric": "frame_time",
            "value": 10.0,
            "unit": "ms",
            "correctness_status": "pass",
        },
        {
            "schema_version": SCHEMA_VERSION,
            "run_id": "self-test",
            "terminal": "kitty",
            "profile": "normalized",
            "platform": "local",
            "benchmark": "render/fullscreen",
            "category": "render",
            "metric": "frame_time",
            "value": 12.0,
            "unit": "ms",
            "correctness_status": "pass",
        },
    ]
    with tempfile.TemporaryDirectory(prefix="bootty-dashboard-self-test.") as temp_dir:
        temp_path = Path(temp_dir)
        input_path = temp_path / "results.jsonl"
        input_path.write_text("".join(json.dumps(row) + "\n" for row in sample_rows), encoding="utf-8")
        rows, read_errors = read_rows([input_path])
        validation_errors = validate_rows(rows)
        if read_errors or validation_errors:
            for error in read_errors + validation_errors:
                print(error, file=sys.stderr)
            return 1
        output_dir = temp_path / "dashboard"
        output_dir.mkdir()
        summaries = summarize(rows)
        write_raw(rows, output_dir)
        write_csv(summaries, output_dir)
        write_dashboard(summaries, output_dir, 0.0)
        if len(summaries) != 2:
            print(f"expected 2 summary rows, got {len(summaries)}", file=sys.stderr)
            return 1
    print("benchmark dashboard self-test passed")
    return 0


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    if args.self_test:
        return run_self_test()
    if not args.inputs:
        print("at least one input JSONL file is required unless --self-test is used", file=sys.stderr)
        return 2
    started = time.perf_counter()
    rows, read_errors = read_rows(args.inputs)
    validation_errors = validate_rows(rows)
    errors = read_errors + validation_errors
    if errors and args.strict:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    valid_rows = [row for row in rows if row_is_valid(row)]
    if not valid_rows:
        print("no valid benchmark metric rows", file=sys.stderr)
        return 1

    args.output_dir.mkdir(parents=True, exist_ok=True)
    raw_path = write_raw(valid_rows, args.output_dir)
    summaries = summarize(valid_rows)
    csv_path = write_csv(summaries, args.output_dir)
    dashboard_path = write_dashboard(summaries, args.output_dir, (time.perf_counter() - started) * 1000.0)
    if errors:
        (args.output_dir / "validation-errors.txt").write_text("\n".join(errors) + "\n", encoding="utf-8")
    print(f"Wrote raw export: {raw_path}")
    print(f"Wrote summary CSV: {csv_path}")
    print(f"Wrote dashboard: {dashboard_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
