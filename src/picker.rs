use std::io::{self, Write};

use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, ClearType},
};
use unicode_width::UnicodeWidthStr;

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_CYAN: &str = "\x1b[36m";

/// Restores the terminal (cursor + cooked mode) on every exit path — including
/// early `?` errors and panics — so a crash mid-pick can never leave the user's
/// shell in raw mode with a hidden cursor.
struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> Result<Self> {
        // Construct the guard first so its Drop restores the terminal even if a
        // step below fails partway (e.g. enable_raw_mode succeeds but Hide errors).
        // disable_raw_mode / Show are harmless no-ops if their step never ran.
        let guard = Self;
        terminal::enable_raw_mode()?;
        execute!(io::stderr(), cursor::Hide)?;
        Ok(guard)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stderr(), cursor::Show);
        let _ = terminal::disable_raw_mode();
    }
}

#[derive(Debug)]
struct LiveFilterState {
    options: Vec<String>,
    query: String,
    matches: Vec<usize>,
    selected_offset: Option<usize>,
}

impl LiveFilterState {
    fn new(options: Vec<String>) -> Self {
        let mut state = Self {
            options,
            query: String::new(),
            matches: Vec::new(),
            selected_offset: None,
        };
        state.refresh_matches();
        state
    }

    fn query(&self) -> &str {
        &self.query
    }

    fn matches(&self) -> &[usize] {
        &self.matches
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected_offset.map(|offset| self.matches[offset])
    }

    fn insert_char(&mut self, value: char) {
        self.query.push(value);
        self.refresh_matches();
    }

    fn backspace(&mut self) {
        self.query.pop();
        self.refresh_matches();
    }

    fn move_down(&mut self) {
        if let Some(offset) = self.selected_offset {
            if offset + 1 < self.matches.len() {
                self.selected_offset = Some(offset + 1);
            }
        }
    }

    fn move_up(&mut self) {
        if let Some(offset) = self.selected_offset {
            if offset > 0 {
                self.selected_offset = Some(offset - 1);
            }
        }
    }

    fn visible_matches(&self, per_page: usize) -> &[usize] {
        if per_page == 0 || self.matches.is_empty() {
            return &[];
        }
        let start = self.visible_start(per_page);
        let end = self.matches.len().min(start + per_page);
        &self.matches[start..end]
    }

    fn visible_selected_offset(&self, per_page: usize) -> Option<usize> {
        let selected = self.selected_offset?;
        Some(selected - self.visible_start(per_page))
    }

    fn visible_start(&self, per_page: usize) -> usize {
        let Some(selected) = self.selected_offset else {
            return 0;
        };
        if per_page == 0 || selected < per_page {
            0
        } else {
            selected + 1 - per_page
        }
    }

    fn option(&self, index: usize) -> &str {
        &self.options[index]
    }

    fn refresh_matches(&mut self) {
        let query = self.query.to_ascii_lowercase();
        self.matches = self
            .options
            .iter()
            .enumerate()
            .filter_map(|(index, option)| {
                if query.is_empty() || option.to_ascii_lowercase().contains(&query) {
                    Some(index)
                } else {
                    None
                }
            })
            .collect();
        self.selected_offset = if self.matches.is_empty() {
            None
        } else {
            Some(0)
        };
    }
}

pub fn pick_index(options: &[String]) -> Result<Option<usize>> {
    if options.is_empty() {
        return Ok(None);
    }

    // The menu is drawn on stderr, not stdout, so the picker works under the
    // installed shell wrapper, which captures stdout (`dir=$(qr go … --print-path)`)
    // while leaving stderr on the terminal. The guard restores the terminal on
    // every return path.
    let _guard = RawModeGuard::enter()?;

    let mut page = 0usize;
    let per_page = 9usize;

    loop {
        render_page(options, page, per_page, None)?;
        if let Event::Key(key) = event::read()? {
            let Some(code) = numbered_picker_key_code(key) else {
                continue;
            };
            match code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
                KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                    let index = c.to_digit(10).unwrap() as usize - 1;
                    let absolute = page * per_page + index;
                    if absolute < options.len() {
                        render_page(options, page, per_page, Some(absolute))?;
                        return Ok(Some(absolute));
                    }
                }
                KeyCode::Right | KeyCode::Down if (page + 1) * per_page < options.len() => {
                    page += 1;
                }
                KeyCode::Left | KeyCode::Up if page > 0 => {
                    page -= 1;
                }
                _ => {}
            }
        }
    }
}

fn numbered_picker_key_code(key: crossterm::event::KeyEvent) -> Option<KeyCode> {
    should_handle_key_event(key.kind).then_some(key.code)
}

fn should_handle_key_event(kind: KeyEventKind) -> bool {
    kind != KeyEventKind::Release
}

pub fn pick_live_index(options: &[String]) -> Result<Option<usize>> {
    if options.is_empty() {
        return Ok(None);
    }

    // Like the numbered picker, the live picker renders on stderr so stdout can
    // stay reserved for `--print-path` and the installed shell wrapper's `cd`.
    let _guard = RawModeGuard::enter()?;
    let mut state = LiveFilterState::new(options.to_vec());
    let per_page = 7usize;
    let mut rendered_lines = 0usize;

    let loop_result: Result<Option<usize>> = (|| {
        loop {
            rendered_lines = render_live_filter(&state, per_page, rendered_lines)?;
            if let Event::Key(key) = event::read()? {
                if !should_handle_key_event(key.kind) {
                    continue;
                }
                match key.code {
                    KeyCode::Enter => return Ok(state.selected_index()),
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(None);
                    }
                    KeyCode::Char(value) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        state.insert_char(value);
                    }
                    KeyCode::Backspace => state.backspace(),
                    KeyCode::Down => state.move_down(),
                    KeyCode::Up => state.move_up(),
                    _ => {}
                }
            }
        }
    })();

    complete_live_filter(loop_result, || clear_live_filter(rendered_lines))
}

fn complete_live_filter<T>(result: Result<T>, cleanup: impl FnOnce() -> Result<()>) -> Result<T> {
    match result {
        Ok(value) => {
            cleanup()?;
            Ok(value)
        }
        Err(error) => {
            let _ = cleanup();
            Err(error)
        }
    }
}

fn render_page(
    options: &[String],
    page: usize,
    per_page: usize,
    selected_absolute: Option<usize>,
) -> Result<()> {
    let mut stderr = io::stderr();
    let start = page * per_page;
    let end = options.len().min(start + per_page);

    execute!(
        stderr,
        terminal::Clear(ClearType::All),
        cursor::MoveTo(0, 0)
    )?;
    write!(stderr, "Multiple matches found:\r\n")?;
    for (display_index, value) in options[start..end].iter().enumerate() {
        let absolute = start + display_index;
        let marker = if selected_absolute == Some(absolute) {
            ">"
        } else {
            " "
        };
        write!(stderr, "{marker} {}) {}\r\n", display_index + 1, value)?;
    }
    if end < options.len() || page > 0 {
        write!(
            stderr,
            "Use arrows to change page. Press 1-9, ESC, or q.\r\n"
        )?;
    } else {
        write!(stderr, "Press 1-9, ESC, or q.\r\n")?;
    }
    stderr.flush()?;
    Ok(())
}

fn render_live_filter(
    state: &LiveFilterState,
    per_page: usize,
    previous_lines: usize,
) -> Result<usize> {
    let mut stderr = io::stderr();
    if previous_lines > 0 {
        execute!(
            stderr,
            cursor::MoveUp(previous_lines as u16),
            terminal::Clear(ClearType::FromCursorDown)
        )?;
    }

    let lines = live_filter_lines(state, per_page, terminal_width());
    for line in &lines {
        write!(stderr, "{line}\r\n")?;
    }
    stderr.flush()?;
    Ok(lines.len())
}

fn clear_live_filter(previous_lines: usize) -> Result<()> {
    if previous_lines == 0 {
        return Ok(());
    }

    let mut stderr = io::stderr();
    execute!(
        stderr,
        cursor::MoveUp(previous_lines as u16),
        terminal::Clear(ClearType::FromCursorDown)
    )?;
    stderr.flush()?;
    Ok(())
}

fn live_filter_lines(state: &LiveFilterState, per_page: usize, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let query = if state.query().is_empty() {
        format!("{ANSI_DIM}type…{ANSI_RESET}")
    } else {
        format!("{ANSI_BOLD}{}{ANSI_RESET}", state.query())
    };
    lines.push(truncate_ansi(
        &format!("{ANSI_CYAN}find{ANSI_RESET} project {query}"),
        width,
    ));

    if state.matches().is_empty() {
        lines.push(truncate_ansi(
            &format!("  {ANSI_DIM}no matches · backspace to widen · esc cancels{ANSI_RESET}"),
            width,
        ));
        return lines;
    }

    for (offset, index) in state.visible_matches(per_page).iter().enumerate() {
        let selected = state.visible_selected_offset(per_page) == Some(offset);
        let marker = if selected {
            format!("{ANSI_CYAN}❯{ANSI_RESET}")
        } else {
            " ".to_string()
        };
        let (name, path) = split_live_label(state.option(*index));
        lines.push(truncate_ansi(
            &format!("{marker} {ANSI_BOLD}{name}{ANSI_RESET} {ANSI_DIM}{path}{ANSI_RESET}"),
            width,
        ));
    }

    let visible = state.visible_matches(per_page).len();
    let total = state.matches().len();
    let count = if total > visible {
        format!("{visible}/{total} matches")
    } else if total == 1 {
        "1 match".to_string()
    } else {
        format!("{total} matches")
    };
    lines.push(truncate_ansi(
        &format!("  {ANSI_DIM}{count} · type to filter · ↑/↓ move · enter selects · esc cancels{ANSI_RESET}"),
        width,
    ));
    lines
}

fn split_live_label(label: &str) -> (&str, &str) {
    if let Some((name, path)) = label.split_once('\t') {
        return (name, path);
    }
    if let Some((name, path)) = label.rsplit_once(" (") {
        if let Some(path) = path.strip_suffix(')') {
            return (name, path);
        }
    }
    (label, "")
}

fn terminal_width() -> usize {
    terminal::size()
        .map(|(width, _)| normalized_terminal_width(width))
        .unwrap_or(100)
}

fn normalized_terminal_width(width: u16) -> usize {
    usize::from(width).max(1)
}

fn truncate_ansi(input: &str, max_width: usize) -> String {
    if max_width == 0 || visible_width(input) <= max_width {
        return input.to_string();
    }

    let target = max_width.saturating_sub(1);
    let mut output = String::new();
    let mut visible_text = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            output.push(ch);
            for next in chars.by_ref() {
                output.push(next);
                if next == 'm' {
                    break;
                }
            }
            continue;
        }

        let mut candidate = visible_text.clone();
        candidate.push(ch);
        if candidate.width() > target {
            break;
        }
        output.push(ch);
        visible_text = candidate;
    }

    output.push('…');
    output.push_str(ANSI_RESET);
    output
}

fn visible_width(input: &str) -> usize {
    let mut visible = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
        } else {
            visible.push(ch);
        }
    }
    visible.width()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn strip_ansi_codes(input: &str) -> String {
        let mut output = String::new();
        let mut chars = input.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                for next in chars.by_ref() {
                    if next == 'm' {
                        break;
                    }
                }
            } else {
                output.push(ch);
            }
        }
        output
    }

    #[test]
    fn live_filter_starts_with_every_option_available() {
        let state = LiveFilterState::new(labels(&["quick-runner", "orion-api", "docs"]));

        assert_eq!(state.matches(), &[0, 1, 2]);
        assert_eq!(state.selected_index(), Some(0));
    }

    #[test]
    fn live_filter_keeps_labels_containing_the_typed_query() {
        let mut state = LiveFilterState::new(labels(&["quick-runner", "orion-api", "docs"]));

        state.insert_char('i');
        state.insert_char('o');

        assert_eq!(state.matches(), &[1]);
        assert_eq!(state.selected_index(), Some(1));
    }

    #[test]
    fn live_filter_clamps_selection_to_first_remaining_match() {
        let mut state = LiveFilterState::new(labels(&["alpha", "beta", "gamma", "alphabet"]));
        state.move_down();
        state.move_down();

        state.insert_char('a');
        state.insert_char('l');
        state.insert_char('p');

        assert_eq!(state.matches(), &[0, 3]);
        assert_eq!(state.selected_index(), Some(0));
    }

    #[test]
    fn live_filter_backspace_restores_broader_matches() {
        let mut state = LiveFilterState::new(labels(&["alpha", "alpine", "beta", "alloy"]));
        state.insert_char('a');
        state.insert_char('l');
        state.insert_char('p');
        assert_eq!(state.matches(), &[0, 1]);

        state.backspace();

        assert_eq!(state.matches(), &[0, 1, 3]);
        assert_eq!(state.selected_index(), Some(0));
    }

    #[test]
    fn live_filter_visible_matches_follow_selection() {
        let mut state = LiveFilterState::new(labels(&[
            "project-00",
            "project-01",
            "project-02",
            "project-03",
            "project-04",
            "project-05",
            "project-06",
            "project-07",
            "project-08",
            "project-09",
            "project-10",
        ]));
        for _ in 0..10 {
            state.move_down();
        }

        assert_eq!(state.selected_index(), Some(10));
        assert_eq!(state.visible_matches(9), &[2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[test]
    fn live_picker_ignores_key_release_events() {
        assert!(should_handle_key_event(KeyEventKind::Press));
        assert!(should_handle_key_event(KeyEventKind::Repeat));
        assert!(!should_handle_key_event(KeyEventKind::Release));
    }

    #[test]
    fn numbered_picker_ignores_key_release_events() {
        let release = crossterm::event::KeyEvent::new_with_kind(
            KeyCode::Char('1'),
            KeyModifiers::empty(),
            KeyEventKind::Release,
        );
        let press = crossterm::event::KeyEvent::new_with_kind(
            KeyCode::Char('1'),
            KeyModifiers::empty(),
            KeyEventKind::Press,
        );

        assert_eq!(numbered_picker_key_code(release), None);
        assert_eq!(numbered_picker_key_code(press), Some(KeyCode::Char('1')));
    }

    #[test]
    fn live_filter_render_is_compact_and_styled() {
        let state = LiveFilterState::new(labels(&[
            "quick-runner\t/Users/aanishbhirud/Development/quick-runner",
            "orion-api\t/Users/aanishbhirud/Development/orion-api",
        ]));

        let lines = live_filter_lines(&state, 7, 72);

        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("\u{1b}[36mfind\u{1b}[0m"));
        assert!(lines[1].contains("❯"));
        assert!(lines[1].contains("\u{1b}[1mquick-runner\u{1b}[0m"));
        assert!(
            lines[1].contains("\u{1b}[2m/Users/aanishbhirud/Development/quick-runner\u{1b}[0m")
        );
        assert!(lines[3].contains("type to filter"));
    }

    #[test]
    fn live_filter_render_truncates_long_rows_to_terminal_width() {
        let state = LiveFilterState::new(labels(&[
            "very-long-project-name\t/Users/aanishbhirud/Development/some/deep/path/that/would/wrap",
        ]));

        let lines = live_filter_lines(&state, 7, 48);

        assert!(visible_width(&lines[1]) <= 48);
        assert!(lines[1].contains('…'));
    }

    #[test]
    fn terminal_width_is_not_inflated_on_narrow_terminals() {
        assert_eq!(normalized_terminal_width(20), 20);
    }

    #[test]
    fn live_filter_cleanup_runs_when_the_picker_loop_errors() {
        let cleaned = std::cell::Cell::new(false);
        let loop_result: Result<()> = Err(anyhow::anyhow!("read failed"));

        let result = complete_live_filter(loop_result, || {
            cleaned.set(true);
            Ok(())
        });

        assert!(cleaned.get());
        assert_eq!(result.unwrap_err().to_string(), "read failed");
    }

    #[test]
    fn live_filter_truncation_counts_unicode_terminal_cells() {
        let output = truncate_ansi("界界界", 4);
        let visible = strip_ansi_codes(&output);

        assert!(unicode_width::UnicodeWidthStr::width(visible.as_str()) <= 4);
        assert!(output.contains('…'));
    }

    #[test]
    fn live_filter_truncation_counts_emoji_presentation_sequences() {
        let output = truncate_ansi("#\u{fe0f}#\u{fe0f}#\u{fe0f}", 4);
        let visible = strip_ansi_codes(&output);

        assert!(unicode_width::UnicodeWidthStr::width(visible.as_str()) <= 4);
        assert!(output.contains('…'));
    }
}
