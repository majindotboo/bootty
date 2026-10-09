use bootty_control::{Caller, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind};
use bootty_host::files::{FileRequest, FileResponse};
use bootty_ui::attachment_source::FileCompletionSource;
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::sync::{Arc, Mutex};

#[rstest]
#[case::empty(Vec::new(), false, true)]
#[case::binary_across_chunks(vec![0xff; 2 * 1024 * 1024 + 17], false, true)]
#[case::changed(vec![0xff; 17], true, false)]
fn file_mentions_stage_verified_bytes_under_the_original_name(
    #[case] contents: Vec<u8>,
    #[case] change_after_descriptor: bool,
    #[case] accepted: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("original.bin");
    std::fs::write(&path, &contents).unwrap();
    let response = FileRequest::OpenReader {
        path: path.to_string_lossy().into_owned(),
        root: directory.path().to_string_lossy().into_owned(),
    }
    .execute()
    .unwrap();
    if change_after_descriptor {
        std::fs::write(&path, b"changed").unwrap();
    }
    let target = CommandTarget {
        kind: ResourceKind::Binding,
        handle: "captured-binding".into(),
        generation: 8,
    };
    let mut invocation = CommandInvocation::new(
        "files.source",
        vec![
            path.to_string_lossy().into_owned(),
            directory.path().to_string_lossy().into_owned(),
        ],
        Caller::Internal,
    );
    invocation.target = Some(target);
    let source = FileCompletionSource {
        invocation: invocation.clone(),
        remote: None,
    };
    let mailbox = Arc::new(Mutex::new(None::<bootty_control::AppCommandReceiver>));
    let inbox = mailbox.clone();
    let expected = invocation;
    let (sender, receiver) = bootty_control::app_command_channel(
        1,
        Arc::new(move || {
            let request = inbox.lock().unwrap().as_ref().unwrap().try_recv().unwrap();
            assert_eq!(request.invocation, expected);
            request
                .response
                .send(CommandOutcome::Success {
                    value: serde_json::to_value(&response).unwrap(),
                    warnings: Vec::new(),
                })
                .unwrap();
        }),
    );
    *mailbox.lock().unwrap() = Some(receiver);
    let result = source.stage(&sender.for_caller(Caller::Internal));
    assert_eq!(result.is_ok(), accepted);
    if let Ok(staged) = result {
        assert_eq!(staged.path.file_name().unwrap(), "original.bin");
        assert_eq!(std::fs::read(&staged.path).unwrap(), contents);
        let staged_path = staged.path.clone();
        let retained = staged.temporary.clone();
        drop(staged);
        assert!(staged_path.exists());
        drop(retained);
        assert!(!staged_path.exists());
    }
}

#[rstest]
#[case::oversized(bootty_agents::MAX_NATIVE_ATTACHMENT_FILE_BYTES + 1, "file.bin")]
#[case::unsafe_name(0, "../outside.bin")]
fn rejected_file_sources_never_open_or_stage_their_bytes(#[case] len: u64, #[case] name: &str) {
    let response = FileResponse::Source(bootty_host::file_reader::FileDescriptor {
        path: "/missing-source".into(),
        name: name.into(),
        revision: "missing".into(),
        len,
    });
    let mailbox = Arc::new(Mutex::new(None::<bootty_control::AppCommandReceiver>));
    let inbox = mailbox.clone();
    let (sender, receiver) = bootty_control::app_command_channel(
        1,
        Arc::new(move || {
            let request = inbox.lock().unwrap().as_ref().unwrap().try_recv().unwrap();
            request
                .response
                .send(CommandOutcome::Success {
                    value: serde_json::to_value(&response).unwrap(),
                    warnings: Vec::new(),
                })
                .unwrap();
        }),
    );
    *mailbox.lock().unwrap() = Some(receiver);
    let source = FileCompletionSource {
        invocation: CommandInvocation::from_action("files.source", Caller::Internal),
        remote: None,
    };
    let error = source
        .stage(&sender.for_caller(Caller::Internal))
        .err()
        .unwrap();
    assert_eq!(
        error,
        if len > bootty_agents::MAX_NATIVE_ATTACHMENT_FILE_BYTES {
            "File exceeds 50 MB"
        } else {
            "Attachment host returned an invalid file name"
        }
    );
}
