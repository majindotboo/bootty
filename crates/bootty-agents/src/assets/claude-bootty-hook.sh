#!/bin/sh
set -eu

pane=${TMUX_PANE:-${BOOTTY_PANE:-}}
bootty --json command agents.claude.ingest --stdin "$pane" "${BOOTTY_AGENT_LAUNCH_CONTEXT:-}" >/dev/null 2>&1 || :
printf '{}\n'
