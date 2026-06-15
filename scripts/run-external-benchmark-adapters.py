#!/usr/bin/env python3
"""Optional adapters for public terminal benchmark tools.

The runner emits JSONL rows in a Bootty-normalized shape. It does not run GUI or
terminal-flooding public benchmarks unless --run is passed; absent tools are
reported as skipped so the harness is safe on developer machines and CI.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import platform
import re
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Iterable

SCHEMA_VERSION = 1
DEFAULT_TIMEOUT_SECONDS = 120


class Adapter:
    def __init__(self, name: str, command: list[str], category: str) -> None:
        self.name = name
        self.command = command
        self.category = category


def now_ns() -> int:
    return time.time_ns()


def command_available(command: list[str]) -> bool:
    return bool(command and shutil.which(command[0]))


def command_string(command: list[str]) -> str:
    return " ".join(shlex.quote(part) for part in command)


def emit(output: Path, row: dict) -> None:
    row.setdefault("schema_version", SCHEMA_VERSION)
    with output.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(row, sort_keys=True) + "\n")


def metadata_row() -> dict:
    return {
        "event": "metadata",
        "recorded_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": os.getcwd(),
    }


def bootty_executable() -> str | None:
    path_bootty = shutil.which("bootty")
    if path_bootty:
        return path_bootty

    release_bootty = Path("target/release/bootty")
    if release_bootty.exists() and os.access(release_bootty, os.X_OK):
        return str(release_bootty)
    return None


def terminal_inventory() -> Iterable[dict]:
    bootty = bootty_executable()
    terminals = {
        "bootty": [bootty] if bootty else ["bootty"],
        "ghostty": ["ghostty", "--version"],
        "kitty": ["kitty", "--version"],
        "wezterm": ["wezterm", "--version"],
        "alacritty": ["alacritty", "--version"],
        "rio": ["rio", "--version"],
        "foot": ["foot", "--version"],
        "xterm": ["xterm", "-version"],
        "st": ["st", "-v"],
    }
    for terminal, command in terminals.items():
        executable = command[0] if command[0] and Path(command[0]).exists() else shutil.which(command[0] or "")
        row = {
            "event": "terminal_inventory",
            "terminal": terminal,
            "status": "present" if executable else "missing",
            "executable": executable,
            "version": None,
        }
        if executable and terminal != "bootty":
            try:
                completed = subprocess.run(
                    command,
                    check=False,
                    capture_output=True,
                    text=True,
                    timeout=5,
                )
                output = " ".join(
                    line.strip()
                    for line in (completed.stdout + "\n" + completed.stderr).splitlines()
                    if line.strip()
                )
                row["version"] = output[:240]
                row["exit_code"] = completed.returncode
            except subprocess.TimeoutExpired:
                row["status"] = "timeout"
        yield row


def numeric_metrics(text: str) -> list[dict]:
    metrics: list[dict] = []

    current_benchmark: str | None = None
    for line in text.splitlines():
        benchmark = re.match(r"\s*(?P<name>[^()]+)\s+\((?P<samples>\d+) samples @ (?P<size>\d+(?:\.\d+)?) MiB\):", line)
        if benchmark:
            current_benchmark = benchmark.group("name").strip()
            metrics.append(
                {
                    "metric": "sample_count",
                    "benchmark": current_benchmark,
                    "value": float(benchmark.group("samples")),
                    "unit": "count",
                }
            )
            metrics.append(
                {
                    "metric": "sample_size",
                    "benchmark": current_benchmark,
                    "value": float(benchmark.group("size")),
                    "unit": "MiB",
                }
            )
            continue

        summary = re.match(
            r"\s*(?P<mean>\d+(?:\.\d+)?)ms avg \(90% < (?P<p90>\d+)ms\) \+-(?P<stddev>\d+(?:\.\d+)?)ms",
            line,
        )
        if summary and current_benchmark:
            metrics.extend(
                [
                    {
                        "metric": "mean_sample_time",
                        "benchmark": current_benchmark,
                        "value": float(summary.group("mean")),
                        "unit": "ms",
                    },
                    {
                        "metric": "p90_sample_time",
                        "benchmark": current_benchmark,
                        "value": float(summary.group("p90")),
                        "unit": "ms",
                    },
                    {
                        "metric": "stddev_sample_time",
                        "benchmark": current_benchmark,
                        "value": float(summary.group("stddev")),
                        "unit": "ms",
                    },
                ]
            )

    patterns = [
        ("throughput", r"(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>[kKmMgG]?B/s)"),
        ("latency", r"(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>ms|us|µs)"),
        ("fps", r"(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>fps|FPS)"),
    ]
    for metric, pattern in patterns:
        for match in re.finditer(pattern, text):
            metrics.append(
                {
                    "metric": metric,
                    "value": float(match.group("value")),
                    "unit": match.group("unit"),
                }
            )
    return metrics[:96]


def cargo_package_benchmarks(package: str, version: str) -> Path | None:
    registry_src = Path.home() / ".cargo" / "registry" / "src"
    if not registry_src.exists():
        return None

    for candidate in sorted(registry_src.glob(f"*/{package}-{version}/benchmarks")):
        if candidate.is_dir():
            return candidate
    return None


def public_benchmark_command(binary: str, package: str, version: str) -> str:
    benchmark_dir = cargo_package_benchmarks(package, version)
    if benchmark_dir is None:
        return binary
    return command_string(
        [
            binary,
            "--benchmarks",
            str(benchmark_dir),
            "--max-samples",
            "3",
            "--max-secs",
            "3",
            "--min-bytes",
            "1048576",
        ]
    )


def unsupported_detail(adapter: Adapter, output: str) -> str | None:
    if adapter.name.startswith("kitty_benchmark") and "No kitten named __benchmark__" in output:
        return "kitty 0.42.2 removed the __benchmark__ kitten; adapter is unsupported for this install"
    return None


def run_adapter(adapter: Adapter, output: Path, run: bool, timeout: int) -> None:
    if not command_available(adapter.command):
        emit(
            output,
            {
                "event": "external_benchmark_adapter",
                "adapter": adapter.name,
                "category": adapter.category,
                "status": "skipped",
                "detail": f"{adapter.command[0]} not found",
                "command": command_string(adapter.command),
                "metrics": [],
            },
        )
        return

    if not run:
        emit(
            output,
            {
                "event": "external_benchmark_adapter",
                "adapter": adapter.name,
                "category": adapter.category,
                "status": "skipped",
                "detail": "pass --run to execute external benchmark commands",
                "command": command_string(adapter.command),
                "metrics": [],
            },
        )
        return

    start = now_ns()
    try:
        completed = subprocess.run(
            adapter.command,
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        duration_ns = now_ns() - start
        combined_output = f"{completed.stdout}\n{completed.stderr}"
        unsupported = unsupported_detail(adapter, combined_output)
        if unsupported:
            status = "unsupported"
            detail = unsupported
        elif completed.returncode == 0:
            status = "pass"
            detail = "ok"
        else:
            status = "fail"
            detail = completed.stderr.splitlines()[:1]
        emit(
            output,
            {
                "event": "external_benchmark_adapter",
                "adapter": adapter.name,
                "category": adapter.category,
                "status": status,
                "detail": detail,
                "command": command_string(adapter.command),
                "duration_ns": duration_ns,
                "exit_code": completed.returncode,
                "stdout_bytes": len(completed.stdout.encode()),
                "stderr_bytes": len(completed.stderr.encode()),
                "metrics": numeric_metrics(combined_output),
                "stdout_preview": completed.stdout[:1000],
                "stderr_preview": completed.stderr[:1000],
            },
        )
    except subprocess.TimeoutExpired as exc:
        emit(
            output,
            {
                "event": "external_benchmark_adapter",
                "adapter": adapter.name,
                "category": adapter.category,
                "status": "timeout",
                "detail": f"timed out after {timeout}s",
                "command": command_string(adapter.command),
                "duration_ns": now_ns() - start,
                "stdout_bytes": len((exc.stdout or "").encode()),
                "stderr_bytes": len((exc.stderr or "").encode()),
                "metrics": [],
            },
        )


def import_csv_rows(
    output: Path,
    source: str,
    path: Path,
    category: str = "external",
    method: str | None = None,
    default_unit: str = "count",
) -> None:
    if not path.exists():
        emit(
            output,
            {
                "event": "external_benchmark_import",
                "source": source,
                "category": category,
                "status": "skipped",
                "detail": f"{path} not found",
                "metrics": [],
            },
        )
        return

    with path.open(newline="", encoding="utf-8") as handle:
        reader = csv.DictReader(handle)
        for index, row in enumerate(reader):
            emit(
                output,
                {
                    "event": "external_benchmark_import",
                    "source": source,
                    "source_csv": str(path),
                    "terminal": csv_value(row, "terminal", "emulator", "application", "app") or source,
                    "profile": csv_value(row, "profile", "config", "configuration") or "imported",
                    "benchmark": csv_value(row, "benchmark", "case", "scenario", "workload", "test", "fixture") or source,
                    "category": category,
                    "latency_method": method,
                    "capture_method": csv_value(row, "capture_method", "method"),
                    "status": csv_status(csv_value(row, "status", "result")),
                    "row_index": index,
                    "run_index": csv_int(row, "run_index", "sample", "sample_index"),
                    "raw": row,
                    "metrics": imported_metrics(row, default_unit),
                },
            )


CSV_METADATA_COLUMNS = {
    "app",
    "application",
    "benchmark",
    "capture_method",
    "case",
    "config",
    "configuration",
    "emulator",
    "fixture",
    "method",
    "notes",
    "profile",
    "result",
    "run_index",
    "sample",
    "sample_index",
    "scenario",
    "status",
    "terminal",
    "test",
    "timestamp",
    "workload",
}


def csv_key(name: str) -> str:
    return re.sub(r"[^a-z0-9]+", "_", name.lower()).strip("_")


def csv_value(row: dict[str, str], *names: str) -> str | None:
    wanted = {csv_key(name) for name in names}
    for key, value in row.items():
        if csv_key(key) in wanted and value is not None and value.strip():
            return value.strip()
    return None


def csv_int(row: dict[str, str], *names: str) -> int | None:
    value = csv_value(row, *names)
    if value is None or not re.fullmatch(r"\d+", value):
        return None
    return int(value)


def csv_status(value: str | None) -> str:
    if value is None:
        return "pass"
    status = csv_key(value)
    if status in {"pass", "passed", "ok", "valid"}:
        return "pass"
    if status in {"fail", "failed", "error"}:
        return "fail"
    if status in {"skip", "skipped"}:
        return "skip"
    if status == "timeout":
        return "timeout"
    if status == "unsupported":
        return "unsupported"
    if status == "invalidated":
        return "invalidated"
    return "pass"


def metric_unit(header: str, default_unit: str) -> str:
    key = csv_key(header)
    if key.endswith("_ns") or key.endswith("ns"):
        return "ns"
    if key.endswith("_us") or key.endswith("us") or key.endswith("microseconds"):
        return "us"
    if key.endswith("_s") or key.endswith("seconds"):
        return "s"
    if key.endswith("_ms") or key.endswith("ms"):
        return "ms"
    if "fps" in key:
        return "fps"
    if "bytes" in key:
        return "bytes"
    if "latency" in key:
        return default_unit if default_unit != "count" else "ms"
    if default_unit != "count" and key in {"mean", "median", "p50", "p90", "p95", "p99", "max", "min", "stddev"}:
        return default_unit
    return "count"


def imported_metrics(row: dict[str, str], default_unit: str = "count") -> list[dict]:
    metrics: list[dict] = []
    for key, value in row.items():
        if csv_key(key) in CSV_METADATA_COLUMNS or value is None:
            continue
        stripped = value.strip()
        if not re.fullmatch(r"-?\d+(?:\.\d+)?", stripped):
            continue
        metrics.append(
            {
                "metric": csv_key(key) or key,
                "value": float(stripped),
                "unit": metric_unit(key, default_unit),
            }
        )
    return metrics


def adapter_list(args: argparse.Namespace) -> Iterable[Adapter]:
    yield Adapter("vtebench", shlex.split(args.vtebench_cmd), "pty_parser_throughput")
    yield Adapter("kitty_benchmark", shlex.split(args.kitty_cmd), "throughput")
    yield Adapter("kitty_benchmark_render", shlex.split(args.kitty_render_cmd), "render_throughput")
    yield Adapter("termbench", shlex.split(args.termbench_cmd), "sink_throughput")
    if args.typometer_cmd:
        yield Adapter("typometer", shlex.split(args.typometer_cmd), "software_visual_latency")
    if args.moktavizen_cmd:
        yield Adapter("moktavizen_wayland", shlex.split(args.moktavizen_cmd), "wayland_competitive")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", default="artifacts/external-benchmarks/results.jsonl")
    parser.add_argument("--run", action="store_true", help="execute public benchmark commands")
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT_SECONDS)
    parser.add_argument(
        "--vtebench-cmd",
        default=os.environ.get(
            "BOOTTY_VTEBENCH_CMD",
            public_benchmark_command("vtebench", "vtebench", "0.3.1"),
        ),
    )
    parser.add_argument(
        "--kitty-cmd",
        default=os.environ.get("BOOTTY_KITTY_BENCH_CMD", "kitty +kitten __benchmark__"),
    )
    parser.add_argument(
        "--kitty-render-cmd",
        default=os.environ.get("BOOTTY_KITTY_RENDER_BENCH_CMD", "kitty +kitten __benchmark__ --render"),
    )
    parser.add_argument(
        "--termbench-cmd",
        default=os.environ.get(
            "BOOTTY_TERMBENCH_CMD",
            public_benchmark_command("termbench", "termbench", "0.1.1"),
        ),
    )
    parser.add_argument("--typometer-cmd", default=os.environ.get("BOOTTY_TYPOMETER_CMD", ""))
    parser.add_argument("--moktavizen-cmd", default=os.environ.get("BOOTTY_MOKTAVIZEN_CMD", ""))
    parser.add_argument("--typometer-csv", type=Path)
    parser.add_argument("--software-latency-csv", type=Path)
    parser.add_argument("--hardware-latency-csv", type=Path)
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text("", encoding="utf-8")
    emit(output, metadata_row())
    for row in terminal_inventory():
        emit(output, row)


    for adapter in adapter_list(args):
        run_adapter(adapter, output, args.run, args.timeout)
    if args.typometer_csv:
        import_csv_rows(
            output,
            "typometer_csv",
            args.typometer_csv,
            category="input_latency",
            method="typometer_software_visual",
            default_unit="ms",
        )
    if args.software_latency_csv:
        import_csv_rows(
            output,
            "software_latency_csv",
            args.software_latency_csv,
            category="input_latency",
            method="software_event_visual",
            default_unit="ms",
        )
    if args.hardware_latency_csv:
        import_csv_rows(
            output,
            "hardware_latency_csv",
            args.hardware_latency_csv,
            category="input_latency",
            method="hardware_key_to_pixel",
            default_unit="ms",
        )

    print(f"Wrote external benchmark adapter results: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
