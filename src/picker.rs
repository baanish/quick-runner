use std::io::{self, Write};

use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode},
    execute,
    terminal::{self, ClearType},
};

pub fn pick_index(options: &[String]) -> Result<Option<usize>> {
    if options.is_empty() {
        return Ok(None);
    }

    let mut stdout = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(stdout, cursor::Hide)?;

    let mut page = 0usize;
    let per_page = 9usize;

    loop {
        render_page(options, page, per_page, None)?;
        match event::read()? {
            Event::Key(key) => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                    let index = c.to_digit(10).unwrap() as usize - 1;
                    let absolute = page * per_page + index;
                    if absolute < options.len() {
                        render_page(options, page, per_page, Some(absolute))?;
                        cleanup()?;
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
            },
            _ => {}
        }
    }

    cleanup()?;
    Ok(None)
}

fn render_page(
    options: &[String],
    page: usize,
    per_page: usize,
    selected_absolute: Option<usize>,
) -> Result<()> {
    let mut stdout = io::stdout();
    let start = page * per_page;
    let end = options.len().min(start + per_page);

    execute!(
        stdout,
        terminal::Clear(ClearType::All),
        cursor::MoveTo(0, 0)
    )?;
    writeln!(stdout, "Multiple matches found:")?;
    for (display_index, value) in options[start..end].iter().enumerate() {
        let absolute = start + display_index;
        let marker = if selected_absolute == Some(absolute) {
            ">"
        } else {
            " "
        };
        writeln!(stdout, "{marker} {}) {}", display_index + 1, value)?;
    }
    if end < options.len() || page > 0 {
        writeln!(stdout, "Use arrows to change page. Press 1-9, ESC, or q.")?;
    } else {
        writeln!(stdout, "Press 1-9, ESC, or q.")?;
    }
    stdout.flush()?;
    Ok(())
}

fn cleanup() -> Result<()> {
    let mut stdout = io::stdout();
    execute!(stdout, cursor::Show)?;
    terminal::disable_raw_mode()?;
    Ok(())
}
