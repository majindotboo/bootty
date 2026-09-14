#!/usr/bin/env python3
"""Measure real owner commands through destination terminal paint in a running BoottyDev."""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import re
import statistics
import subprocess
import time


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)]


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def is_switch(command):
    return (len(command) == 3 and command[0] == "command"
            and command[1] in ("select_tab", "select_session", "select_space")
            and isinstance(command[2], str) and command[2].isdigit() and int(command[2]) > 0)


def summary(rows):
    result = {}
    for case in sorted({row["case"] for row in rows}):
        case_rows = [row for row in rows if row["case"] == case]
        samples = [row for row in case_rows if not row["warmup"]]
        failures = sum(row["status"] != "passed" for row in case_rows)
        passed = [row for row in samples if row["status"] == "passed"]
        metrics = {}
        for metric in ("command_ms", "command_to_paint_ms", "owner_to_paint_ms"):
            values = [row[metric] for row in passed]
            if values:
                metrics[metric] = {
                    "p50": percentile(values, .50), "p95": percentile(values, .95),
                    "p99": percentile(values, .99), "max": max(values),
                    "mean": statistics.mean(values), "stdev": statistics.pstdev(values),
                }
        result[case] = {"samples": len(samples), "failures": failures,
                        "status": "passed" if samples and not failures else "invalidated",
                        "metrics": metrics}
    return result


class Runner:
    def __init__(self, args):
        self.args = args
        self.env = dict(os.environ, BOOTTY_DEVELOPMENT_NAMESPACE=args.namespace)
        self.doctor = self.command(["doctor"])
        if self.doctor["instance"]["instance_id"] != args.namespace:
            raise RuntimeError("The running owner is not the requested development identity")
        self.pid = self.doctor["instance"]["pid"]
        self.trace = args.trace.open()
        self.current_target = None

    def command(self, command):
        result = subprocess.run([str(self.args.binary), "--json", *command],
                                env=self.env, capture_output=True, text=True,
                                timeout=self.args.timeout, check=True)
        response = json.loads(result.stdout)
        if "error" in response:
            raise RuntimeError(response["error"])
        outcome = response.get("result", response)
        if isinstance(outcome, dict) and outcome.get("status") not in (None, "success"):
            raise RuntimeError(outcome)
        return outcome

    def switch(self, destination):
        self.trace.seek(0, 2)
        started = time.time_ns()
        monotonic = time.monotonic_ns()
        self.command(destination["command"])
        command_ms = (time.monotonic_ns() - monotonic) / 1e6
        deadline = time.monotonic() + self.args.timeout
        owner_started = None
        while time.monotonic() < deadline:
            position = self.trace.tell()
            line = self.trace.readline()
            if not line.endswith("\n"):
                self.trace.seek(position)
                time.sleep(.002)
                continue
            event = json.loads(line)
            if (event.get("event") == "command_received" and event.get("pid") == self.pid
                    and event.get("command") == destination["command"][1]
                    and event["unix_ns"] >= started and owner_started is None):
                owner_started = event["unix_ns"]
            if (event.get("event") == "terminal_painted"
                    and event.get("target") == destination["target"]
                    and event.get("pid") == self.pid
                    and event.get("focused") and event.get("has_text")
                    and owner_started is not None
                    and event["unix_ns"] >= owner_started
                    and event["unix_ns"] >= started):
                # Reject wall-clock adjustments; the trace and controller use the same host clock.
                wall_elapsed = time.time_ns() - started
                mono_elapsed = time.monotonic_ns() - monotonic
                if abs(wall_elapsed - mono_elapsed) > 5_000_000:
                    raise RuntimeError("Host clock changed during sample")
                self.current_target = destination["target"]
                return {"command_ms": command_ms,
                        "command_to_paint_ms": (event["unix_ns"] - started) / 1e6,
                        "owner_to_paint_ms": (event["unix_ns"] - owner_started) / 1e6,
                        "paint": event}
        raise TimeoutError(f"Destination never painted with input focus: {destination['target']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--namespace", required=True)
    parser.add_argument("--trace", type=Path, required=True)
    parser.add_argument("--cases", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=10)
    parser.add_argument("--seed", type=int, default=7)
    args = parser.parse_args()
    if not re.fullmatch(r"bootty-dev-[0-9a-f]{16}", args.namespace):
        parser.error("An explicit isolated development namespace is required")
    if args.samples < 1 or args.warmups < 0 or args.timeout <= 0:
        parser.error("samples and timeout must be positive; warmups must be nonnegative")
    recipe = json.loads(args.cases.read_text())
    if not recipe["cases"] or len({case["name"] for case in recipe["cases"]}) != len(recipe["cases"]):
        parser.error("Provide at least one case, with unique case names")
    for case in recipe["cases"]:
        if len(case["destinations"]) != 2:
            parser.error("Each case needs two distinct destinations")
        if case["destinations"][0]["target"] == case["destinations"][1]["target"]:
            parser.error("Identical destinations would measure a no-op")
        for destination in case["destinations"]:
            command = destination["command"]
            if not is_switch(command):
                parser.error("Destinations must use select_tab/session/space with a positive index")
        if not all(is_switch(command) for command in case.get("setup", [])):
            parser.error("Setup may only change tab/session/Space selection")
    if not all(is_switch(command) for command in recipe.get("restore", [])):
        parser.error("Restore may only change tab/session/Space selection")
    args.output.mkdir(parents=True, exist_ok=False)
    runner = Runner(args)
    metadata = {"schema_version": 1, "platform": platform.platform(),
                "binary": str(args.binary.resolve()), "binary_sha256": digest(args.binary),
                "runner_sha256": digest(Path(__file__)),
                "load_average": os.getloadavg() if hasattr(os, "getloadavg") else None,
                "namespace": args.namespace, "doctor": runner.doctor, "recipe": recipe,
                "samples": args.samples, "warmups": args.warmups, "seed": args.seed,
                "boundary": "CLI launch and owner invocation to focused destination CPU paint; excludes display scanout",
                "outliers": "none removed", "settle_seconds": .1}
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    rows = []
    randomizer = random.Random(args.seed)
    try:
        with (args.output / "samples.jsonl").open("w") as output:
            for case in recipe["cases"]:
                for command in case.get("setup", []):
                    runner.command(command)
                runner.current_target = None
                for iteration in range(args.warmups + args.samples):
                    directions = [0, 1]
                    randomizer.shuffle(directions)
                    for index in directions:
                        destination = case["destinations"][index]
                        row = {"case": case["name"], "iteration": iteration,
                               "warmup": iteration < args.warmups, "direction": index,
                               "target": destination["target"]}
                        try:
                            # Establish the opposite destination; never time a no-op.
                            source = case["destinations"][1 - index]
                            if runner.current_target is None:
                                runner.command(destination["command"])
                                time.sleep(.1)
                            if runner.current_target != source["target"]:
                                runner.switch(source)
                            time.sleep(.1)
                            row.update(runner.switch(destination), status="passed")
                        except (RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
                            row.update(status="invalidated", error=str(error))
                        rows.append(row)
                        output.write(json.dumps(row) + "\n")
                        output.flush()
                        print(f"{case['name']} {iteration}:{index} {row['status']} "
                              f"{row.get('command_to_paint_ms', '')}", flush=True)
                        if row["status"] != "passed":
                            raise RuntimeError(row["error"])
    finally:
        result = summary(rows)
        (args.output / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
        runner.trace.close()
        for command in recipe.get("restore", []):
            runner.command(command)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
