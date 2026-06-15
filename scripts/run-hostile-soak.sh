#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/run-hostile-soak.sh [output-dir]

Runs opt-in hostile-input soak measurements outside routine validation. This is
intended for crash/hang/recovery evidence, not default CI. It writes command
logs and a small JSONL summary.

Environment:
  BOOTTY_HOSTILE_SOAK_SECONDS=10       timeout per measured command
  BOOTTY_HOSTILE_SOAK_SAMPLE_SIZE=10   Criterion sample size
USAGE
}

if [[ ${1:-} == "-h" || ${1:-} == "--help" ]]; then
  usage
  exit 0
fi

output_dir=${1:-artifacts/hostile-soak/$(date -u +%Y%m%dT%H%M%SZ)}
mkdir -p "$output_dir"
summary=$output_dir/summary.jsonl
: >"$summary"

timeout_seconds=${BOOTTY_HOSTILE_SOAK_SECONDS:-10}
sample_size=${BOOTTY_HOSTILE_SOAK_SAMPLE_SIZE:-10}

json_escape() {
  local value=${1-}
  value=${value//\\/\\\\}
  value=${value//"/\\"}
  value=${value//$'\n'/\\n}
  value=${value//$'\r'/\\r}
  printf '%s' "$value"
}

run_case() {
  local name=$1
  shift
  local log=$output_dir/$name.log
  local start end status detail exit_code
  start=$(date +%s)
  set +e
  if command -v timeout >/dev/null 2>&1; then
    timeout "$timeout_seconds" "$@" >"$log" 2>&1
  else
    "$@" >"$log" 2>&1
  fi
  exit_code=$?
  set -e
  end=$(date +%s)
  if [[ $exit_code -eq 0 ]]; then
    status=pass
    detail=ok
  elif [[ $exit_code -eq 124 ]]; then
    status=timeout
    detail="timed out after ${timeout_seconds}s"
  else
    status=fail
    detail=$(tail -n 1 "$log" 2>/dev/null || printf 'command failed')
  fi
  printf '{"schema_version":1,"event":"hostile_soak","name":"%s","status":"%s","detail":"%s","duration_s":%s,"exit_code":%s,"log":"%s"}\n' \
    "$(json_escape "$name")" \
    "$status" \
    "$(json_escape "$detail")" \
    "$((end - start))" \
    "$exit_code" \
    "$(json_escape "$log")" >>"$summary"
}

run_case hostile_mixed_soak_256_rounds \
  cargo bench -p bootty-app --bench hostile_input hostile_mixed_soak_256_rounds -- \
  --sample-size "$sample_size" --measurement-time 0.2 --warm-up-time 0.1

run_case hostile_extended_recovery_ladder \
  cargo bench -p bootty-app --bench hostile_input hostile_extended_recovery_ladder -- \
  --sample-size "$sample_size" --measurement-time 0.2 --warm-up-time 0.1

run_case hostile_long_line_16mb_write \
  cargo bench -p bootty-app --bench hostile_input hostile_long_line_16mb_write -- \
  --sample-size "$sample_size" --measurement-time 0.2 --warm-up-time 0.1

printf 'Wrote hostile soak evidence: %s\n' "$output_dir"
