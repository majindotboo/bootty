use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use bootty_agents::{AgentKind, TerminalHistoryQuery, terminal_provider_history};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::{Value, json};

type FixtureResult<T> = Result<T, Box<dyn std::error::Error>>;

fn project(root: &Path, name: &str) -> FixtureResult<PathBuf> {
    let path = root.join(name);
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn saved(
    root: &Path,
    provider: AgentKind,
    name: &str,
    records: &[Value],
    modified: u64,
) -> FixtureResult<PathBuf> {
    let store = if provider == AgentKind::Claude {
        "projects"
    } else {
        "sessions"
    };
    let project = project(&root.join(store), "provider-encoded-project")?;
    let path = project.join(format!("{name}.jsonl"));
    let bytes = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&path, bytes)?;
    let file = fs::File::options().write(true).open(&path)?;
    let modified = UNIX_EPOCH
        .checked_add(Duration::from_secs(modified))
        .ok_or("Fixture timestamp is out of range")?;
    file.set_times(fs::FileTimes::new().set_modified(modified))?;
    Ok(path)
}

const fn query<'a>(
    provider: AgentKind,
    account: &'a Path,
    cwd: Option<&'a Path>,
) -> TerminalHistoryQuery<'a> {
    TerminalHistoryQuery {
        provider,
        program: "unused-file-provider",
        account_directory: account,
        cwd,
        limit: 50,
    }
}

#[rstest]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
fn history_without_a_stored_title_uses_the_first_user_prompt(
    #[case] provider: AgentKind,
    #[values(false, true)] text_blocks: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let cwd = project(directory.path(), "workspace").unwrap();
    let content = if text_blocks {
        json!([{"type":"image","data":"ignored"},{"type":"text","text":"Fix the\n history picker"}])
    } else {
        json!("Fix the\n history picker")
    };
    let mut records = vec![];
    if provider == AgentKind::Pi {
        records.push(json!({"type":"session","version":3,"id":"saved-id","cwd":cwd}));
    }
    records.push(json!({"type":"assistant","sessionId":"saved-id","cwd":cwd,"message":{"role":"assistant","content":"Not the conversation title"}}));
    records.push(json!({"type":if provider == AgentKind::Claude { "user" } else { "unknown" },"sessionId":"other-id","cwd":cwd,"message":{"role":"user","content":"Foreign conversation"}}));
    records.push(json!({"type":if provider == AgentKind::Claude { "user" } else { "message" },"sessionId":"saved-id","cwd":cwd,"message":{"role":"user","content":content}}));
    let file = saved(directory.path(), provider, "saved-id", &records, 20).unwrap();
    let original = fs::read(&file).unwrap();
    let entries =
        terminal_provider_history(&query(provider, directory.path(), Some(&cwd))).unwrap();
    assert_eq!(entries[0].title.as_deref(), Some("Fix the history picker"));
    assert_eq!(fs::read(file).unwrap(), original);
}

#[rstest]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
fn saved_history_retains_exact_identity_account_project_dates_and_stored_title(
    #[case] provider: AgentKind,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let cwd = project(directory.path(), "workspace with spaces").unwrap();
    let records = if provider == AgentKind::Claude {
        vec![
            json!({"type":"user","sessionId":"exact-id","cwd":cwd,"timestamp":"2026-01-01T00:00:00.000Z","message":{"content":"raw secret transcript"}}),
            json!({"type":"ai-title","sessionId":"exact-id","aiTitle":"Generated title"}),
            json!({"type":"custom-title","sessionId":"other-id","customTitle":"Foreign title"}),
            json!({"type":"custom-title","sessionId":"exact-id","customTitle":"Saved title"}),
        ]
    } else {
        vec![
            json!({"type":"session","version":3,"id":"exact-id","cwd":cwd,"timestamp":"2026-01-01T00:00:00.000Z"}),
            json!({"type":"message","id":"message-id","message":{"role":"user","content":"raw secret transcript"}}),
            json!({"type":"session_info","id":"metadata-id","name":"Saved title"}),
        ]
    };
    let file = saved(
        directory.path(),
        provider,
        "exact-id",
        &records,
        1_767_225_700,
    )
    .unwrap();
    let original = fs::read(&file).unwrap();
    let entries =
        terminal_provider_history(&query(provider, directory.path(), Some(&cwd))).unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.provider, provider);
    assert_eq!(entry.session_id, "exact-id");
    assert_eq!(entry.title.as_deref(), Some("Saved title"));
    assert_eq!(entry.cwd, cwd);
    assert_eq!(entry.account_directory, directory.path());
    assert_eq!(entry.created_at, Some(1_767_225_600_000));
    assert_eq!(entry.updated_at, Some(1_767_225_700_000));
    let resume_id = if provider == AgentKind::Pi {
        file.canonicalize().unwrap().to_str().unwrap().to_owned()
    } else {
        "exact-id".to_owned()
    };
    assert_eq!(entry.resume_id, resume_id);
    assert!(
        !serde_json::to_string(&entries)
            .unwrap()
            .contains("raw secret")
    );
    assert_eq!(fs::read(file).unwrap(), original);
}

#[cfg(unix)]
#[rstest]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
#[case(AgentKind::Codex)]
fn existing_project_aliases_match_without_rewriting_provider_metadata(
    #[case] provider: AgentKind,
    #[values(false, true)] saved_alias: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let actual = project(directory.path(), "workspace")
        .unwrap()
        .canonicalize()
        .unwrap();
    let alias = directory.path().join("workspace-link");
    std::os::unix::fs::symlink(&actual, &alias).unwrap();
    let (stored, requested) = if saved_alias {
        (&alias, &actual)
    } else {
        (&actual, &alias)
    };
    let executable;
    let mut request = query(provider, directory.path(), Some(requested));
    if provider == AgentKind::Codex {
        let pages =
            [json!({"data":[{"id":"saved-id","cwd":stored,"ephemeral":false}],"nextCursor":null})];
        executable = program(&directory, &codex_script(directory.path(), &pages).unwrap()).unwrap();
        request.program = &executable;
    } else {
        let record = if provider == AgentKind::Claude {
            json!({"type":"user","sessionId":"saved-id","cwd":stored})
        } else {
            json!({"type":"session","version":3,"id":"saved-id","cwd":stored})
        };
        saved(directory.path(), provider, "saved-id", &[record], 20).unwrap();
    }
    let entries = terminal_provider_history(&request).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].cwd, *stored);
    assert_eq!(entries[0].session_id, "saved-id");
    assert_eq!(entries[0].account_directory, directory.path());
}

#[rstest]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
fn nonexistent_project_paths_require_exact_metadata_scope(
    #[case] provider: AgentKind,
    #[values(false, true)] same: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let requested = directory.path().join("missing-project");
    let stored = if same {
        requested.clone()
    } else {
        directory.path().join("other-missing-project")
    };
    let record = if provider == AgentKind::Claude {
        json!({"type":"user","sessionId":"saved-id","cwd":stored})
    } else {
        json!({"type":"session","version":3,"id":"saved-id","cwd":stored})
    };
    saved(directory.path(), provider, "saved-id", &[record], 20).unwrap();
    let entries =
        terminal_provider_history(&query(provider, directory.path(), Some(&requested))).unwrap();
    assert_eq!(entries.len(), usize::from(same));
    let all = terminal_provider_history(&query(provider, directory.path(), None)).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].cwd, stored);
}

#[rstest]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
fn history_without_stored_title_does_not_use_transcript_prompts(#[case] provider: AgentKind) {
    let directory = assert_fs::TempDir::new().unwrap();
    let cwd = project(directory.path(), "workspace").unwrap();
    let header = if provider == AgentKind::Claude {
        json!({"type":"user","sessionId":"saved-id","cwd":cwd,"timestamp":"2026-01-01T01:00:00+01:00","message":{"content":"first prompt secret"}})
    } else {
        json!({"type":"session","version":3,"id":"saved-id","cwd":cwd,"timestamp":"2026-01-01T01:00:00+01:00"})
    };
    saved(
        directory.path(),
        provider,
        "saved-id",
        &[header],
        1_767_225_700,
    )
    .unwrap();
    let entries = terminal_provider_history(&query(provider, directory.path(), None)).unwrap();
    assert_eq!(entries[0].title, None);
    assert_eq!(entries[0].created_at, Some(1_767_225_600_000));
}

#[rstest]
fn history_filters_project_metadata_and_applies_latest_name_and_recency_limit() {
    let directory = assert_fs::TempDir::new().unwrap();
    let first = project(directory.path(), "first workspace").unwrap();
    let other = project(directory.path(), "other workspace").unwrap();
    for (id, cwd, modified) in [
        ("old-id", &first, 10),
        ("new-id", &first, 30),
        ("other-id", &other, 50),
    ] {
        saved(
            directory.path(),
            AgentKind::Pi,
            id,
            &[
                json!({"type":"session","version":3,"id":id,"cwd":cwd,"timestamp":"2026-01-01T00:00:00Z"}),
                json!({"type":"session_info","name":"Earlier name"}),
                json!({"type":"session_info","name":"Latest name"}),
            ],
            modified,
        ).unwrap();
    }
    let mut request = query(AgentKind::Pi, directory.path(), Some(&first));
    request.limit = 1;
    let entries = terminal_provider_history(&request).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id, "new-id");
    assert_eq!(entries[0].title.as_deref(), Some("Latest name"));
    assert_eq!(
        terminal_provider_history(&query(AgentKind::Pi, directory.path(), Some(&other))).unwrap()
            [0]
        .session_id,
        "other-id"
    );
}

#[rstest]
#[case(json!({"type":"session","version":4,"id":"saved-id","cwd":"/workspace"}))]
#[case(json!({"type":"session","version":3,"id":"../outside","cwd":"/workspace"}))]
#[case(json!({"type":"session","version":3,"id":"saved-id","cwd":"relative"}))]
#[case(json!({"type":"message","message":{"content":"private malformed metadata"}}))]
fn unsupported_or_invalid_session_metadata_is_a_bounded_error(#[case] header: Value) {
    let directory = assert_fs::TempDir::new().unwrap();
    saved(directory.path(), AgentKind::Pi, "saved-id", &[header], 10).unwrap();
    let error =
        terminal_provider_history(&query(AgentKind::Pi, directory.path(), None)).unwrap_err();
    assert!(!error.contains("private"));
}

#[rstest]
fn claude_generated_title_relocation_and_sidechain_metadata_follow_provider_identity() {
    let directory = assert_fs::TempDir::new().unwrap();
    let relocated = project(directory.path(), "relocated").unwrap();
    saved(
        directory.path(),
        AgentKind::Claude,
        "saved-id",
        &[
            json!({"type":"user","sessionId":"saved-id","cwd":"/original","timestamp":"2026-01-01T00:00:00Z"}),
            json!({"type":"ai-title","sessionId":"saved-id","aiTitle":"Generated saved title"}),
            json!({"type":"relocated","sessionId":"saved-id","relocatedCwd":relocated}),
        ],
        20,
    ).unwrap();
    saved(
        directory.path(),
        AgentKind::Claude,
        "agent-id",
        &[json!({"type":"user","sessionId":"agent-id","cwd":relocated,"isSidechain":true})],
        30,
    )
    .unwrap();
    let entries = terminal_provider_history(&query(
        AgentKind::Claude,
        directory.path(),
        Some(&relocated),
    ))
    .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title.as_deref(), Some("Generated saved title"));
    assert_eq!(entries[0].cwd, relocated);
    assert_eq!(
        terminal_provider_history(&query(
            AgentKind::Claude,
            directory.path(),
            Some(Path::new("/original"))
        ))
        .unwrap(),
        Vec::new()
    );
}

#[rstest]
fn large_transcripts_only_contribute_header_and_current_tail_metadata() {
    let directory = assert_fs::TempDir::new().unwrap();
    let cwd = project(directory.path(), "workspace").unwrap();
    let path = saved(
        directory.path(),
        AgentKind::Pi,
        "saved-id",
        &[
            json!({"type":"session","version":3,"id":"saved-id","cwd":cwd,"timestamp":"2026-01-01T00:00:00Z"}),
            json!({"type":"session_info","name":"Obsolete prefix name"}),
            json!({"type":"message","message":{"content":"secret payload".repeat(20_000)}}),
            json!({"type":"session_info","name":"Current tail name"}),
        ],
        20,
    ).unwrap();
    let original = fs::read(&path).unwrap();
    let entries =
        terminal_provider_history(&query(AgentKind::Pi, directory.path(), Some(&cwd))).unwrap();
    assert_eq!(entries[0].title.as_deref(), Some("Current tail name"));
    assert!(!serde_json::to_string(&entries).unwrap().contains("payload"));
    assert_eq!(fs::read(path).unwrap(), original);
}

#[cfg(unix)]
#[rstest]
fn session_symlinks_and_nested_subagents_cannot_escape_selected_account() {
    let directory = assert_fs::TempDir::new().unwrap();
    let outside = assert_fs::TempDir::new().unwrap();
    let external = saved(
        outside.path(),
        AgentKind::Pi,
        "external-id",
        &[json!({"type":"session","version":3,"id":"external-id","cwd":"/outside"})],
        20,
    )
    .unwrap();
    let store = project(directory.path(), "sessions/project").unwrap();
    std::os::unix::fs::symlink(external, store.join("linked.jsonl")).unwrap();
    let nested = project(&store, "subagents").unwrap();
    fs::write(nested.join("agent.jsonl"), "private nested session").unwrap();
    assert_eq!(
        terminal_provider_history(&query(AgentKind::Pi, directory.path(), None)).unwrap(),
        Vec::new()
    );
    fs::remove_dir_all(directory.path().join("sessions")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("sessions"),
        directory.path().join("sessions"),
    )
    .unwrap();
    assert!(terminal_provider_history(&query(AgentKind::Pi, directory.path(), None)).is_err());
}

#[rstest]
fn missing_history_is_empty_and_scan_budget_failure_is_explicit() {
    let directory = assert_fs::TempDir::new().unwrap();
    assert_eq!(
        terminal_provider_history(&query(AgentKind::Pi, directory.path(), None)).unwrap(),
        Vec::new()
    );
    let store = project(directory.path(), "sessions").unwrap();
    // Files do not need transcript content to exercise the directory-entry bound.
    for id in 0..4097 {
        fs::write(store.join(format!("unrelated-{id}")), "").unwrap();
    }
    assert!(
        terminal_provider_history(&query(AgentKind::Pi, directory.path(), None))
            .unwrap_err()
            .contains("4096")
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]
    #[test]
    fn another_account_cannot_supply_selected_account_history(id in "[a-z][a-z0-9-]{1,20}") {
        let selected = assert_fs::TempDir::new().unwrap();
        let other = assert_fs::TempDir::new().unwrap();
        saved(other.path(), AgentKind::Pi, &id, &[json!({"type":"session","version":3,"id":id,"cwd":"/workspace"})], 20).unwrap();
        prop_assert!(terminal_provider_history(&query(AgentKind::Pi, selected.path(), Some(Path::new("/workspace")))).unwrap().is_empty());
    }
}

#[cfg(unix)]
fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn program(directory: &assert_fs::TempDir, script: &str) -> FixtureResult<String> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = directory.path().join("codex");
    fs::write(&path, format!("#!/bin/sh\n{script}\n"))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path
        .to_str()
        .ok_or("Fixture program path is not UTF-8")?
        .to_owned())
}

#[cfg(unix)]
fn codex_script(account: &Path, pages: &[Value]) -> FixtureResult<String> {
    use std::fmt::Write as _;
    let mut script = format!(
        "[ \"$CODEX_HOME\" = {} ] || exit 9\n[ \"$*\" = 'app-server --listen stdio://' ] || exit 9\nIFS= read -r initialize\nprintf '%s\\n' '{{\"id\":1,\"result\":{{}}}}'\nIFS= read -r initialized\n",
        quoted(
            account
                .to_str()
                .ok_or("Fixture account path is not UTF-8")?
        )
    );
    for (index, page) in pages.iter().enumerate() {
        write!(
            script,
            "IFS= read -r request\ncase \"$request\" in *'\"method\":\"thread/list\"'*'\"useStateDbOnly\":true'*) ;; *) exit 9 ;; esac\nprintf '%s\\n' {}\n",
            quoted(&json!({"id":index.saturating_add(2),"result":page}).to_string())
        )?;
    }
    Ok(script)
}

#[cfg(unix)]
#[rstest]
fn codex_public_history_pages_retain_thread_id_without_preview_or_turns() {
    let directory = assert_fs::TempDir::new().unwrap();
    let cwd = project(directory.path(), "workspace").unwrap();
    let pages = [
        json!({"data":[{"id":"thread-one","sessionId":"root-tree-id","name":"Saved thread title","cwd":cwd,"createdAt":10,"updatedAt":20,"ephemeral":false,"preview":"secret preview","turns":["secret transcript"]}],"nextCursor":"next-page"}),
        json!({"data":[{"id":"thread-two","cwd":cwd,"createdAt":5,"updatedAt":15,"ephemeral":false}],"nextCursor":null}),
    ];
    let program = program(&directory, &codex_script(directory.path(), &pages).unwrap()).unwrap();
    let mut request = query(AgentKind::Codex, directory.path(), Some(&cwd));
    request.program = &program;
    let entries = terminal_provider_history(&request).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].session_id, "thread-one");
    assert_eq!(entries[0].resume_id, "thread-one");
    assert_eq!(entries[0].created_at, Some(10_000));
    assert_eq!(entries[0].updated_at, Some(20_000));
    assert_eq!(entries[0].title.as_deref(), Some("Saved thread title"));
    assert_eq!(entries[1].title, None);
    assert!(!serde_json::to_string(&entries).unwrap().contains("secret"));
}

#[cfg(unix)]
#[rstest]
#[case(json!({"data":[{"id":"foreign-id","cwd":"/another-project","ephemeral":false}],"nextCursor":null}))]
#[case(json!({"data":[{"id":"../outside","cwd":"/workspace","ephemeral":false}],"nextCursor":null}))]
#[case(json!({"data":[{"id":"saved-id","cwd":"/workspace","updatedAt":i64::MAX,"ephemeral":false}],"nextCursor":null}))]
#[case(json!({"data":[{"id":"saved-id","cwd":"/workspace","ephemeral":false},{"id":"saved-id","cwd":"/workspace","ephemeral":false}],"nextCursor":null}))]
fn codex_invalid_or_ambiguous_history_fails_closed(#[case] page: Value) {
    let directory = assert_fs::TempDir::new().unwrap();
    let cwd = project(directory.path(), "workspace").unwrap();
    let page = page
        .to_string()
        .replace("/workspace", cwd.to_str().unwrap());
    let page: Value = serde_json::from_str(&page).unwrap();
    let program = program(
        &directory,
        &codex_script(directory.path(), &[page]).unwrap(),
    )
    .unwrap();
    let mut request = query(AgentKind::Codex, directory.path(), Some(&cwd));
    request.program = &program;
    assert!(terminal_provider_history(&request).is_err());
}

#[cfg(unix)]
#[rstest]
fn codex_malformed_history_reaps_the_owned_process_and_redacts_provider_errors() {
    let directory = assert_fs::TempDir::new().unwrap();
    let pid_file = directory.path().join("provider.pid");
    let script = format!(
        "printf '%s' \"$$\" > {}\nprintf '%s\\n' '{{malformed secret response}}'\nexec /bin/cat",
        quoted(pid_file.to_str().unwrap())
    );
    let program = program(&directory, &script).unwrap();
    let mut request = query(AgentKind::Codex, directory.path(), None);
    request.program = &program;
    let error = terminal_provider_history(&request).unwrap_err();
    assert!(!error.contains("secret"));
    let pid = fs::read_to_string(pid_file).unwrap();
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", &pid])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}
