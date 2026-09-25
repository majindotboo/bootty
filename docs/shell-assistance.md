# Shell assistance

Shells own editing. The command API exposes prompt leases and host-scoped history;
there is no separate shell editor panel.

## Commands

All callers use the same command mailbox:

- `shell.prompt`: read prompt revision, eligibility, shell, history path, cwd
  and recent metadata for the target native terminal.
- `shell.history QUERY [MODE]`: read and rank that terminal's host history.
- `shell.apply REVISION TEXT SUBMIT`: atomically validate and hand off a draft;
  `SUBMIT` is `true` or `false`. Text is limited to 16 KiB; control characters other than LF and tab are
  rejected. LF is preserved in bracketed paste.
- `history.search SPEC [MODE]`: host-scoped read on any binding. JSON fields are
  `shell`, `path`, `query`, `cwd`, and `recent` (normally an empty array).

The optional launch hooks preserve rc files and existing DEBUG traps. They emit
OSC 133 lifecycle plus bounded Bootty prompt/command metadata. Replayed output
never publishes these reports as live shell activity.

## Semantic history search

History search defaults to `local`: the existing offline subsequence matching and
directory/frequency/recency ranking. Pass `semantic` explicitly to rank existing
commands by meaning using TypeSafe. The desktop process must have
`TYPESAFE_API_KEY` in its environment when it starts; setting the key only in a
terminal pane or CLI client does not configure the desktop owner.

```sh
bootty command shell.history "find what is listening on a port" semantic
bootty command history.search '{"shell":"bash","path":"~/.bash_history","query":"show disk usage for each directory","cwd":"/work","recent":[]}' semantic
```

The first form requires a native terminal with shell hooks. The second works on
any binding, including native, rmux, tmux, and Herdr, locally or remotely. It reads
the selected host's history without assuming ownership of its prompt.

Semantic mode retrieves candidates without lexical filtering, deduplicates and
orders them using the existing history reader, then sends at most 50 commands
and 16 KiB of command text plus the query to TypeSafe. Metadata such as cwd and
timestamps stays local; paths or secrets inside command text are still sent.
For remote bindings, the daemon only reads history. The desktop makes the API
request and keeps the key out of the daemon protocol.

Each returned entry retains its original command and metadata, with an added
`relevance` probability between 0 and 1. Results are sorted by relevance; ties
keep the local ordering. No commands are generated or executed. A top-ranked
result can still be irrelevant: callers should inspect the scores rather than
treating rank 1 as a match. This experiment has no automatic acceptance cutoff.
The result also reports `model`, token `usage`, and `truncated` when candidates
were omitted. Empty candidate sets make no API request and return null model
and usage. `shell.history` nests these fields under `history` as before.

The model is pinned to `jev-1.13.0`. One request uses the remaining command
deadline, capped at ten seconds, with no retries or redirects. Missing credentials,
service errors, or malformed answers fail explicitly; callers can retry in
`local` mode. The candidate bound can miss older or infrequent commands; expand
retrieval only after measuring candidate recall on representative history.

Run the live comparison using synthetic commands only:

```sh
cargo run -p bootty-host --example semantic_history
```

It reports local and semantic top-1 accuracy, candidate presence, per-query
latency, token usage, and a query with no matching command. It does not read or
upload real shell history. Normal tests use an injected transport and make no
TypeSafe requests.
