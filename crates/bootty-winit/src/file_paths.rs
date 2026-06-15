use std::path::Path;

pub fn format_file_paths_for_paste<'a>(
    paths: impl IntoIterator<Item = &'a Path>,
) -> Option<String> {
    let formatted = paths.into_iter().map(shell_quote_path).collect::<Vec<_>>();
    if formatted.is_empty() {
        None
    } else {
        Some(formatted.join(" "))
    }
}

fn shell_quote_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if path.chars().all(is_unquoted_shell_path_char) {
        return path.into_owned();
    }

    format!("'{}'", path.replace('\'', "'\\''"))
}

fn is_unquoted_shell_path_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | '+' | '=' | ':' | ',')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn formats_single_plain_path_without_quotes() {
        assert_eq!(
            format_file_paths_for_paste([Path::new("/tmp/image.png")]),
            Some("/tmp/image.png".to_owned())
        );
    }

    #[test]
    fn shell_quotes_paths_with_spaces_and_single_quotes() {
        assert_eq!(
            format_file_paths_for_paste([Path::new("/tmp/Screen Shot's 1.png")]),
            Some("'/tmp/Screen Shot'\\''s 1.png'".to_owned())
        );
    }

    #[test]
    fn joins_multiple_paths_with_spaces() {
        assert_eq!(
            format_file_paths_for_paste([Path::new("/tmp/a.png"), Path::new("/tmp/b c.png"),]),
            Some("/tmp/a.png '/tmp/b c.png'".to_owned())
        );
    }

    #[test]
    fn returns_none_for_empty_drops() {
        assert_eq!(format_file_paths_for_paste([]), None);
    }
}
