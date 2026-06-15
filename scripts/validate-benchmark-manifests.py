#!/usr/bin/env python3
"""Validate checked-in benchmark launcher and profile manifests."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
LAUNCHER = ROOT / "benchmarks" / "launcher-matrix.json"
PROFILES = ROOT / "benchmarks" / "profiles.json"
REQUIRED_PLATFORMS = {"linux_wayland", "linux_x11", "macos", "windows"}
REQUIRED_PROFILES = {
    "default",
    "normalized",
    "native_fast",
    "low_latency",
    "feature_heavy",
    "compatibility",
    "native_terminfo",
}
PLACEHOLDER_RE = re.compile(r"\{([a-zA-Z0-9_]+)\}")


def load_json(path: Path) -> Any:
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)


def fail(message: str) -> None:
    raise SystemExit(f"benchmark manifest validation failed: {message}")


def flatten_argv(value: Any) -> list[str]:
    if not isinstance(value, list) or not value:
        fail(f"argv must be a non-empty list: {value!r}")
    for part in value:
        if not isinstance(part, str) or not part:
            fail(f"argv entries must be non-empty strings: {value!r}")
    return value


def validate_launcher(data: dict[str, Any]) -> None:
    if data.get("schema_version") != 1:
        fail("launcher schema_version must be 1")
    placeholders = data.get("placeholders")
    if not isinstance(placeholders, dict) or not placeholders:
        fail("launcher placeholders must be a non-empty object")
    known_placeholders = set(placeholders)

    run_policy = data.get("run_policy")
    if not isinstance(run_policy, dict):
        fail("launcher run_policy is required")
    if run_policy.get("warmup_runs", 0) < 1 or run_policy.get("measured_runs", 0) < 2:
        fail("run_policy must declare warmup and measured runs")
    for field in ("terminal_version", "config_hash", "profile", "term", "power_mode"):
        if field not in run_policy.get("required_metadata", []):
            fail(f"run_policy.required_metadata missing {field}")

    terminals = data.get("terminals")
    if not isinstance(terminals, dict) or not terminals:
        fail("launcher terminals must be a non-empty object")
    covered_platforms: set[str] = set()
    for name, terminal in terminals.items():
        if not isinstance(terminal, dict):
            fail(f"terminal {name} must be an object")
        if not terminal.get("tags"):
            fail(f"terminal {name} must declare tags")
        flatten_argv(terminal.get("version_argv"))
        platforms = terminal.get("platforms")
        if not isinstance(platforms, dict) or not platforms:
            fail(f"terminal {name} must declare platforms")
        covered_platforms.update(platforms)
        for platform_name, platform_spec in platforms.items():
            if not isinstance(platform_spec, dict):
                fail(f"terminal {name}/{platform_name} must be an object")
            argv = flatten_argv(platform_spec.get("argv"))
            for part in argv:
                for placeholder in PLACEHOLDER_RE.findall(part):
                    if placeholder not in known_placeholders:
                        fail(f"terminal {name}/{platform_name} uses unknown placeholder {placeholder}")
    missing = REQUIRED_PLATFORMS - covered_platforms
    if missing:
        fail(f"launcher matrix does not cover platforms: {sorted(missing)}")


def validate_profiles(data: dict[str, Any]) -> None:
    if data.get("schema_version") != 1:
        fail("profiles schema_version must be 1")
    shared = data.get("shared_normalization")
    if not isinstance(shared, dict):
        fail("profiles shared_normalization is required")
    for field in ("font_family", "font_size_pt", "grids", "scrollback_lines", "env"):
        if field not in shared:
            fail(f"shared_normalization missing {field}")
    sampling = data.get("sampling")
    if not isinstance(sampling, dict):
        fail("profiles sampling is required")
    if sampling.get("warmup_runs", 0) < 1 or sampling.get("measured_runs", 0) < 2:
        fail("sampling must declare warmup and measured runs")

    profiles = data.get("profiles")
    if not isinstance(profiles, dict):
        fail("profiles object is required")
    missing = REQUIRED_PROFILES - set(profiles)
    if missing:
        fail(f"profiles missing required entries: {sorted(missing)}")
    labels: set[str] = set()
    for name, profile in profiles.items():
        if not isinstance(profile, dict):
            fail(f"profile {name} must be an object")
        for field in ("label", "purpose", "normalization", "term_policy", "config_policy"):
            if not profile.get(field):
                fail(f"profile {name} missing {field}")
        label = profile["label"]
        if label in labels:
            fail(f"duplicate profile label {label}")
        labels.add(label)


def main() -> int:
    launcher = load_json(LAUNCHER)
    profiles = load_json(PROFILES)
    validate_launcher(launcher)
    validate_profiles(profiles)
    print(
        "benchmark manifests valid: "
        f"{len(launcher['terminals'])} terminals, {len(profiles['profiles'])} profiles"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
