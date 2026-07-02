/// Render untrusted text visibly before writing it to an interactive terminal.
///
/// Filesystem metadata, Git remote names, and AI responses can contain C0/C1
/// control bytes. Escaping controls keeps those bytes from moving the cursor,
/// clearing the screen, recoloring text, or otherwise changing the preview that
/// the user is meant to inspect.
pub fn escape_untrusted(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_control() {
            output.extend(ch.escape_default());
        } else {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_control_chars_and_preserves_plain_text() {
        assert_eq!(escape_untrusted("ok\u{1b}[2J\nnext"), "ok\\u{1b}[2J\\nnext");
    }
}
