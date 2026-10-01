# Agent sessions

Open New Session, choose a project and checkout, then choose Terminal, Codex, Claude, or Pi. Agent conversations appear with terminal sessions in Sessions. They use each provider's native protocol and need no installed hooks.

Native sessions stream assistant text, reasoning and tool activity. The composer submits prompts through Bootty's command mailbox, supports queued followups, and preserves an unsent draft. Permission and question requests receive explicit replies. History shows saved conversations; resume restores the provider session. Account controls use provider APIs or the provider's interactive authentication terminal.

## Commands

```sh
bootty command agents.codex.start /path/to/project
bootty command agents.native.list
bootty agents.codex.prompt "Inspect the failing test" --target "SESSION_ID@GENERATION"
bootty agents.codex.interrupt --target "SESSION_ID@GENERATION"
bootty agents.codex.history --target "SESSION_ID@GENERATION"
bootty agents.codex.resume --target "SESSION_ID@GENERATION"
```

Replace `codex` with `claude` or `pi` as needed. `agents.native.list` returns records with `id`, `generation`, `binding_id`, title, configuration, and a normalized snapshot. Use the returned session target and generation for commands; a backend pane target cannot address a native conversation. Provider thread IDs are not Bootty targets.

The session registry persists before process startup. A failed start remains visible with its error. Restart restores stopped sessions and their bounded recent history, without launching processes automatically. Protocol input, queued work, requests, and output are bounded. Unsupported provider operations report their limit instead of inventing success.

Native sessions currently run in local Spaces. Remote terminals remain available through each mux backend. Browser panels use Wry and hide while GPUI dialogs or sheets overlap their native surface.

## Computer use

Open the computer setup, enable Computer use, and grant Screen Recording and Accessibility. Native commands then expose desktop snapshots and input through the same invocation path. Agents cannot enable access or request permission. Secure text input pauses computer operations. Screenshots are private PNG files; metadata and accessibility text are bounded.

## Orchestration

Coordination attaches existing agent sessions to durable runs. Create a run and tasks, attach workers, and dispatch a task. The provider accepting a prompt means the task is running. A completion or failure report must identify the worker target and dispatch attempt. A restart marks unfinished dispatches interrupted; retry is explicit. Messages have their own delivery results.

## Existing integrations

Existing custom Lua and Luau files remain preserved and unsupported. Previously configured terminal adapters can still publish pane-scoped events, but native sessions do not need them. Remove old adapters through their existing settings when they are no longer needed.
