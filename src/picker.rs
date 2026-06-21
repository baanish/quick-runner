use std::io::{self, Write};

use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode},
    execute,
    terminal::{self, ClearType},
};

/// Restores the terminal (cursor + cooked mode) on every exit path — including
/// early `?` errors and panics — so a crash mid-pick can never leave the user's
/// shell in raw mode with a hidden cursor.
struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(io::stderr(), cursor::Hide)?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stderr(), cursor::Show);
        let _ = terminal::disable_raw_mode();
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
        write!(stderr, "Use arrows to change page. Press 1-9, ESC, or q.\r\n")?;
    } else {
        write!(stderr, "Press 1-9, ESC, or q.\r\n")?;
    }
    stderr.flush()?;
    Ok(())
}
