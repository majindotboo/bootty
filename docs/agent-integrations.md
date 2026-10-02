# Agent terminals

Open New Session, choose a project and checkout, then choose Terminal, Codex,
Claude, or Pi. An agent opens its own interactive terminal interface in a real
backend session. Tabs, splits, focus, input, and process lifetime belong to the
selected mux backend. Bootty does not start a second conversation process or
install hooks for this flow.

Bootty retains bounded launch metadata associated with the exact terminal target.
Provider icons can use that identity even when a foreground executable changes.
The provider owns its conversation files, authentication, approval prompts, and
interactive history picker. Launch metadata does not imply that the provider is
ready, working, or authenticated.

## Commands

```sh
bootty command agents.codex.start /path/to/project
bootty command agents.codex.tab /path/to/project
bootty command agents.list
bootty agents.codex.prompt "Inspect the failing test" --target "TERMINAL_HANDLE@GENERATION"
bootty agents.codex.interrupt --target "TERMINAL_HANDLE@GENERATION"
bootty command agents.codex.sessions /path/to/project
bootty command agents.codex.history /path/to/project
bootty command agents.codex.resume PROVIDER_SESSION_ID /path/to/project
bootty command agents.codex.fork PROVIDER_SESSION_ID /path/to/project
bootty command agents.codex.account.status
bootty command agents.codex.account.login
```

The palette also offers Open Codex tab, Open Claude tab, and Open Pi tab for the
current session. `tab` creates a backend window, then launches the provider only
in that new terminal. It never writes a launch command into an existing pane.
An explicit session target can address another session without changing its
existing terminals; opaque backends report their creation limit.

Replace `codex` with `claude` or `pi`. `start` and `tab` accept optional working directory,
executable, and JSON argv. Arguments remain literal. The GUI focuses the created
terminal; callers receive its issued target.
Session names combine the provider and project, using the backend's existing
uniqueness rules when that name is already in use.
`prompt` pastes text and submits it through the ordinary terminal command path.
`interrupt` and `abort` send the provider's terminal interrupt key. `stop` closes
that backend pane and requires exact destructive confirmation.

Use targets returned by `agents.list`, session creation, or `spaces.list`.
A target includes its kind, opaque handle, and generation. Provider session IDs
are selectors for `resume` and `fork`, never substitutes for Bootty targets.
Launch, history, and account commands accept Binding targets; terminal input
commands accept Terminal targets. Stale generations and provider mismatches are
rejected.

`spaces.list` includes each session's terminal and pane targets, working directory,
and windows with issued window and pane targets. Herdr retains its opaque
attachment boundary and reports inner topology unsupported.

## History and accounts

`sessions` reads bounded metadata from provider-owned local conversation files.
Each record includes provider session ID, title, working directory, file path,
modification time, and optional latest observed token counts. Discovery skips
malformed records, unsupported versions, and symlinks; it reads at most 128 files
and 16 MiB per request. It never reads credential stores. Token counts describe
an observed response, not account quota or billing.

`history` opens the provider's native resume picker in a new terminal. `resume`
opens a selected conversation; `fork` starts a new conversation using the
provider's native fork command. Codex and Claude also support their picker when
no session ID is supplied. Pi requires a selector for `fork`.

`account.status` uses a bounded provider CLI readiness query. Pi requires its
provider ID argument, such as `openai-codex`; availability alone is not reported
as authentication. Login and logout open the provider's interactive authentication
terminal. Pi uses `/login` or `/logout` there. Logout requires destructive
confirmation. Starting the terminal does not claim that authentication completed.

Local native, rmux, and tmux backends launch supported agent terminals. Remote
launches use the backend's existing remote creation capability and require the
provider executable on that host. Remote history discovery and account status
queries report unsupported; native provider pickers and authentication terminals
remain available remotely. Herdr does not expose inner session creation.

On POSIX rmux and tmux launches, the new agent process clears inherited color
suppression with the platform environment helper. An explicit `session.env`
`NO_COLOR` value takes precedence. Existing server environments and user
processes are unchanged. Native PTYs apply the same policy directly.

## Computer use

Open Connections, enable Computer use, and grant the platform's required access.
Computer commands expose snapshots and input through the same invocation path.
Agents cannot enable access or request permission. Secure text input pauses
computer operations. Screenshot files are private, and metadata and accessibility
text are bounded. Platforms without a supported capture or input API report the
specific unsupported operation.

## Coordination

Coordination attaches registered agent terminals to durable runs. Create a run
and tasks, attach workers, and dispatch a task. Successful terminal input means
the prompt was delivered; completion requires an explicit report from the worker.
Reports identify the exact worker target and dispatch attempt. A restart marks
unfinished dispatches interrupted; retry is explicit. Messages retain their own
delivery results.

Existing custom Lua and Luau files remain preserved and unsupported. Previously
configured terminal adapters can still publish pane-scoped events. The primary
terminal launch, history, and account paths do not need them. Settings does not
install or remove legacy adapters; stale adapter requests report unsupported
without changing files or configuration.
