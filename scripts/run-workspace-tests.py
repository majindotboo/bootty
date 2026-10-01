#!/usr/bin/env python3
"""Run nextest with bounded disk growth; never prune artifacts."""

import argparse
import json
import math
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

GIB = 1024 ** 3


def positive_gib(value):
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise argparse.ArgumentTypeError("use a finite positive GiB value")
    return number


def disk_stop_reason(initial, available, *, reserve, budget):
    if available < reserve:
        return "free space fell below the reserve"
    if initial - available > budget:
        return "disk growth exceeded the build budget"
    return None


def stop_child(child):
    if child.poll() is not None:
        return
    if os.name == "posix":
        try:
            os.killpg(child.pid, signal.SIGTERM)
        except ProcessLookupError:
            child.wait()
            return
    else:
        child.send_signal(signal.CTRL_BREAK_EVENT)
    try:
        child.wait(timeout=10)
    except subprocess.TimeoutExpired:
        if os.name == "posix":
            os.killpg(child.pid, signal.SIGKILL)
        else:
            child.kill()
        child.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reserve-gib", type=positive_gib, default=12)
    parser.add_argument("--growth-budget-gib", type=positive_gib, default=12)
    parser.add_argument("--check-only", action="store_true")
    args, forwarded = parser.parse_known_args()
    if forwarded[:1] == ["--"]:
        forwarded = forwarded[1:]
    root = Path(__file__).resolve().parent.parent
    target = Path(os.environ.get("CARGO_TARGET_DIR", root / "target"))
    if not target.is_absolute():
        target = root / target
    existing = target
    while not existing.exists():
        existing = existing.parent
    initial = shutil.disk_usage(existing).free
    reserve, budget = int(args.reserve_gib * GIB), int(args.growth_budget_gib * GIB)
    command = ["cargo", "nextest", "run", *forwarded]
    report = {"command": command, "target_dir": str(target), "initial_free_bytes": initial,
              "reserve_bytes": reserve, "growth_budget_bytes": budget,
              "profile_note": "Routine test debug is disabled; dev/profiling profiles retain symbols."}
    if initial < reserve + budget:
        print(json.dumps({**report, "status": "blocked"}), file=sys.stderr)
        print("Not enough free space for the test build budget plus reserve. Review the retention dry run; no artifacts were deleted.", file=sys.stderr)
        return 2
    if args.check_only:
        print(json.dumps({**report, "status": "ready"}, indent=2))
        return 0
    evidence = root / "artifacts" / "test-builds" / f"{time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())}-{os.getpid()}.json"
    evidence.parent.mkdir(parents=True, exist_ok=True)
    child = subprocess.Popen(command, cwd=root, start_new_session=os.name == "posix",
                             creationflags=subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0)
    minimum = initial
    reason = None
    try:
        while child.poll() is None:
            available = shutil.disk_usage(existing).free
            minimum = min(minimum, available)
            reason = disk_stop_reason(initial, available, reserve=reserve, budget=budget)
            if reason:
                stop_child(child)
                break
            time.sleep(2)
    except BaseException:
        stop_child(child)
        raise
    finally:
        report.update({"minimum_free_bytes": minimum, "exit_code": child.poll(), "stop_reason": reason})
        evidence.write_text(json.dumps(report, indent=2) + "\n")
    if reason:
        print(f"Stopped this test process group: {reason}. Evidence: {evidence}", file=sys.stderr)
        return 2
    return child.returncode if child.returncode >= 0 else 128 - child.returncode


if __name__ == "__main__":
    sys.exit(main())
