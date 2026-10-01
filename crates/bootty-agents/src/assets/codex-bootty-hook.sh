#!/bin/sh
set -eu

pane=${TMUX_PANE:-${BOOTTY_PANE:-}}
# Pane ids repeat across servers. tmux names its server's socket in $TMUX; Bootty's rmux panes
# leave $TMUX empty and name theirs in $RMUX.
server=${TMUX:-${RMUX:-}}
bootty --json command agents.codex.ingest --stdin --detach "$pane" "${BOOTTY_AGENT_LAUNCH_CONTEXT:-}" "$server" >/dev/null 2>&1 || :
printf '{}\n'
