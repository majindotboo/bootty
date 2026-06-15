#!/usr/bin/env python3
"""Run public PTY benchmarks inside terminal emulators.

This is the competitive path for tools such as vtebench and termbench. Running
those tools directly from an agent shell measures a pipe, not a terminal. This
runner launches each benchmark as the child process of a terminal emulator,
waits for the tool to write a .dat file, then terminates GUI processes that do
not exit on child completion.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Iterable

SCHEMA_VERSION = 1
DEFAULT_TIMEOUT_SECONDS = 90
TARGET_COLUMNS = 80
TARGET_LINES = 24



class Terminal:
    def __init__(self, name: str) -> None:
        self.name = name


class Tool:
    def __init__(self, name: str, binary: str, package: str, version: str) -> None:
        self.name = name
        self.binary = binary
        self.package = package
        self.version = version


class ResourceSamples:
    def __init__(self) -> None:
        self.count = 0
        self.cpu_total = 0.0
        self.cpu_max = 0.0
        self.rss_max_bytes = 0

    def add(self, cpu_percent: float, rss_bytes: int) -> None:
        self.count += 1
        self.cpu_total += cpu_percent
        self.cpu_max = max(self.cpu_max, cpu_percent)
        self.rss_max_bytes = max(self.rss_max_bytes, rss_bytes)

    def metrics(self) -> list[dict]:
        if self.count == 0:
            return []
        return [
            {"benchmark": "process", "metric": "resource_sample_count", "value": float(self.count), "unit": "count"},
            {"benchmark": "process", "metric": "mean_cpu_percent", "value": self.cpu_total / self.count, "unit": "percent"},
            {"benchmark": "process", "metric": "max_cpu_percent", "value": self.cpu_max, "unit": "percent"},
            {"benchmark": "process", "metric": "max_rss_bytes", "value": float(self.rss_max_bytes), "unit": "bytes"},
        ]


class CorrectnessResult:
    def __init__(self, status: str, detail: str, artifact: str | None = None) -> None:
        self.status = status
        self.detail = detail
        self.artifact = artifact

    def invalidates_benchmark(self) -> bool:
        return self.status in {"fail", "unsupported", "invalidated"}


class CatchUpResult:
    def __init__(
        self,
        status: str,
        detail: str,
        artifact: str | None = None,
        response_time_ns: int | None = None,
        sentinel: str | None = None,
    ) -> None:
        self.status = status
        self.detail = detail
        self.artifact = artifact
        self.response_time_ns = response_time_ns
        self.sentinel = sentinel

    def metrics(self) -> list[dict]:
        if self.response_time_ns is None:
            return []
        return [
            {
                "benchmark": "terminal_response_catch_up",
                "metric": "post_producer_response_time",
                "value": float(self.response_time_ns),
                "unit": "ns",
            }
        ]


CORRECTNESS_PROBE = r'''#!/usr/bin/env python3
import json
import os
import re
import select
import sys
import termios
import time
import tty


def write_result(path, status, detail, checks):
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(
            {
                "schema_version": 1,
                "event": "terminal_correctness_probe",
                "status": status,
                "detail": detail,
                "checks": checks,
            },
            handle,
            sort_keys=True,
        )
        handle.write("\n")


def drain(fd):
    while True:
        ready, _, _ = select.select([fd], [], [], 0)
        if not ready:
            return
        if not os.read(fd, 4096):
            return


def query(fd_in, fd_out, name, sequence, pattern, timeout=1.0):
    os.write(fd_out, sequence)
    deadline = time.monotonic() + timeout
    data = b""
    while time.monotonic() < deadline:
        ready, _, _ = select.select([fd_in], [], [], max(0.0, deadline - time.monotonic()))
        if not ready:
            break
        chunk = os.read(fd_in, 4096)
        if not chunk:
            break
        data += chunk
        if re.search(pattern, data):
            return {"name": name, "status": "pass", "response": data.decode("ascii", "replace")[:120]}
    return {"name": name, "status": "fail", "response": data.decode("ascii", "replace")[:120]}


def main():
    result_path = sys.argv[1]
    fd_in = sys.stdin.fileno()
    fd_out = sys.stdout.fileno()
    if not os.isatty(fd_in) or not os.isatty(fd_out):
        write_result(result_path, "unsupported", "stdin/stdout are not a pty", [])
        return 2

    old = termios.tcgetattr(fd_in)
    checks = []
    try:
        tty.setraw(fd_in)
        drain(fd_in)
        os.write(fd_out, b"\x1b[?1049h\x1b[2J\x1b[H")
        checks.append(query(fd_in, fd_out, "cursor_origin_dsr", b"\x1b[6n", rb"\x1b\[1;1R"))
        os.write(fd_out, b"\x1b[7;11H")
        checks.append(query(fd_in, fd_out, "cursor_address_dsr", b"\x1b[6n", rb"\x1b\[7;11R"))
        checks.append(query(fd_in, fd_out, "primary_device_attributes", b"\x1b[c", rb"\x1b\[[?=>]?[0-9;]*c"))
        passed = all(check["status"] == "pass" for check in checks)
        status = "pass" if passed else "fail"
        detail = "ok" if passed else "one or more terminal response checks failed"
        write_result(result_path, status, detail, checks)
        return 0 if passed else 1
    except Exception as error:
        write_result(result_path, "fail", repr(error), checks)
        return 1
    finally:
        try:
            os.write(fd_out, b"\x1b[?1049l")
        finally:
            termios.tcsetattr(fd_in, termios.TCSANOW, old)


if __name__ == "__main__":
    raise SystemExit(main())
'''


POST_PRODUCER_CATCH_UP_PROBE = r'''#!/usr/bin/env python3
import json
import os
import re
import select
import sys
import termios
import time
import tty
import uuid


DSR_PATTERN = re.compile(rb"\x1b\[[0-9]+;[0-9]+R")


def write_result(path, **fields):
    row = {
        "schema_version": 1,
        "event": "terminal_post_producer_catch_up_probe",
    }
    row.update(fields)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(row, handle, sort_keys=True)
        handle.write("\n")


def drain(fd):
    while True:
        ready, _, _ = select.select([fd], [], [], 0)
        if not ready:
            return
        if not os.read(fd, 4096):
            return


def wait_for_dsr(fd, timeout_sec):
    deadline = time.monotonic() + timeout_sec
    data = b""
    while time.monotonic() < deadline:
        ready, _, _ = select.select([fd], [], [], max(0.0, deadline - time.monotonic()))
        if not ready:
            break
        chunk = os.read(fd, 4096)
        if not chunk:
            break
        data += chunk
        if DSR_PATTERN.search(data):
            return data, time.monotonic_ns()
    return data, None


def main():
    result_path = sys.argv[1]
    timeout_ms = int(sys.argv[2]) if len(sys.argv) > 2 else 2000
    token = sys.argv[3] if len(sys.argv) > 3 else uuid.uuid4().hex[:12]
    timeout_sec = max(1, timeout_ms) / 1000.0
    fd_in = sys.stdin.fileno()
    fd_out = sys.stdout.fileno()
    sentinel = f"BOOTTY-BENCH-CATCHUP-{token}"
    if not os.isatty(fd_in) or not os.isatty(fd_out):
        write_result(
            result_path,
            status="unsupported",
            detail="stdin/stdout are not a pty",
            sentinel=sentinel,
            timeout_ms=timeout_ms,
        )
        return 2

    old = termios.tcgetattr(fd_in)
    try:
        tty.setraw(fd_in)
        drain(fd_in)
        start_ns = time.monotonic_ns()
        writable = select.select([], [fd_out], [], timeout_sec)[1]
        if not writable:
            write_result(
                result_path,
                status="fail",
                detail="timed out waiting for pty output readiness",
                sentinel=sentinel,
                start_ns=start_ns,
                timeout_ms=timeout_ms,
            )
            return 1
        sentinel_bytes = sentinel.encode("ascii")
        os.write(fd_out, b"\r\n\x1b[7m" + sentinel_bytes + b"\x1b[0m\r\n")
        os.write(fd_out, b"\x1b[6n")
        response, response_ns = wait_for_dsr(fd_in, timeout_sec)
        response_text = response.decode("ascii", "replace")[:160]
        if response_ns is None:
            write_result(
                result_path,
                status="fail",
                detail="timed out waiting for DSR response",
                sentinel=sentinel,
                start_ns=start_ns,
                timeout_ms=timeout_ms,
                response=response_text,
            )
            return 1
        write_result(
            result_path,
            status="pass",
            detail="ok",
            sentinel=sentinel,
            start_ns=start_ns,
            response_ns=response_ns,
            response_time_ns=response_ns - start_ns,
            timeout_ms=timeout_ms,
            response=response_text,
        )
        return 0
    except Exception as error:
        write_result(
            result_path,
            status="fail",
            detail=repr(error),
            sentinel=sentinel,
            timeout_ms=timeout_ms,
        )
        return 1
    finally:
        termios.tcsetattr(fd_in, termios.TCSANOW, old)


if __name__ == "__main__":
    raise SystemExit(main())
'''


def write_correctness_probe(path: Path) -> None:
    path.write_text(CORRECTNESS_PROBE, encoding="utf-8")
    path.chmod(0o755)


def write_catch_up_probe(path: Path) -> None:
    path.write_text(POST_PRODUCER_CATCH_UP_PROBE, encoding="utf-8")
    path.chmod(0o755)


def emit(path: Path, row: dict) -> None:
    row.setdefault("schema_version", SCHEMA_VERSION)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(row, sort_keys=True) + "\n")


def command_string(argv: list[str]) -> str:
    return " ".join(argv)


def cargo_package_benchmarks(package: str, version: str) -> Path | None:
    registry_src = Path.home() / ".cargo" / "registry" / "src"
    if not registry_src.exists():
        return None
    for candidate in sorted(registry_src.glob(f"*/{package}-{version}/benchmarks")):
        if candidate.is_dir():
            return candidate
    return None


def bootty_executable() -> str | None:
    path_bootty = shutil.which("bootty")
    if path_bootty:
        return path_bootty
    release_bootty = Path("target/release/bootty")
    if release_bootty.exists() and os.access(release_bootty, os.X_OK):
        return str(release_bootty)
    return None


def terminal_version(name: str) -> tuple[str, str | None, str | None]:
    if name == "bootty":
        bootty = bootty_executable()
        if not bootty:
            return "missing", None, None
        return "present", bootty, None

    commands = {
        "kitty": ["kitty", "--version"],
        "alacritty": ["alacritty", "--version"],
        "wezterm": ["wezterm", "--version"],
        "ghostty": ["ghostty", "+version"],
    }
    command = commands[name]
    executable = command[0]
    resolved = executable if executable and Path(executable).exists() else shutil.which(executable or "")
    if not resolved:
        return "missing", None, None
    try:
        completed = subprocess.run(command, capture_output=True, text=True, timeout=5, check=False)
    except subprocess.TimeoutExpired:
        return "timeout", resolved, None
    output = " ".join(
        line.strip()
        for line in (completed.stdout + "\n" + completed.stderr).splitlines()
        if line.strip()
    )
    return "present", resolved, output[:240]


def write_wrapper(
    path: Path,
    command: list[str],
    cwd: Path,
    transcript: Path,
    done: Path,
    rc_path: Path,
    pty_size_path: Path,
    catch_up_probe: Path | None = None,
    catch_up_result: Path | None = None,
    catch_up_timeout_ms: int = 2000,
    catch_up_token: str | None = None,
) -> None:
    quoted_command = " ".join(sh_quote(part) for part in command)
    catch_up_script = ""
    if catch_up_probe is not None and catch_up_result is not None:
        token = catch_up_token or done.stem
        catch_up_script = (
            f"{sh_quote(sys.executable)} {sh_quote(str(catch_up_probe))} "
            f"{sh_quote(str(catch_up_result))} {sh_quote(str(catch_up_timeout_ms))} "
            f"{sh_quote(token)} 2>> {sh_quote(str(transcript))} || true\n"
        )
    script = f"""#!/bin/sh
cd {sh_quote(str(cwd))} || exit 127
export COLUMNS={TARGET_COLUMNS}
export LINES={TARGET_LINES}
tty=\"/dev/$(ps -o tty= -p $$ | tr -d ' ')\"
stty size < \"$tty\" > {sh_quote(str(pty_size_path))} 2>/dev/null || true
{quoted_command} 2> {sh_quote(str(transcript))}
rc=$?
printf '%s\n' "$rc" > {sh_quote(str(rc_path))}
{catch_up_script}touch {sh_quote(str(done))}
exit "$rc"
"""
    path.write_text(script, encoding="utf-8")
    path.chmod(0o755)


def sh_quote(value: str) -> str:
    return "'" + value.replace("'", "'\\''") + "'"


BOOTTY_BENCH_CONFIG = """version = 1

[window]
title = "Bootty benchmark"
width = 560
height = 565
fullscreen = false
window-decoration = "auto"
macos-titlebar-style = "transparent"

[chrome]
sidebar = false
status-bar = false
window-tabs = false
sidebar-width = 0
status-height = 0
gap = 0
unfocused-sidebar-dim = 0
unfocused-terminal-dim = 0

[session]
max-scrollback = 0
"""


def write_bootty_benchmark_config(temp: Path) -> Path:
    config_dir = temp / "xdg" / "bootty"
    config_dir.mkdir(parents=True, exist_ok=True)
    config_path = config_dir / "config.toml"
    config_path.write_text(BOOTTY_BENCH_CONFIG, encoding="utf-8")
    return config_path


def benchmark_profile(terminal: str, use_user_bootty_config: bool) -> str:
    if terminal == "bootty" and use_user_bootty_config:
        return "user"
    return f"normalized-{TARGET_COLUMNS}x{TARGET_LINES}"

def bootty_config_profile(terminal: str, use_user_bootty_config: bool) -> str | None:
    if terminal != "bootty":
        return None
    return "user" if use_user_bootty_config else "benchmark-normalized"


def launch_terminal(
    terminal: str,
    wrapper: Path,
    cwd: Path,
    temp: Path,
    use_user_bootty_config: bool,
) -> tuple[list[str], dict[str, str], bool, str | None, bool]:
    env = os.environ.copy()
    if terminal == "bootty":
        bootty = bootty_executable()
        if not bootty:
            return [], env, False, "bootty binary not found; build target/release/bootty or put bootty on PATH", True
        env["BOOTTY_SHELL"] = str(wrapper)
        if not use_user_bootty_config:
            config_path = write_bootty_benchmark_config(temp)
            env["XDG_CONFIG_HOME"] = str(config_path.parent.parent)
        return [bootty], env, True, None, True
    if terminal == "kitty":
        if not shutil.which("kitty"):
            return [], env, False, "kitty not found", True
        return [
            "kitty",
            "--config",
            "NONE",
            "--override",
            f"initial_window_width={TARGET_COLUMNS}c",
            "--override",
            f"initial_window_height={TARGET_LINES}c",
            "--override",
            "remember_window_size=no",
            "--directory",
            str(cwd),
            str(wrapper),
        ], env, True, None, True
    if terminal == "alacritty":
        if not shutil.which("alacritty"):
            return [], env, False, "alacritty not found", True
        alacritty_config = temp / "alacritty.toml"
        alacritty_config.write_text("", encoding="utf-8")
        return [
            "alacritty",
            "--config-file",
            str(alacritty_config),
            "--option",
            f"window.dimensions.columns={TARGET_COLUMNS}",
            "--option",
            f"window.dimensions.lines={TARGET_LINES}",
            "--working-directory",
            str(cwd),
            "-e",
            str(wrapper),
        ], env, True, None, True
    if terminal == "wezterm":
        if not shutil.which("wezterm"):
            return [], env, False, "wezterm not found", True
        return [
            "wezterm",
            "--skip-config",
            "--config",
            f"initial_cols={TARGET_COLUMNS}",
            "--config",
            f"initial_rows={TARGET_LINES}",
            "--config",
            "enable_tab_bar=false",
            "start",
            "--always-new-process",
            "--no-auto-connect",
            "--cwd",
            str(cwd),
            "--",
            str(wrapper),
        ], env, True, None, True
    if terminal == "ghostty":
        if not shutil.which("ghostty"):
            return [], env, False, "ghostty not found", True
        if sys.platform == "darwin":
            ghostty_config = temp / "ghostty.config"
            ghostty_config.write_text(
                "\n".join(
                    [
                        f"window-width = {TARGET_COLUMNS}",
                        f"window-height = {TARGET_LINES}",
                        "window-save-state = never",
                        "window-decoration = false",
                        "shell-integration = none",
                        "confirm-close-surface = false",
                        "quit-after-last-window-closed = true",
                        "wait-after-command = false",
                        f"working-directory = {cwd}",
                        f"initial-command = {wrapper}",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            return [
                "open",
                "-na",
                "Ghostty.app",
                "--args",
                f"--config-file={ghostty_config}",
            ], env, True, None, False
        return ["ghostty", "-e", str(wrapper)], env, True, None, True
    return [], env, False, f"unknown terminal {terminal}", True


def sample_process_resources(pid: int) -> tuple[float, int] | None:
    try:
        completed = subprocess.run(
            ["ps", "-o", "%cpu=", "-o", "rss=", "-p", str(pid)],
            capture_output=True,
            text=True,
            timeout=1,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if completed.returncode != 0:
        return None
    fields = completed.stdout.split()
    if len(fields) < 2:
        return None
    try:
        cpu_percent = float(fields[0])
        rss_bytes = int(fields[1]) * 1024
    except ValueError:
        return None
    return cpu_percent, rss_bytes


def sample_resources(samples: ResourceSamples | None, proc: subprocess.Popen) -> None:
    if samples is None:
        return
    sample = sample_process_resources(proc.pid)
    if sample is not None:
        samples.add(*sample)
def wait_for_done(
    done: Path,
    proc: subprocess.Popen,
    timeout: int,
    samples: ResourceSamples | None,
    fail_on_process_exit: bool,
) -> str:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        sample_resources(samples, proc)
        if done.exists():
            return "completed"
        if proc.poll() is not None and fail_on_process_exit:
            return "process_exited"
        time.sleep(0.1)
    return "timeout"


def stop_process(proc: subprocess.Popen) -> None:
    if proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


def parse_dat(path: Path) -> list[dict]:
    if not path.exists():
        return []
    lines = [line.strip() for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    if not lines:
        return []
    names = lines[0].split()
    samples: dict[str, list[float]] = {name: [] for name in names}
    for line in lines[1:]:
        for name, value in zip(names, line.split()):
            if value == "_":
                continue
            try:
                samples[name].append(float(value))
            except ValueError:
                continue

    metrics: list[dict] = []
    for name, values in samples.items():
        if not values:
            continue
        sorted_values = sorted(values)
        metrics.extend(
            [
                {"benchmark": name, "metric": "sample_count", "value": float(len(values)), "unit": "count"},
                {"benchmark": name, "metric": "min_sample_time", "value": sorted_values[0], "unit": "ms"},
                {"benchmark": name, "metric": "mean_sample_time", "value": statistics.fmean(values), "unit": "ms"},
                {"benchmark": name, "metric": "p50_sample_time", "value": percentile(sorted_values, 50), "unit": "ms"},
                {"benchmark": name, "metric": "p90_sample_time", "value": percentile(sorted_values, 90), "unit": "ms"},
                {"benchmark": name, "metric": "max_sample_time", "value": sorted_values[-1], "unit": "ms"},
            ]
        )
        if len(values) > 1:
            metrics.append(
                {
                    "benchmark": name,
                    "metric": "stddev_sample_time",
                    "value": statistics.stdev(values),
                    "unit": "ms",
                }
            )
    return metrics


def read_actual_pty_size(path: Path) -> str | None:
    if not path.exists():
        return None
    return path.read_text(encoding="utf-8").strip()


def expected_pty_size() -> str:
    return f"{TARGET_LINES} {TARGET_COLUMNS}"


def validate_pty_size(status: str, detail: str, actual_size: str | None) -> tuple[str, str]:
    if status != "pass":
        return status, detail
    if actual_size != expected_pty_size():
        return "invalidated", f"actual_pty_size={actual_size!r} expected={expected_pty_size()!r}"
    return status, detail


def parse_bootty_trace_metrics(trace: Path) -> list[dict]:
    if not trace.exists():
        return []
    last_parse_done_ns: int | None = None
    last_frame_presented_ns: int | None = None
    frame_count = 0
    parse_count = 0
    with trace.open(encoding="utf-8") as handle:
        for line in handle:
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                continue
            ts_ns = row.get("ts_ns")
            if not isinstance(ts_ns, int):
                continue
            event = row.get("event")
            if event == "parse_done":
                last_parse_done_ns = ts_ns
                parse_count += 1
            elif event == "frame_presented":
                last_frame_presented_ns = ts_ns
                frame_count += 1
    metrics = [
        {"benchmark": "bootty_trace", "metric": "parse_done_count", "value": float(parse_count), "unit": "count"},
        {"benchmark": "bootty_trace", "metric": "frame_presented_count", "value": float(frame_count), "unit": "count"},
    ]
    if last_parse_done_ns is not None and last_frame_presented_ns is not None:
        metrics.append(
            {
                "benchmark": "bootty_trace",
                "metric": "visual_catch_up_time",
                "value": float(max(0, last_frame_presented_ns - last_parse_done_ns)),
                "unit": "ns",
            }
        )
    return metrics


def read_correctness_result(path: Path) -> CorrectnessResult:
    if not path.exists():
        return CorrectnessResult("fail", "correctness probe did not write a result", str(path))
    try:
        row = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        return CorrectnessResult("fail", f"invalid correctness result: {error}", str(path))
    status = row.get("status")
    detail = row.get("detail")
    if status not in {"pass", "fail", "skip", "unsupported", "invalidated"}:
        return CorrectnessResult("fail", f"invalid correctness status: {status!r}", str(path))
    return CorrectnessResult(status, str(detail or status), str(path))


def read_catch_up_result(path: Path) -> CatchUpResult:
    if not path.exists():
        return CatchUpResult("skip", "catch-up probe did not run", str(path))
    try:
        row = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        return CatchUpResult("fail", f"invalid catch-up result: {error}", str(path))
    status = row.get("status")
    detail = row.get("detail")
    if status not in {"pass", "fail", "skip", "unsupported", "invalidated"}:
        return CatchUpResult("fail", f"invalid catch-up status: {status!r}", str(path))
    response_time_ns = row.get("response_time_ns")
    if not isinstance(response_time_ns, int):
        response_time_ns = None
    sentinel = row.get("sentinel")
    return CatchUpResult(status, str(detail or status), str(path), response_time_ns, sentinel)


def run_correctness_gate(args: argparse.Namespace, output: Path, terminal: Terminal) -> CorrectnessResult:
    if args.skip_correctness_gate or args.dry_run:
        return CorrectnessResult("skip", "correctness gate skipped")

    term_status, executable, version = terminal_version(terminal.name)
    if term_status != "present":
        return CorrectnessResult("skip", f"terminal {term_status}")

    run_dir = output.parent / "terminal-public" / terminal.name / "correctness"
    run_dir.mkdir(parents=True, exist_ok=True)
    result_path = run_dir / "result.json"
    transcript = run_dir / "transcript.txt"
    done = run_dir / "done"
    rc_path = run_dir / "exit-code.txt"
    pty_size_path = run_dir / "pty-size.txt"
    for stale in [result_path, transcript, done, rc_path, pty_size_path]:
        stale.unlink(missing_ok=True)

    with tempfile.TemporaryDirectory(prefix="bootty-terminal-correctness-") as temp:
        temp_path = Path(temp)
        probe = temp_path / "correctness-probe.py"
        wrapper = temp_path / "run-correctness.sh"
        write_correctness_probe(probe)
        write_wrapper(
            wrapper,
            [sys.executable, str(probe), str(result_path)],
            Path.cwd(),
            transcript,
            done,
            rc_path,
            pty_size_path,
        )
        launch_argv, env, launchable, skip_reason, fail_on_process_exit = launch_terminal(
            terminal.name,
            wrapper,
            Path.cwd(),
            temp_path,
            args.use_user_bootty_config,
        )
        if not launchable:
            return CorrectnessResult("unsupported", skip_reason or "terminal not launchable")
        proc = subprocess.Popen(launch_argv, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        wait_status = wait_for_done(done, proc, args.timeout, None, fail_on_process_exit)
        exit_code = None
        if rc_path.exists():
            try:
                exit_code = int(rc_path.read_text(encoding="utf-8").strip())
            except ValueError:
                exit_code = None
        stop_process(proc)

    result = read_correctness_result(result_path)
    actual_pty_size = read_actual_pty_size(pty_size_path)
    status, detail = validate_pty_size(result.status, result.detail, actual_pty_size)
    if wait_status != "completed" or exit_code not in (0, None):
        status = "fail" if status == "pass" else status
        detail = f"{detail}; wait={wait_status} exit={exit_code}"
    result = CorrectnessResult(status, detail, str(result_path))
    emit(
        output,
        {
            "event": "terminal_correctness_gate",
            "terminal": terminal.name,
            "terminal_version": version,
            "terminal_executable": executable,
            "status": result.status,
            "detail": result.detail,
            "command": command_string(launch_argv),
            "profile": benchmark_profile(terminal.name, args.use_user_bootty_config),
            "bootty_config_profile": bootty_config_profile(terminal.name, args.use_user_bootty_config),
            "target_pty_columns": TARGET_COLUMNS,
            "target_pty_lines": TARGET_LINES,
            "actual_pty_size": actual_pty_size,
            "raw_artifact": result.artifact,
            "transcript_path": str(transcript),
        },
    )
    return result


def apply_correctness_status(
    status: str,
    detail: str,
    correctness: CorrectnessResult,
) -> tuple[str, str]:
    if status == "pass" and correctness.invalidates_benchmark():
        return "invalidated", f"{detail}; correctness={correctness.status}: {correctness.detail}"
    return status, detail


def percentile(sorted_values: list[float], pct: int) -> float:
    if not sorted_values:
        return 0.0
    index = min(len(sorted_values) - 1, max(0, int(round((pct / 100) * (len(sorted_values) - 1)))))
    return sorted_values[index]


def run_one(
    args: argparse.Namespace,
    output: Path,
    terminal: Terminal,
    tool: Tool,
    correctness: CorrectnessResult,
) -> None:
    term_status, executable, version = terminal_version(terminal.name)
    if term_status != "present":
        emit(
            output,
            {
                "event": "terminal_public_benchmark",
                "terminal": terminal.name,
                "tool": tool.name,
                "benchmark": tool.name,
                "category": "pty",
                "status": "skipped",
                "detail": f"terminal {term_status}",
                "metrics": [],
                "correctness_status": correctness.status,
                "correctness_detail": correctness.detail,
                "correctness_artifact": correctness.artifact,
            },
        )
        return

    tool_binary = shutil.which(tool.binary)
    benchmark_dir = cargo_package_benchmarks(tool.package, tool.version)
    if not tool_binary or benchmark_dir is None:
        emit(
            output,
            {
                "event": "terminal_public_benchmark",
                "terminal": terminal.name,
                "terminal_version": version,
                "terminal_executable": executable,
                "tool": tool.name,
                "benchmark": tool.name,
                "category": "pty",
                "status": "skipped",
                "detail": "benchmark binary or fixture directory missing",
                "metrics": [],
                "correctness_status": correctness.status,
                "correctness_detail": correctness.detail,
                "correctness_artifact": correctness.artifact,
            },
        )
        return

    run_dir = output.parent / "terminal-public" / terminal.name / tool.name
    run_dir.mkdir(parents=True, exist_ok=True)
    dat = run_dir / "results.dat"
    transcript = run_dir / "transcript.txt"
    done = run_dir / "done"
    rc_path = run_dir / "exit-code.txt"
    trace = run_dir / "bootty-trace.jsonl"
    pty_size_path = run_dir / "pty-size.txt"
    catch_up_path = run_dir / "post-producer-catch-up.json"
    for stale in [dat, transcript, done, rc_path, trace, pty_size_path, catch_up_path]:
        stale.unlink(missing_ok=True)

    with tempfile.TemporaryDirectory(prefix="bootty-terminal-bench-") as temp:
        wrapper = Path(temp) / "run-benchmark.sh"
        benchmark_command = [
            tool_binary,
            "--benchmarks",
            str(benchmark_dir),
            "--max-samples",
            str(args.max_samples),
            "--max-secs",
            str(args.max_secs),
            "--min-bytes",
            str(args.min_bytes),
            "--dat",
            str(dat),
        ]
        catch_up_probe = None
        catch_up_token = None
        if not args.skip_catch_up_probe:
            catch_up_probe = temp_path / "post-producer-catch-up.py"
            write_catch_up_probe(catch_up_probe)
            catch_up_token = f"{terminal.name}-{tool.name}-{time.time_ns()}"
        write_wrapper(
            wrapper,
            benchmark_command,
            Path.cwd(),
            transcript,
            done,
            rc_path,
            pty_size_path,
            catch_up_probe=catch_up_probe,
            catch_up_result=None if args.skip_catch_up_probe else catch_up_path,
            catch_up_timeout_ms=args.catch_up_timeout_ms,
            catch_up_token=catch_up_token,
        )
        launch_argv, env, launchable, skip_reason, fail_on_process_exit = launch_terminal(
            terminal.name,
            wrapper,
            Path.cwd(),
            temp_path,
            args.use_user_bootty_config,
        )
        if not launchable:
            emit(
                output,
                {
                    "event": "terminal_public_benchmark",
                    "terminal": terminal.name,
                    "terminal_version": version,
                    "terminal_executable": executable,
                    "tool": tool.name,
                    "benchmark": tool.name,
                    "category": "pty",
                    "status": "unsupported",
                    "detail": skip_reason,
                    "metrics": [],
                    "correctness_status": correctness.status,
                    "correctness_detail": correctness.detail,
                    "correctness_artifact": correctness.artifact,
                },
            )
            return

        if terminal.name == "bootty" and args.bootty_trace:
            env["BOOTTY_BENCH_TRACE"] = str(trace)

        start = time.time_ns()
        if args.dry_run:
            emit(
                output,
                {
                    "event": "terminal_public_benchmark",
                    "terminal": terminal.name,
                    "terminal_version": version,
                    "terminal_executable": executable,
                    "tool": tool.name,
                    "benchmark": tool.name,
                    "category": "pty",
                    "status": "skipped",
                    "detail": "dry run",
                    "command": command_string(launch_argv),
                    "benchmark_command": command_string(benchmark_command),
                    "profile": benchmark_profile(terminal.name, args.use_user_bootty_config),
                    "bootty_config_profile": bootty_config_profile(terminal.name, args.use_user_bootty_config),
                    "trace_path": str(trace) if terminal.name == "bootty" and args.bootty_trace else None,
                    "metrics": [],
                    "correctness_status": correctness.status,
                    "correctness_detail": correctness.detail,
                    "correctness_artifact": correctness.artifact,
                    "catch_up_status": "skip",
                    "catch_up_detail": "dry run",
                    "catch_up_artifact": None,
                    "catch_up_sentinel": None,
                },
            )
            return

        resource_samples = ResourceSamples() if args.resource_sample else None
        proc = subprocess.Popen(launch_argv, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        wait_status = wait_for_done(done, proc, args.timeout, resource_samples, fail_on_process_exit)
        if wait_status == "completed" and args.post_completion_grace_ms > 0:
            time.sleep(args.post_completion_grace_ms / 1000)
        duration_ns = time.time_ns() - start
        exit_code = None
        if rc_path.exists():
            try:
                exit_code = int(rc_path.read_text(encoding="utf-8").strip())
            except ValueError:
                exit_code = None
        stop_process(proc)

    catch_up = (
        CatchUpResult("skip", "catch-up probe skipped")
        if args.skip_catch_up_probe
        else read_catch_up_result(catch_up_path)
    )
    benchmark_metrics = parse_dat(dat)
    metrics = [*benchmark_metrics, *catch_up.metrics()]
    if resource_samples is not None:
        metrics.extend(resource_samples.metrics())
    if terminal.name == "bootty" and args.bootty_trace:
        metrics.extend(parse_bootty_trace_metrics(trace))
    status = "pass" if wait_status == "completed" and exit_code == 0 and benchmark_metrics else "fail"
    detail = "ok" if status == "pass" else f"wait={wait_status} exit={exit_code} benchmark_metrics={len(benchmark_metrics)}"
    actual_pty_size = read_actual_pty_size(pty_size_path)
    status, detail = validate_pty_size(status, detail, actual_pty_size)
    status, detail = apply_correctness_status(status, detail, correctness)
    emit(
        output,
        {
            "event": "terminal_public_benchmark",
            "terminal": terminal.name,
            "terminal_version": version,
            "terminal_executable": executable,
            "tool": tool.name,
            "tool_version": tool.version,
            "benchmark": tool.name,
            "category": "pty",
            "status": status,
            "detail": detail,
            "correctness_status": correctness.status,
            "correctness_detail": correctness.detail,
            "correctness_artifact": correctness.artifact,
            "catch_up_status": catch_up.status,
            "catch_up_detail": catch_up.detail,
            "catch_up_artifact": catch_up.artifact,
            "catch_up_sentinel": catch_up.sentinel,
            "command": command_string(launch_argv),
            "dat_path": str(dat),
            "transcript_path": str(transcript),
            "profile": benchmark_profile(terminal.name, args.use_user_bootty_config),
            "bootty_config_profile": bootty_config_profile(terminal.name, args.use_user_bootty_config),
            "trace_path": str(trace) if terminal.name == "bootty" and args.bootty_trace else None,
            "target_pty_columns": TARGET_COLUMNS,
            "target_pty_lines": TARGET_LINES,
            "actual_pty_size": actual_pty_size,
            "duration_ns": duration_ns,
            "exit_code": exit_code,
            "metrics": metrics,
        },
    )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", default="artifacts/external-benchmarks/terminal-public.jsonl")
    parser.add_argument("--terminal", action="append", choices=["bootty", "kitty", "alacritty", "wezterm", "ghostty"])
    parser.add_argument("--tool", action="append", choices=["vtebench", "termbench"])
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT_SECONDS)
    parser.add_argument("--max-samples", type=int, default=3)
    parser.add_argument("--max-secs", type=int, default=3)
    parser.add_argument("--min-bytes", type=int, default=1048576)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument(
        "--post-completion-grace-ms",
        type=int,
        default=0,
        help="wait after the benchmark command exits before terminating the terminal",
    )
    parser.add_argument("--bootty-trace", action="store_true", help="write Bootty internal trace next to Bootty run artifacts")
    parser.add_argument("--resource-sample", action="store_true", help="sample terminal process RSS and CPU while each benchmark runs")
    parser.add_argument(
        "--use-user-bootty-config",
        action="store_true",
        help="run Bootty with the caller's normal config instead of the normalized benchmark config",
    )
    parser.add_argument(
        "--skip-correctness-gate",
        action="store_true",
        help="do not run the terminal response correctness gate before benchmark rows",
    )
    parser.add_argument(
        "--skip-catch-up-probe",
        action="store_true",
        help="do not run the post-producer terminal response catch-up probe after each benchmark",
    )
    parser.add_argument(
        "--catch-up-timeout-ms",
        type=int,
        default=2000,
        help="timeout for the post-producer catch-up probe DSR response",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text("", encoding="utf-8")
    emit(
        output,
        {
            "event": "metadata",
            "recorded_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "platform": platform.platform(),
            "cwd": os.getcwd(),
            "max_samples": args.max_samples,
            "max_secs": args.max_secs,
            "min_bytes": args.min_bytes,
            "bootty_config_profile": "user" if args.use_user_bootty_config else "benchmark-normalized",
            "target_pty_columns": TARGET_COLUMNS,
            "target_pty_lines": TARGET_LINES,
            "catch_up_probe": not args.skip_catch_up_probe,
            "catch_up_timeout_ms": args.catch_up_timeout_ms,
        },
    )

    terminals = [Terminal(name) for name in (args.terminal or ["bootty", "kitty", "alacritty", "wezterm", "ghostty"])]
    tools = {
        "vtebench": Tool("vtebench", "vtebench", "vtebench", "0.3.1"),
        "termbench": Tool("termbench", "termbench", "termbench", "0.1.1"),
    }
    selected_tools = [tools[name] for name in (args.tool or ["vtebench", "termbench"])]

    correctness_results = {
        terminal.name: run_correctness_gate(args, output, terminal) for terminal in terminals
    }

    for terminal in terminals:
        correctness = correctness_results.get(
            terminal.name, CorrectnessResult("skip", "correctness gate not run")
        )
        for tool in selected_tools:
            run_one(args, output, terminal, tool, correctness)

    print(f"Wrote terminal public benchmark results: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
