use bootty_mux::workspace::terminal_cwd_for_mux_command;
use rstest::rstest;

#[rstest]
#[case("file:///C:/Users/Developer/project", "C:/Users/Developer/project")]
#[case("file://localhost/c:/work/project", "c:/work/project")]
#[case("file:///C:/work/a%20b/%C3%A9", "C:/work/a b/é")]
#[case("file://remote/home/developer/project", "/home/developer/project")]
#[case("file:///home/developer/project", "/home/developer/project")]
#[case("file:///workspace:project/files", "/workspace:project/files")]
#[case("file:///C:relative", "/C:relative")]
#[case("C:\\work\\project", "C:\\work\\project")]
fn terminal_locations_preserve_the_destination_hosts_path(
    #[case] observed: &str,
    #[case] expected: &str,
) {
    assert_eq!(
        terminal_cwd_for_mux_command(Some(observed.to_owned()), Some("anchor".to_owned())),
        Some(expected.to_owned())
    );
}

#[rstest]
#[case("")]
#[case("file://host")]
#[case("file:///work/%FF")]
#[case("file:///work/%0")]
fn unusable_terminal_locations_keep_the_anchor(#[case] observed: &str) {
    assert_eq!(
        terminal_cwd_for_mux_command(Some(observed.to_owned()), Some("anchor".to_owned())),
        Some("anchor".to_owned())
    );
}
