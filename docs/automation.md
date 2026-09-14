# Automation

All actions use the command catalog. `bootty commands` lists it; `bootty describe
NAME` describes arguments and targets. A target captures a resource generation,
so replacing a pane cannot redirect a pending action to another process.

## Wait for a condition

```sh
bootty wait agents.pi.state --topic agents.pi.event --pointer /status --equals idle
```

Use the topic returned by `system.describe` for the installed provider. `--equals`
accepts JSON (numbers, booleans, arrays or objects); other input is a string.
`--pointer` selects a value using JSON Pointer. Arguments for the snapshot command
follow `--`. `--target` accepts a target object as JSON; otherwise the current
resource is captured once, before subscribing.

Wait accepts only read-only snapshot commands. It subscribes before reading,
checks the snapshot, then waits for events and reads again. Events are wakeups,
not proof that the condition holds. A queue gap creates a new subscription and
reconciles from a fresh snapshot. Completions from the snapshot command itself
are ignored, preventing a read/notification loop.

The default deadline is 60 seconds; `--timeout` changes it. Exit status is 0 for
a match, 10 for timeout and 130 for Ctrl+C. Output includes the matched or last
observed value. A stale target, lost owner or snapshot failure is an error.
Subscriptions are removed on normal completion, cancellation and error. Ctrl+C
is observed within one bounded request (at most five seconds).

Low-level clients can use `event.wait` with `subscription`, `cursor` and
`timeout_ms` (0–4000). It returns the same bounded event batch as polling, plus
`timed_out`. `bootty events wait ID --cursor N` exposes it directly. No event is
lost between checking the queue and arming the wait. Unsubscription wakes waiters.

Terminal silence is not process completion. A detached command task describes
completion of a catalog invocation; it is not a general process exit handle.

## Diagnose an owner

`bootty doctor` reports the exact selected instance, control protocol and limits,
configuration identity, live binding generations, backend capabilities, session
counts, reported agents and the last application error. Availability reflects
backend observations already held by the owner; it does not probe or launch a
remote session. An unavailable binding produces exit status 7. Without a running
owner the command fails unless `--start` is explicitly supplied.

`bootty command doctor` exposes the workspace portion through the shared command
catalog, including socket and integration callers. The CLI adds control endpoint
and protocol information around that same result.

## Run a host job

```sh
bootty run --cwd /path/on/the/selected/host -- cargo check
bootty run --detach --cwd /srv/project -- /bin/sh -c 'make && make test'
bootty jobs.list
bootty jobs.read JOB_ID 0 4000
bootty jobs.cancel JOB_ID
bootty jobs.forget JOB_ID
```

`run` starts a batch process on the selected binding's local, SSH or WSL host.
The program and arguments are passed literally; invoke a shell explicitly for
pipelines or shell expansion. The cwd is explicit and belongs to that host.
`--target` can capture a different binding as a JSON target. The Jobs Dock panel
provides launch, cancel, output inspection and forget through those same commands.

The CLI streams stdout and stderr separately and returns the observed process
exit code (or 128 plus a Unix signal). `--json` emits batches as newline-delimited
JSON, with base64 output bytes and an explicit nullable exit code/signal. A lost
transport never invents an exit code. Windows codes outside the CLI's 8-bit exit
range remain exact in JSON and map to CLI status 1.

`--timeout` is 1–86400 seconds, defaulting to an hour. Timeout returns status 124;
Ctrl+C returns 130. Process groups on Unix and Job Objects on Windows isolate
children before they can spawn descendants. Completion and cancellation clean
up that owned tree, including inherited output pipes. SSH/WSL use the versioned daemon protocol; losing the input channel cancels the remote job tree.

Jobs belong to the live Bootty window owner. Closing that window cancels them.
`--detach` detaches the CLI only. `--keep` retains a completed job for inspection;
otherwise the CLI forgets it after consuming its output. Up to eight jobs may run
and 64 records / 512 KiB of summary metadata may be retained per owner. Forget rejects a running job.

Each job retains at most 2 MiB of encoded output and 512 chunks. Reads return at
most 64 KiB and report a cursor gap explicitly; the streaming CLI fails on a gap
rather than presenting incomplete output as complete. The panel keeps bounded
UTF-8 previews of each stream; the command interface preserves original bytes.
`jobs.changed` supplies coalesced wakeups for snapshot waits, scoped to the live
registry generation. `jobs.read` can also wait up to four seconds for output or
an observed process exit without polling the UI thread.

File transfers share this job owner and expose typed progress through `transfers.start`;
see [Transfers and forwards](transfers.md).
