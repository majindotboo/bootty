#![cfg(unix)]
use bootty_config::config::SshRemoteConfig;
use bootty_host::{
    CommandOutput, CommandRunner,
    ssh::SshRemote,
    ssh_forward::{ForwardLease, loopback_destination},
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::cell::RefCell;
use url::Url;

#[derive(Default)]
struct Runner {
    calls: RefCell<Vec<Vec<String>>>,
    fail: bool,
}
impl CommandRunner for Runner {
    fn run(&self, _: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        self.calls.borrow_mut().push(args.to_vec());
        Ok(CommandOutput {
            success: !self.fail,
            stdout: String::new(),
            stderr: if self.fail {
                "forward rejected".to_owned()
            } else {
                String::new()
            },
        })
    }
}

#[rstest]
#[case("http://localhost:3000/a?x=1#part", "localhost")]
#[case("https://127.0.0.1/a", "127.0.0.1")]
#[case("http://0.0.0.0:8080/a", "127.0.0.1")]
fn forwarded_urls_preserve_resource_and_use_a_private_control_master(
    #[case] input: &str,
    #[case] host: &str,
) {
    let mut config = SshRemoteConfig::for_host("development");
    config.args = vec!["-S".to_owned(), "/user/existing-master".to_owned()];
    let remote = SshRemote::new(config);
    let runner = Runner::default();
    let original = Url::parse(input).unwrap();
    let lease = ForwardLease::start(remote.clone(), &original, &runner).unwrap();
    let opened = Url::parse(&lease.url(original.clone()).unwrap()).unwrap();
    assert_eq!(
        (
            opened.host_str(),
            opened.path(),
            opened.query(),
            opened.fragment()
        ),
        (
            Some(host),
            original.path(),
            original.query(),
            original.fragment()
        )
    );
    assert!(opened.port().is_some());
    assert!(lease.matches(&remote, &original));
    assert!(!lease.matches(
        &SshRemote::new(SshRemoteConfig::for_host("other")),
        &original
    ));
    lease.check(&runner).unwrap();
    let calls = runner.calls.borrow();
    let start = &calls[0];
    assert!(
        start
            .windows(2)
            .any(|p| p == ["-o", "ExitOnForwardFailure=yes"])
    );
    let control_index = start
        .iter()
        .position(|value| value.starts_with("ControlPath="))
        .unwrap();
    assert!(control_index < start.iter().position(|value| value == "-S").unwrap());
    assert!(!start[control_index].contains("/user/existing-master"));
    assert!(calls[1].windows(2).any(|p| p == ["-O", "check"]));
}

#[rstest]
fn rejected_forward_never_returns_a_usable_url() {
    let error = ForwardLease::start(
        SshRemote::new(SshRemoteConfig::for_host("development")),
        &Url::parse("http://127.0.0.1:3000/").unwrap(),
        &Runner {
            fail: true,
            ..Runner::default()
        },
    )
    .err()
    .unwrap();
    assert!(format!("{error:#}").contains("forward rejected"));
}

#[rstest]
#[case("https://example.com")]
#[case("http://192.168.1.2:3000")]
#[case("ftp://localhost")]
fn ordinary_destinations_do_not_create_forwards(#[case] input: &str) {
    assert!(loopback_destination(&Url::parse(input).unwrap()).is_none());
}

#[rstest]
fn retry_has_a_new_identity_and_preserves_the_original_host_and_resource() {
    let remote = SshRemote::new(SshRemoteConfig::for_host("captured-host"));
    let runner = Runner::default();
    let original = Url::parse("http://localhost:3000/resource?q=x").unwrap();
    let first = ForwardLease::start(remote, &original, &runner).unwrap();
    let second = first.restart(&runner).unwrap();
    assert_ne!(first.id(), second.id());
    assert_eq!(first.info().host, second.info().host);
    assert_eq!(second.info().source_url, original.to_string());
    assert_eq!(second.info().remote_port, 3000);
    first.check(&runner).unwrap();
    first.close(&runner).unwrap();
    second.check(&runner).unwrap();
    second.close(&runner).unwrap();
}
