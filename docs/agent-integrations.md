# Agent integrations

Bootty launches Codex, Claude Code and Pi in real backend terminals. Their TUIs
own prompts, transcripts, approval dialogs and authentication. Bootty retains
bounded launch metadata and observes supported provider interfaces to show
activity beside sessions, including sessions that are not selected.

`bootty-agents` owns the provider registry, observation lifetimes and retained
metadata. The mux backend owns the terminal process, tabs and splits. The
palette, keybindings, CLI, socket and integrations use the same
`CommandInvocation` path and the same host-issued resource targets.

## Launch and control

For each provider, `agents.<provider>.start` opens a session and
`agents.<provider>.tab` opens a tab in an existing session. Provider names are
`codex`, `claude` and `pi`. Both commands accept optional `cwd`, `program` and
`argv` arguments. `argv` is a JSON array of literal arguments: quotes, spaces,
newlines and shell metacharacters are passed to the executable without shell
expansion. Executable and argument bounds are checked before backend creation.

`start` accepts an exact Binding target; `tab` accepts an exact Session target.
Native, rmux and tmux use the shared backend creation path. Herdr retains its
opaque attachment boundary and does not expose inner tab or pane creation.
Palette and keybinding launches select the returned terminal. CLI and socket
launches return its target without changing desktop selection.

`agents.<provider>.state`, `prompt`, `follow_up`, `steer`, `abort`, `interrupt`
and `stop` require the exact Terminal target issued by the host. The target
must still be live and registered for that provider. A stale generation is
rejected. Commands can address a terminal in another Space without selecting
that Space. Prompt delivery uses terminal paste and submit; it does not start a
second conversation process. Interrupt sends Escape to Claude or Ctrl-C to
Codex and Pi. Stop closes the backend pane and requires the normal destructive
command confirmation.

Provider options such as model, profile, thinking, sandbox and approval settings
remain literal native CLI options in `argv`. Retained metadata keeps only
reusable options. Prompts, credentials, arbitrary config overrides and transient
session selectors are excluded from that metadata.

## Observed activity

The public activity snapshot identifies its exact Terminal target and provider.
Its states are Starting, Idle, Working, Waiting, Finished, Stopped, Error and
Unavailable. A missing connection or unavailable native query is reported as
Unavailable; process presence alone does not prove that an agent is working or
waiting. Provider identities come from actual protocol responses or native
queries, never from a working directory, a recent file or terminal text.

### Codex

For a local native backend pane, Bootty owns a private Codex app-server and
starts the interactive TUI with Codex's supported `--remote` transport. A
transparent local relay forwards the native protocol and correlates actual
thread creation, resume and fork responses. Thread and turn lifecycle events
supply activity; a successful initialization handshake supplies Idle readiness
before a thread exists. Approval requests supply Waiting without answering or
changing the user's grants.

The private server has the same lifetime as the process-local pane's app owner.
It is not attached to a persistent rmux or tmux TUI: closing Bootty must not
terminate a surviving turn. Those backends launch the original Codex TUI
arguments and report observation Unavailable. Bootty does not start, stop or
reconfigure the user's shared Codex daemon to work around this limit.

The private relay currently requires Unix sockets. A platform without that
supported observation transport reports an explicit launch error for the
observed path rather than pretending a handshake succeeded.

### Claude Code

A fresh observed Claude terminal receives a native `--session-id` UUID. An
explicit resumed identity uses the provider's exact session ID. Bootty queries
`claude agents --json --all` and matches that complete identity. Native busy,
waiting and idle states drive activity; an observed working-to-idle transition
marks Finished. If the exact session is absent or the query fails, observation
is Unavailable. An ambiguous resume selector is rejected rather than assigned
to another conversation.

The query uses the installed executable and existing provider authentication.
Launching a TUI does not establish that the account is signed in.

### Pi

Bootty supplies a private extension only to the launched Pi process using its
native extension argument. No extension is installed globally. The supported
SDK provides the actual session ID and session file, agent lifecycle events,
and prompt start/end events. These drive Working, Waiting and completed,
failed or aborted outcomes. Nested prompts remain Waiting until the final
prompt ends.

The extension publishes bounded snapshots through a private local Unix socket.
Its identity token and files belong to that launch and are removed when the
observer retires. Unsupported platforms report the observation limitation
explicitly. Dropping an observer does not terminate a backend-owned Pi process.

## Accounts

`agents.<provider>.account.status` uses the installed provider's supported
account query. It returns the provider, an authenticated boolean when the
query establishes it, and a bounded explanation. It does not return tokens or
infer authentication from an available model. Pi requires a provider ID for its
native auth check and does not refresh credentials during a status read.

`account.login` and `account.logout` open the provider's own interactive terminal
flow in an exact binding. Codex uses its device login or logout command; Claude
uses its native auth commands; Pi opens its TUI with its own `/login` and
`/logout` menus.
Completing that terminal flow is the provider's responsibility. Bootty does not
claim authentication success merely because the terminal opened.

## Native conversations

New session opens a prompt-first native conversation. New native agent tab adds
one to the current saved session. Codex, Pi and Claude use their installed
providers' public protocols: Codex app-server, Pi RPC and Claude stream-json.
The selected account, project, task and conversation identity stay captured for
the lifetime of that conversation. Local and remote conversations use the same
captured provider protocol; the remote daemon owns the provider process transport.
A conversation's unsent text and attachment chips are saved with it shortly after
typing stops, and return when it reopens, including after a restart. The New Session
form's words and choices are saved beside the configuration, in
`new-session-draft.json`, a second after they stop changing, and return when the form
next opens on the same project; pasted files, which live in temporary files, don't.
A conversation that finishes a turn shows Finished in the sidebar until its pane is
seen; Mark unread in its row menu shows it again.

The Add menu is shared by creation drafts and existing conversations. It includes
files, local application windows, and the captured provider's enabled skills.
Codex also supplies installed callable apps and enabled plugins, with typed
mentions that remain intact when queued. `@` followed by words also offers other
conversations, by title or short name; the message keeps the name, and the provider
reads each mentioned conversation's latest prompts and replies, oldest first, up to
8 KiB each and four conversations a message. The transcript shows the message as
written. Plan mode and conversation goals appear
when advertised by that server. Claude always offers Plan mode, its own permission
mode; approving Claude's plan leaves it. Pi's Plan mode comes from Bootty's Pi
extension, so a Pi launched without Bootty's tools has none: while it is on, only
Pi's read, grep, find and ls tools run, and each prompt asks for a plan. Goals use Codex's thread goal API; they
are not added as prompt text. The composer drawer shows a conversation's goal with
its state and the tokens it spent against any budget, follows Codex's changes to it,
and pauses, resumes or removes it (`agents.native.goal-status`, `agents.native.goal`).
The plan drawer also shows Claude's to-do list, from TodoWrite or its task tools.
Compaction shows as a row for every provider: Claude's says how far it shrank the
context, which the meter then reads, and Pi's own compactions show as they run. The
context meter's menu compacts the conversation as `/compact` does. Commands Claude
runs itself, such as `/cost` or a `/compact` with nothing to compact, show Claude's
output as it laid it out. Codex's and
Claude's usage limit reports show in the meter's tooltip; a turn a limit stopped says
when it resets and can wait to send the turn again then. The sidebar's usage meter
uses those reports while they are fresh, and asks `codexbar` otherwise.
In Codex, `/review` runs its reviewer on the uncommitted changes, or with the words
after it as instructions, and shows as a review row; commands only Codex's terminal
app runs, such as `/init` or `/status`, are refused rather than sent to the model as
words. A ready plan, Codex's or one Claude asks to carry out, can instead go to a new
conversation in the same session with the same provider and settings; Claude's is
then declined here with a note saying so. A question's answer can carry files, by
drop or Attach files…, which go to a local provider as their paths. In the Changes
diff, and in a conversation's file changes, selected lines take a comment that goes
into the conversation's draft with the file, the lines and their text.
In a Git work tree, every turn snapshots the files before it starts and after it
ends, untracked files included, without touching the index. A reply's file-diff
button, or Review on the list of files a turn changed, opens that turn's changes
in the Changes panel (`agents.native.turn-diff`). Each file in that list shows
its diff in place when clicked. A prompt's rewind
button asks how far to go back to before it. Rewinding the conversation
(`agents.native.rewind PROMPT [files]`) makes the provider forget that message's
turn and every later one, drops them from the transcript, and brings the message
back to the composer. With `files`, or alone (`agents.native.rewind-files`), the
files go back too. Both are refused while a turn runs. Codex reverts its thread
before the turn (`thread/revert`), so a steering message goes with the turn it
joined. Claude rewinds in place and refuses when its conversation has messages
this one doesn't show. Pi moves back in its session tree through Bootty's Pi
extension, which needs Bootty's tools, and keeps the abandoned turns in its file.
Sketch and skill recording are not supported here yet.

Session naming also updates the exact newly created conversation's tab title.
Delayed names apply only while each captured title remains unchanged; sibling
conversations retain their own titles. Submitting a prompt preserves the title
supplied at creation or by a later rename.

Native Codex launches use a process-local `default_mode_request_user_input` feature
override so supported SDKs can ask questions in Default mode. This does not change
the provider account's configuration or permission policy.

The transcript, composer, approval requests and questions live in a selectable
backend pane beside terminals. Close stops that pane and provider while retaining
the saved conversation and history. Up in an empty composer brings back the
conversation's sent messages, newest first, and Down goes forward again. A sent
message's pencil brings it back to change and send again, with its attachments; its
browser notes return as notes, screenshots included, while they're still saved, and
otherwise their text returns with the message.
Images a tool returns show under its row, open or folded. Any image opens in a
viewer that fits it to the window; Ctrl or Command with the wheel, a pinch, `+`
and `-` zoom, dragging or the wheel pans, a double click zooms in or back to fit,
`0` fits, and the arrow keys step through the conversation's other images. A sent
file opens with a click, and its menu shows it in the file manager. A reply with a
markdown table offers Copy table, which copies every table as tab-separated cells.
A turn's last reply shows the tokens the turn used, and its price when Claude or Pi
reports one; the tooltip splits out cached input and reasoning.

A prompt sent from the composer shows at the end of the transcript, which follows
its reply.

The terminal's find and copy mode keys act on a focused conversation. Find
(Cmd+F, Ctrl+Shift+F elsewhere) highlights a query's matches across the shown
messages, ignoring the case of ASCII letters; Enter and Shift+Enter step through them
and Escape closes it. Copy mode (Cmd+Y, or the prefix and `[`) puts a cursor on the
latest message, or adopts a pointer selection: `h` `j` `k` `l`, `w` `b` `e`, `0` `^` `$`,
`gg` `G`, Ctrl+D and Ctrl+U move it, with counts and `f` `F` `t` `T` `;` `,`; `v`, `V`
and Ctrl+V select by character, line or column, and `o` swaps the ends. `y` or Enter
copies the selection, or the cursor's line, and leaves; `c` comments on a selection in
one reply and `r` reacts to it; Escape clears the selection, then leaves.
Choosing Agent in the split chooser creates its backend pane beside the captured
parent, in the requested direction. The parent task and window stay unchanged.
Interrupt cancels its current turn, as does Escape in the composer while a turn
runs and the composer has nothing else to dismiss, and Interrupt Agent in the
palette (`interrupt_agent`) for the conversation in the focused pane. Opening a stopped conversation automatically
reattaches its exact provider selector: a Codex thread, Pi session file or Claude
session UUID. Pi's launch extension checkpoints its public session header and
entries before a fresh identity is published, then Pi reattaches that same file.
This preserves side chats that have not received their first message. The private
checkpoint command is excluded from composer completion. A rejected
prompt retains the draft and does not change the saved title. Claude history is
bounded to the turns retained by Bootty until a public history interface exists.

Codex approvals show the command, working directory and reason. Allow once retains
no grant. Allow for this session uses Codex's session approval cache; Always allow
applies only the exact proposed command-prefix rule displayed in the request.
Provider-restricted decisions, altered rules and expired requests are rejected.
Deny and stop declines and ends the turn that asked: Codex's `cancel` decision,
or Claude's denial with `interrupt`. Deny with note tells the agent why: Claude
reads the note as the reason for the denial; Codex and Pi decline, and the note
steers the turn as the next message.
A new approval takes focus unless the composer holds a draft; Enter then takes its
primary choice and Escape denies it. Reusable scopes stay on their buttons.
A blocking question takes focus the same way: digits 1–9 pick an option on the
current question and move to the next, Enter moves on and sends from the last
question once each has an answer, and Escape cancels a Claude question.
Several questions show one at a time with their place ("Question 2 of 3"), and
Back and Next move between them without losing answers. A typed answer can span
lines with Shift+Enter; a secret answer stays on one masked line.
A Codex subagent's request appears in its parent conversation with the
subagent's name, and stays answerable after the parent's turn ends.
A file-change approval shows the diff it would apply: Codex's for the file-change
item it names, and Claude's Edit, MultiEdit and Write built from their old and new
text, numbered within the snippet since the request has no file positions.

An MCP server's request shows what it asks for. A tool approval offers Allow for
this session and Always allow only when the server names those scopes. A form
shows its fields, typed and checked against the server's schema before Submit
sends them. A sign-in page shows its address, and only http and https pages open,
from the button that names their host. Bootty declines anything else it can't
fill in.

Permission changes during a turn are saved for the next turn. The current prompt
and approval keep their captured policy. After the turn becomes idle, Bootty
reattaches the same provider conversation with the saved policy and a fresh tool
lease before accepting its next prompt.

Provider request waits pause while an owned question or approval is pending.
Answering or cancelling resumes the remaining transport budget. The provider's
actual reply still owns prompt acceptance; a visible dialog never invents turn success.
Pi extension commands can open a question before acknowledging their prompt.
Bootty returns the observed waiting state to the command caller immediately and
keeps an owned reply worker until answer, cancellation or shutdown. This prevents
the caller's deadline from expiring during human input; a later provider rejection
still appears as a conversation error.

Bootty tools attach privately at launch using the existing provider policy and
captured caller, Binding and terminal identity. Focus changes do not retarget
them. Native conversations also attach `list_agents`, `get_agent_status` and `list_models`
before provider initialization. These read the persisted conversation identity
and its exact provider account. Agent listing returns compact lifecycle metadata
only in the captured Space; status/model reads cannot choose another agent or account.
`list_providers` and `inspect_provider` expose only that captured provider's persisted
model, effort, fast mode and permission choices. They omit executable, account and
project paths, and do not authenticate or change policy. Legacy unspecified policy
remains unspecified; available choices use explicit modes supported by the provider.
Terminal agents keep their terminal-scoped catalog. Inherited child leases cannot
list siblings or advertise computer input/capture. Root resume renews the lease;
restored children keep their provider identity without renewing the former parent's grant.
Disabling the provider or revoking its authority
revokes the lease and child access. Private attachment arguments and credentials
are excluded from saved launch metadata. Computer capture and input each require
their existing grants.

`computer.capture` returns the exact application window as a validated PNG MCP
image with capture geometry. `computer.snapshot` retains its file export behavior.
Image responses have a separate bounded envelope; ordinary tool requests and
text responses retain their smaller limit.
Codex and Pi accept correlated image-tool results and bounded provider history.
Pi codemode image echoes must match the pixels from an observed capture in that
exact parent call. Only fingerprints are retained for the live turn; historical
image replies come from the correlated provider-owned history response and confer
no capture authority.
Claude tool-result echoes above 1 MiB remain unsupported until its public protocol
has a verified correlated format.

Native prompts accept host-admitted PNGs through the same conversation owner.
Captured image references remain in history; encoded pixels are transport-only.
Provider rejection retains the pending draft and its attachments.

Typed spawning tools create child work in the captured destination. Native parents
create native provider children in real mux tasks on their same local or remote
host. The provider executable, account, project, model and permissions come from
the accepted parent record; children start with fresh provider identities. Dependent work
starts only after its prerequisite's exact first turn succeeds. Failed or
interrupted turns retain their conversation for review. Recovery goes through
the shared invocation owner once. A stale destination requires explicit Restart.

## Retained sessions and limits

`agents.<provider>.history` reads bounded conversation metadata from the selected
provider account, scoped to the current project or all projects, 200 sessions a
page; `before`, in UTC epoch milliseconds, asks for the page of sessions last
updated before it. Stored titles take precedence; conversations without a title use
a short first-user-message preview: Codex's thread preview, or the bounded file
prefix for Claude and Pi. Discovery does not rewrite or launch a conversation.
`resume` and `fork` accept an explicit provider session selector and launch the
provider's own TUI with its supported native arguments. The provider owns the
conversation history and displays it in that TUI. The metadata list is not a
transcript renderer or a complete provider-wide history index.

Restored active records are marked Unavailable until a new supported observation
is established. Explicitly stopped records retain Stopped and their observed
provider identities. Bootty does not infer a surviving TUI's identity after restart.
Remote backend launches use the backend's literal argv path; native observation
remains Unavailable until a supported host bridge exists. No local query is
used to claim a remote terminal's activity or account state.

Settings does not offer bundled hook installation. Existing user provider
configuration and custom Lua or Luau files are preserved; custom scripts are
reported as unsupported and are not executed. Existing ingress wire values
remain accepted by the legacy library boundary, but primary terminal launch and
activity do not depend on that ingress.

## Dictation and voice

Both composers use the bundled Pi audio host for microphone capture and playback
on macOS, Linux and Windows. Dictation uses OpenAI Realtime transcription and
inserts the text at the cursor.

Speech runs on the Codex sign-in already on this machine: a Codex conversation's
own account, otherwise the selected Codex account. A ChatGPT sign-in sends its
token with its account id; Codex in API-key mode sends that key. Nothing else is
read. An expired token stops before opening the microphone. Credentials never
enter the GUI, command arguments, or error messages.

Without a current sign-in, dictation on macOS uses Apple's on-device speech
recognizer, so audio and text stay on the Mac. The first such dictation asks for
the Speech Recognition permission. Where neither is available, as on Windows and
Linux without Codex, the buttons don't show and the commands report why.

Voice mode is a Codex call on a Codex conversation's own thread. The audio host
makes a WebRTC offer, the conversation takes it to Codex with
`thread/realtime/start`, and Codex's answer connects the call. Codex then listens,
starts turns on the conversation for what it delegates, and speaks the replies; the
transcript shows only the delegated request. Ending voice mode stops the call.
Other conversations and drafts show no voice button.

Speech notifications and conversation errors offer **Copy details** for the original
diagnostic. A conversation that failed because its provider account needs signing
in also offers **Sign in**, which runs the same sign-in flow as Settings.

Claude loads the connectors of the Claude account it signs in with, the same ones
Claude Desktop has; Bootty passes only its own tool server beside them. A Claude
signed in with an Anthropic Console key has no account connectors, and its status
in Settings says so. **Sign in** runs `claude auth login`, which uses the Claude
subscription.
