#![cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt as _};

use assert_fs::TempDir;
use bootty_agents::{
    AgentKind, GeneratedSessionNames, NativeSessionConfig, generate_session_names,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

proptest! {
    #[test]
    fn portable_names_preserve_unicode_titles(title in "[A-Za-z😀漢字]{1,60}", slug in "[a-z][a-z0-9]{0,40}") {
        let names = GeneratedSessionNames { title, slug };
        prop_assert!(names.validate().is_ok());
        let encoded = serde_json::to_vec(&names).unwrap();
        prop_assert_eq!(serde_json::from_slice::<GeneratedSessionNames>(&encoded).unwrap(), names);
    }
}

#[rstest]
#[case("fix-composer", false, true)]
#[case("../escape", false, false)]
#[case("fix-composer", true, false)]
fn naming_uses_a_private_ephemeral_query_and_rejects_invalid_output(
    #[case] slug: &str,
    #[case] cancelled: bool,
    #[case] accepted: bool,
) {
    let root = TempDir::new().expect("temporary account");
    let script = root.path().join("provider.py");
    fs::write(
        &script,
        r"#!/usr/bin/env python3
import json,os,pathlib,sys
account=pathlib.Path(os.environ['CODEX_HOME'])
args=sys.argv[1:]
assert 'exec' in args and '--ephemeral' in args
assert args[args.index('--sandbox')+1]=='read-only'
assert args[args.index('--model')+1]=='quick-model'
assert args.count('--model')==1
assert 'features.shell_tool=false' in args and 'mcp_servers={}' in args
assert 'Fix the composer' in args[-1]
assert pathlib.Path.cwd()!=account
schema=pathlib.Path(args[args.index('--output-schema')+1])
assert json.loads(schema.read_text())['additionalProperties']==False
(account/'query-directory').write_text(str(pathlib.Path.cwd()))
output=pathlib.Path(args[args.index('--output-last-message')+1])
output.write_text(json.dumps({'title':'Fix composer','slug':(account/'slug').read_text()}))
",
    )
    .expect("provider script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).expect("executable provider");
    fs::write(root.path().join("slug"), slug).expect("response slug");
    let mut config = NativeSessionConfig::new(AgentKind::Codex, root.path());
    config.program = script.to_string_lossy().into_owned();
    config.account_directory = Some(root.path().to_string_lossy().into_owned());
    config.arguments = vec!["--model".into(), "conversation-model".into()];
    let result = generate_session_names(&config, "quick-model", "Fix the composer", || cancelled);
    assert_eq!(result.is_ok(), accepted);
    if cancelled {
        assert!(
            !root.path().join("query-directory").exists(),
            "cancelled queries cannot launch providers"
        );
    } else {
        let path = fs::read_to_string(root.path().join("query-directory"))
            .expect("observed private query");
        assert!(
            !std::path::Path::new(&path).exists(),
            "private query files are removed on success and failure"
        );
    }
    if let Ok(names) = result {
        assert_eq!(names.title, "Fix composer");
    }
}
