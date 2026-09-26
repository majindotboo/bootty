#![cfg(unix)]

use assert_fs::prelude::*;
use bootty_host::{
    files::{FileRequest, FileResponse},
    media::{MAX_MEDIA_CHUNK, MediaDescriptor, MediaReader},
    remote::RemoteHost,
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    io::{Read as _, Seek as _, SeekFrom},
    os::unix::fs::PermissionsExt as _,
};

fn descriptor(path: &std::path::Path) -> anyhow::Result<MediaDescriptor> {
    let FileResponse::Media(descriptor) = (FileRequest::Read {
        path: path.to_string_lossy().into_owned(),
    })
    .execute()?
    else {
        anyhow::bail!("expected media metadata")
    };
    Ok(descriptor)
}

fn remote(directory: &assert_fs::TempDir, body: &str) -> anyhow::Result<RemoteHost> {
    let script = directory.child("ssh-fixture");
    let home = bootty_host::shell_quote(&directory.path().to_string_lossy());
    let program = bootty_host::shell_quote(env!("CARGO_BIN_EXE_bootty-daemon"));
    script.write_str(&format!(
        r#"#!/bin/sh
export HOME={home}
export XDG_CONFIG_HOME={home}
export XDG_STATE_HOME={home}
export BOOTTY_APPLICATION_IDENTITY=bootty-dev
export BOOTTY_DEVELOPMENT_NAMESPACE=bootty-dev-7f11e00000000009
unset BOOTTY_DAEMON_STATE
for arg do last=$arg; done
case "$last" in
  *remote-ping) printf '%s\n' '{}:{}';;
  *)
    printf 'x' >> {home}/connections
    {body}
    ;;
esac
"#,
        bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION,
        env!("CARGO_PKG_VERSION"),
        body = body.replace("DAEMON", &program)
    ))?;
    std::fs::set_permissions(script.path(), std::fs::Permissions::from_mode(0o700))?;
    let mut config = bootty_config::config::SshRemoteConfig::for_host("fixture");
    config.program = script.path().to_string_lossy().into_owned();
    Ok(RemoteHost::new(config))
}

#[rstest]
fn seekable_remote_reads_use_one_real_daemon_channel() {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("image without extension");
    let mut bytes: Vec<u8> = (0..251_u8).cycle().take(2 * MAX_MEDIA_CHUNK + 13).collect();
    bytes[..6].copy_from_slice(b"GIF89a");
    file.write_binary(&bytes).unwrap();
    let remote = remote(
        &directory,
        "exec DAEMON --application-identity bootty-dev remote-exec \"${last##* }\"",
    )
    .unwrap();
    let mut source = MediaReader::open(&descriptor(file.path()).unwrap(), Some(&remote)).unwrap();
    let mut actual = [0; 13];
    for offset in [
        0,
        bytes.len().saturating_sub(actual.len()),
        MAX_MEDIA_CHUNK - 3,
        0,
    ] {
        source
            .seek(SeekFrom::Start(u64::try_from(offset).unwrap()))
            .unwrap();
        source.read_exact(&mut actual).unwrap();
        assert_eq!(actual, bytes[offset..offset.saturating_add(actual.len())]);
    }
    let cancellation = source.cancellation();
    cancellation.cancel();
    assert_eq!(
        source.read(&mut actual).unwrap_err().kind(),
        std::io::ErrorKind::ConnectionAborted
    );
    assert_eq!(
        source.read_exact(&mut actual).unwrap_err().kind(),
        std::io::ErrorKind::ConnectionAborted
    );
    drop(source);
    directory.child("connections").assert("x");
}

#[rstest]
#[case("printf '%s\\n' '{\"status\":\"data\",\"length\":10}'; printf 'bad'")]
#[case("printf '%s\\n' '{\"status\":\"data\",\"length\":1048577}'")]
#[case("printf '%s\\n' '{\"status\":\"ready\",\"len\":10,\"revision\":\"wrong\"}'")]
fn invalid_remote_bodies_fail_and_retire_the_channel(#[case] reply: &str) {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("source");
    file.write_binary(b"GIF89adata").unwrap();
    let descriptor = descriptor(file.path()).unwrap();
    let ready =
        serde_json::json!({"status":"ready", "len":descriptor.len, "revision":descriptor.revision})
            .to_string();
    let body = format!(
        "printf '%s\\n' {}; IFS= read -r request; {reply}",
        bootty_host::shell_quote(&ready)
    );
    let remote = remote(&directory, &body).unwrap();
    let mut source = MediaReader::open(&descriptor, Some(&remote)).unwrap();
    assert!(source.read_exact(&mut [0; 1]).is_err());
    assert_eq!(
        source.read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::ConnectionAborted
    );
    assert_eq!(
        source.read_exact(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::ConnectionAborted
    );
    drop(source);
    directory.child("connections").assert("x");
}
