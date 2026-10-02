#!/usr/bin/env python3
"""Plan retention of Cargo test generations; never delete or modify artifacts."""

import argparse
import contextlib
import hashlib
import json
import math
import re
import shutil
import subprocess
import time
from pathlib import Path

try:
    import fcntl
except ImportError:
    fcntl = None

IDENTITY_FIELDS = ("target", "profile", "features", "rustflags", "rustc", "compile_kind", "config", "path")
TEST_FINGERPRINT = re.compile(r"test-(?:integration-test|lib|bin|bench|example)-[\w-]+\.json")


def inventory(root, packages=None):
    """Only recognize self-contained test hash directories in Cargo's per-artifact layout."""
    records, unknown = [], []
    for package in sorted(root.iterdir()):
        if packages is not None and package.name not in packages:
            continue
        if not package.is_dir() or package.is_symlink():
            continue
        for generation in sorted(package.iterdir()):
            if not generation.is_dir():
                continue
            try:
                if generation.is_symlink() or not re.fullmatch(r"[0-9a-f]{16}", generation.name):
                    raise ValueError("unrecognized directory")
                fingerprints = list((generation / "fingerprint").glob("*.json"))
                if len(fingerprints) != 1 or not TEST_FINGERPRINT.fullmatch(fingerprints[0].name):
                    raise ValueError("not an isolated test/benchmark fingerprint")
                data = json.loads(fingerprints[0].read_text())
                identity = {key: data[key] for key in IDENTITY_FIELDS}
                if any(type(identity[key]) is not int for key in IDENTITY_FIELDS if key not in ("features", "rustflags")):
                    raise ValueError("unrecognized Cargo identity")
                if not isinstance(identity["features"], str) or not isinstance(identity["rustflags"], list):
                    raise ValueError("unrecognized feature/flag identity")
                paths = [generation, *generation.rglob("*")]
                if any(path.is_symlink() for path in paths):
                    raise ValueError("contains a symlink")
                if not (generation / "out").is_dir():
                    raise ValueError("missing output directory")
                name = TEST_FINGERPRINT.fullmatch(fingerprints[0].name).group(0)
                name = re.sub(r"^test-(?:integration-test|lib|bin|bench|example)-", "", name[:-5])
                stems = {f"{name}-{generation.name}", f"{name.replace('-', '_')}-{generation.name}"}
                expected = {stem + suffix for stem in stems
                            for suffix in ("", ".exe", ".pdb", ".d", ".dSYM")}
                expected.update("lib" + stem + ".rmeta" for stem in stems)
                if any(path.name not in expected for path in (generation / "out").iterdir()):
                    raise ValueError("unrecognized output file; preserve the whole generation")
                files = [path for path in paths if path.is_file()]
                if not files:
                    raise ValueError("empty generation")
                stats = [(path.relative_to(generation).as_posix(), path.stat()) for path in files]
                stamp = [(name, st.st_dev, st.st_ino, st.st_size, st.st_mtime_ns, st.st_mode) for name, st in stats]
                records.append({
                    "path": str(generation), "package": package.name,
                    "fingerprint": fingerprints[0].stem, "identity": identity,
                    "newest_mtime": max(st.st_mtime for _, st in stats),
                    "allocated_bytes": sum(st.st_blocks * 512 for _, st in stats),
                    "snapshot_sha256": hashlib.sha256(json.dumps(sorted(stamp)).encode()).hexdigest(),
                })
            except (ValueError, KeyError, TypeError, OSError) as error:
                unknown.append({"path": str(generation), "reason": str(error)})
    return records, unknown


def retention_plan(records, *, now, keep=2, idle_seconds=1800, open_paths=(), readers_verified=True):
    if keep < 2 or idle_seconds < 0:
        raise ValueError("retain at least two generations and use a nonnegative idle interval")
    groups = {}
    for record in records:
        key = json.dumps([record["package"], record["fingerprint"], record["identity"]], sort_keys=True)
        groups.setdefault(key, []).append(record)
    retained, candidates = [], []
    for group in groups.values():
        ordered = sorted(group, key=lambda row: (-row["newest_mtime"], row["path"]))
        for index, row in enumerate(ordered):
            reasons = []
            if index < keep:
                reasons.append("newest retained generation")
            if now - row["newest_mtime"] < idle_seconds:
                reasons.append("recent or future-dated output")
            if not readers_verified:
                reasons.append("live-reader check unavailable")
            path = Path(row["path"])
            if any(Path(open_path).is_relative_to(path) for open_path in open_paths):
                reasons.append("live reader")
            item = {**row, "protection_reasons": reasons,
                    "newer_preserved": [entry["path"] for entry in ordered[:keep]]}
            (retained if reasons else candidates).append(item)
    before = sum(row["allocated_bytes"] for row in records)
    reclaimable = sum(row["allocated_bytes"] for row in candidates)
    return {"identity_groups": len(groups), "retained": retained, "candidates": candidates,
            "known_bytes_before": before, "candidate_bytes": reclaimable,
            "known_bytes_after_proposed_retention": before - reclaimable}


@contextlib.contextmanager
def idle_locks(paths):
    if fcntl is None:
        raise RuntimeError("Retention needs POSIX build locks; all artifacts remain protected")
    with contextlib.ExitStack() as stack:
        for path in paths:
            lock = stack.enter_context(path.open("a"))
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield


def readers(target):
    executable = shutil.which("lsof")
    if not executable:
        return [], False
    result = subprocess.run([executable, "-n", "-F", "n", "+D", str(target)], capture_output=True, text=True)
    verified = result.returncode in (0, 1) and not result.stderr.strip()
    return [line[1:] for line in result.stdout.splitlines() if line.startswith("n/")], verified


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", required=True, type=Path)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--package", default="bootty-ui")
    parser.add_argument("--keep", default=2, type=int)
    parser.add_argument("--idle-minutes", default=30, type=float)
    parser.add_argument("--lock-file", default=Path("/tmp/bootty-cargo-build.lock"), type=Path)
    args = parser.parse_args()
    if args.keep < 2 or not math.isfinite(args.idle_minutes) or args.idle_minutes < 0:
        parser.error("retain at least two generations and use a finite nonnegative idle interval")
    if args.target_dir.is_symlink() or not args.target_dir.is_dir():
        parser.error("target-dir must be an existing directory, not a symlink")
    target = args.target_dir.resolve()
    if args.manifest.is_symlink() or args.manifest.resolve().is_relative_to(target):
        parser.error("manifest must be outside the Cargo target directory and must not be a symlink")
    if args.manifest.exists():
        try:
            previous = json.loads(args.manifest.read_text())
            if previous.get("schema_version") != 1 or previous.get("mode") != "dry-run-only":
                raise ValueError("not a retention manifest")
        except (OSError, ValueError, AttributeError):
            parser.error("refuse to overwrite a file that is not a retention manifest")
    root = target / "debug" / "build"
    if not root.is_dir():
        parser.error("unrecognized Cargo output layout; artifacts remain protected")
    locks = [args.lock_file]
    cargo_lock = target / "debug" / ".cargo-lock"
    if cargo_lock.exists():
        locks.append(cargo_lock)
    try:
        with idle_locks(locks):
            records, unknown = inventory(root, packages={args.package})
            open_paths, verified = readers(target)
            report = retention_plan(records, now=time.time(), keep=args.keep,
                                    idle_seconds=args.idle_minutes * 60,
                                    open_paths=open_paths, readers_verified=verified)
            report.update({"schema_version": 1, "mode": "dry-run-only", "target_dir": str(target),
                           "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                           "free_bytes": shutil.disk_usage(target).free, "keep": args.keep,
                           "readers_verified": verified, "unknown_protected": unknown})
    except BlockingIOError:
        parser.exit(2, "Build lock is busy; wait for an idle boundary. No artifacts inspected or changed.\n")
    except RuntimeError as error:
        parser.exit(2, str(error) + "\n")
    args.manifest.parent.mkdir(parents=True, exist_ok=True)
    args.manifest.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Dry run: {len(report['candidates'])} candidates, {report['candidate_bytes']} allocated bytes; {args.manifest}")


if __name__ == "__main__":
    main()
