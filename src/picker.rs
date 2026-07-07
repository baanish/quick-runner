use std::io::{self, Write};

use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{self, ClearType},
};

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
            match key.code {
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

pub fn pick_live_index(options: &[String]) -> Result<Option<usize>> {
    if options.is_empty() {
        return Ok(None);
    }

    // Like the numbered picker, the live picker renders on stderr so stdout can
    // stay reserved for `--print-path` and the installed shell wrapper's `cd`.
    let _guard = RawModeGuard::enter()?;
    let mut state = LiveFilterState::new(options.to_vec());
    let per_page = 9usize;

    loop {
        render_live_filter(&state, per_page)?;
        if let Event::Key(key) = event::read()? {
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

fn render_live_filter(state: &LiveFilterState, per_page: usize) -> Result<()> {
    let mut stderr = io::stderr();
    execute!(
        stderr,
        terminal::Clear(ClearType::All),
        cursor::MoveTo(0, 0)
    )?;

    write!(stderr, "Find project: {}\r\n", state.query())?;
    if state.matches().is_empty() {
        write!(
            stderr,
            "No matches. Type to filter, Backspace to edit, Esc to cancel.\r\n"
        )?;
        stderr.flush()?;
        return Ok(());
    }

    for (offset, index) in state.visible_matches(per_page).iter().enumerate() {
        let marker = if state.visible_selected_offset(per_page) == Some(offset) {
            ">"
        } else {
            " "
        };
        write!(stderr, "{marker} {}\r\n", state.option(*index))?;
    }

    if state.matches().len() > per_page {
        write!(
            stderr,
            "Showing {per_page}/{} matches. ",
            state.matches().len()
        )?;
    }
    write!(
        stderr,
        "Type to filter. ↑/↓ move. Enter selects. Esc cancels.\r\n"
    )?;
    stderr.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
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
}
