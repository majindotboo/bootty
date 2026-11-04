use clap::Parser;
use rstest::rstest;
use xtasks::build::BuildArgs;

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    build: BuildArgs,
}

#[rstest]
fn build_defaults_to_development_and_accepts_release() {
    assert!(!Cli::try_parse_from(["build"]).unwrap().build.release);
    assert!(
        Cli::try_parse_from(["build", "--release"])
            .unwrap()
            .build
            .release
    );
}
