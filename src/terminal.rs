/// Render untrusted text visibly before writing it to an interactive terminal.
///
/// Filesystem metadata, Git remote names, and AI responses can contain C0/C1
/// control bytes or Unicode format controls. Escaping them keeps those bytes
/// from moving the cursor, clearing the screen, recoloring or reordering text,
/// or otherwise changing the preview that the user is meant to inspect.
pub fn escape_untrusted(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_control() || is_unicode_format_control(ch) {
            output.extend(ch.escape_default());
        } else {
            output.push(ch);
        }
    }
    output
}

fn is_unicode_format_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{00ad}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_control_chars_and_preserves_plain_text() {
        assert_eq!(escape_untrusted("ok\u{1b}[2J\nnext"), "ok\\u{1b}[2J\\nnext");
    }

    #[test]
    fn escapes_bidi_and_format_controls() {
        assert_eq!(
            escape_untrusted("safe\u{202e}txt\u{2066}"),
            "safe\\u{202e}txt\\u{2066}"
        );
    }
}
