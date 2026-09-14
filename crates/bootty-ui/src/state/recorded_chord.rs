/// Lowercase a single-letter key in a recorded chord (the physical-key serializer emits uppercase
/// letters, but bootty's default keybinds and the UI recorder use lowercase, e.g. `cmd+x`).
/// Multi-character key names like `Tab`/`F5` and non-letters are left untouched.
pub(super) fn normalize_recorded_chord(chord: String) -> String {
    match chord.rsplit_once('+') {
        Some((mods, key)) => normalize_recorded_key(key)
            .map(|key| format!("{mods}+{key}"))
            .unwrap_or(chord),
        None => normalize_recorded_key(&chord).unwrap_or(chord),
    }
}

fn normalize_recorded_key(key: &str) -> Option<String> {
    if is_single_ascii_letter(key) {
        return Some(key.to_ascii_lowercase());
    }
    if let Some(letter) = key.strip_prefix("Key")
        && is_single_ascii_letter(letter)
    {
        return Some(letter.to_ascii_lowercase());
    }
    if let Some(digit) = key.strip_prefix("Digit")
        && digit.len() == 1
        && digit.as_bytes().first().is_some_and(u8::is_ascii_digit)
    {
        return Some(digit.to_owned());
    }
    None
}

fn is_single_ascii_letter(value: &str) -> bool {
    let mut chars = value.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_alphabetic())
}
