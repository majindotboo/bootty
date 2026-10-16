use bootty_ui::presentation::markdown_images::markdown_image_path;
use pretty_assertions::assert_eq;
use rstest::rstest;

/// A reply's image is a file on the conversation's host when written as a path: absolute as
/// written, `file://` as its decoded path, and relative from the conversation's directory. Web
/// and data URLs, and home paths, aren't files to read.
#[rstest]
#[case::relative("chart.png", Some("/work/project/chart.png"))]
#[case::dot_relative("./out/chart.png", Some("/work/project/out/chart.png"))]
#[case::absolute("/tmp/shot.png", Some("/tmp/shot.png"))]
#[case::file_url("file:///tmp/my%20shot.png", Some("/tmp/my shot.png"))]
#[case::windows("C:\\shots\\one.png", Some("C:\\shots\\one.png"))]
#[case::web("https://example.com/a.png", None)]
#[case::web_upper("HTTP://example.com/a.png", None)]
#[case::data("data:image/png;base64,AAAA", None)]
#[case::home("~/shot.png", None)]
fn reply_images_name_host_files(#[case] url: &str, #[case] expected: Option<&str>) {
    assert_eq!(
        markdown_image_path(url, "/work/project/"),
        expected.map(str::to_owned)
    );
}
