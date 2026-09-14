use bootty_config::config::{WslDistribution, WslRemoteConfig};
use bootty_host::{
    CommandBytes, CommandOutput, CommandRunner,
    remote::RemoteHost,
    wsl::{WslRemote, decode_wsl_text, distributions},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

struct Listing(Vec<u8>);
impl CommandRunner for Listing {
    fn run(&self, _: &str, _: &[String]) -> anyhow::Result<CommandOutput> {
        anyhow::bail!("text decoding would lose bytes")
    }
    fn run_bytes(&self, program: &str, args: &[String]) -> anyhow::Result<CommandBytes> {
        assert_eq!(program, "wsl.exe");
        assert_eq!(args, ["--list", "--quiet"]);
        Ok(CommandBytes {
            success: true,
            stdout: self.0.clone(),
            stderr: vec![],
        })
    }
}

#[rstest]
#[case(false)]
#[case(true)]
fn distribution_listing_decodes_before_splitting_lines(#[case] bom: bool) {
    let text = "Ubuntu\r\n開発Linux\r\nUbuntu\r\n";
    let mut bytes = if bom { vec![0xff, 0xfe] } else { vec![] };
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    let names = distributions(&Listing(bytes)).unwrap();
    assert_eq!(
        names
            .iter()
            .map(WslDistribution::as_str)
            .collect::<Vec<_>>(),
        ["Ubuntu", "開発Linux"]
    );
}
#[rstest]
#[case(vec![0xff, 0xfe, 65])]
#[case(vec![0xff, 0xfe, 0, 0xd8])]
#[case(vec![0xff])]
fn invalid_list_output_is_not_silently_replaced(#[case] bytes: Vec<u8>) {
    assert!(decode_wsl_text(&bytes).is_err());
}
#[rstest]
fn host_identity_and_command_arguments_keep_the_distribution_boundary() {
    let config = WslRemoteConfig {
        distribution: WslDistribution::new("Ubuntu 開発").unwrap(),
    };
    let remote = WslRemote::new(config.clone());
    let args = ["a b", "$(touch bad)", "'quoted'", "/home/user/file"].map(str::to_owned);
    let (program, actual) = remote.command("/bin/cat", &args);
    assert_eq!(program, "wsl.exe");
    assert_eq!(
        &actual[..6],
        [
            "--distribution",
            "Ubuntu 開発",
            "--cd",
            "~",
            "--exec",
            "/bin/cat"
        ]
    );
    assert_eq!(&actual[6..], args);
    let host = RemoteHost::new(config);
    assert!(host.as_ssh().is_none());
    assert!(
        bootty_host::files::host_identity(Some(&host.target()))
            .expect("file host namespace")
            .starts_with("wsl:")
    );
}
proptest! {
    #[test]
    fn utf16_round_trip(text in "[^\\p{Cc}]{0,100}") {
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        prop_assert_eq!(decode_wsl_text(&bytes).unwrap(), text);
    }
}

#[cfg(unix)]
#[rstest]
fn process_runners_preserve_non_utf8_output() {
    let args = ["-c", "printf '\\377\\376U\\000\\012\\000'"].map(str::to_owned);
    for output in [
        bootty_host::SystemCommandRunner
            .run_bytes("/bin/sh", &args)
            .unwrap(),
        bootty_host::CancellableCommandRunner::new(bootty_host::CommandCancellation::default())
            .run_bytes("/bin/sh", &args)
            .unwrap(),
    ] {
        assert!(output.success);
        assert_eq!(decode_wsl_text(&output.stdout).unwrap(), "U\n");
    }
}
