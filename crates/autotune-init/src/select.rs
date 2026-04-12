//! Interactive select widget with an inline text input option.
//!
//! Arrow keys navigate options. Enter selects. If the last option is a
//! free-text field, Enter activates it for typing. While typing, arrow
//! up/down exits the text field and returns to option navigation.

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{self, ClearType},
};
use std::io::{self, Write};

/// Result of the select prompt.
pub enum SelectResult {
    /// User selected an option by index.
    Option(usize),
    /// User typed a free-text response.
    FreeText(String),
}

/// Show an interactive select menu with optional inline text input.
///
/// `items` are the selectable option labels. If `has_free_text` is true,
/// an extra "Type your own answer..." row is appended at the bottom.
/// When that row is active, Enter opens an inline text editor; arrow
/// keys exit the editor and return to selection.
pub fn interactive_select(items: &[String], has_free_text: bool) -> io::Result<SelectResult> {
    let total = items.len() + if has_free_text { 1 } else { 0 };
    let mut cursor_pos: usize = 0;
    let mut text_mode = false;
    let mut text_buf = String::new();

    terminal::enable_raw_mode()?;
    let mut stdout = io::stderr();

    // Hide cursor initially
    execute!(stdout, cursor::Hide)?;

    // Draw initial state
    draw(
        &mut stdout,
        items,
        has_free_text,
        cursor_pos,
        text_mode,
        &text_buf,
    )?;

    let result = loop {
        if let Event::Key(key) = event::read()? {
            // Ctrl+C always exits
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                cleanup(&mut stdout, total)?;
                terminal::disable_raw_mode()?;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "user cancelled"));
            }

            if text_mode {
                match key.code {
                    KeyCode::Enter => {
                        break SelectResult::FreeText(text_buf);
                    }
                    KeyCode::Up => {
                        // Exit text mode, move to previous option
                        text_mode = false;
                        cursor_pos = cursor_pos.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        // Exit text mode, but we're already at the bottom
                        text_mode = false;
                    }
                    KeyCode::Backspace => {
                        text_buf.pop();
                    }
                    KeyCode::Char(c) => {
                        text_buf.push(c);
                    }
                    KeyCode::Esc => {
                        text_mode = false;
                        text_buf.clear();
                    }
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Up => {
                        cursor_pos = cursor_pos.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        if cursor_pos + 1 < total {
                            cursor_pos += 1;
                        }
                    }
                    KeyCode::Enter => {
                        if has_free_text && cursor_pos == items.len() {
                            // Activate text input mode
                            text_mode = true;
                        } else {
                            break SelectResult::Option(cursor_pos);
                        }
                    }
                    _ => {}
                }
            }

            draw(
                &mut stdout,
                items,
                has_free_text,
                cursor_pos,
                text_mode,
                &text_buf,
            )?;
        }
    };

    cleanup(&mut stdout, total)?;
    terminal::disable_raw_mode()?;

    Ok(result)
}

fn draw(
    out: &mut impl Write,
    items: &[String],
    has_free_text: bool,
    cursor_pos: usize,
    text_mode: bool,
    text_buf: &str,
) -> io::Result<()> {
    let total = items.len() + if has_free_text { 1 } else { 0 };

    // Move to start of menu and clear
    // Move up to the first line of the menu
    if total > 0 {
        execute!(out, cursor::MoveToColumn(0))?;
        // Clear from current position down
        for _ in 0..total {
            execute!(
                out,
                terminal::Clear(ClearType::CurrentLine),
                cursor::MoveDown(1)
            )?;
        }
        // Move back up
        execute!(out, cursor::MoveUp(total as u16))?;
    }

    for (i, item) in items.iter().enumerate() {
        let marker = if i == cursor_pos && !text_mode {
            ">"
        } else {
            " "
        };
        execute!(out, terminal::Clear(ClearType::CurrentLine))?;
        write!(out, "  {marker} {item}\r\n")?;
    }

    if has_free_text {
        let is_active = cursor_pos == items.len();
        execute!(out, terminal::Clear(ClearType::CurrentLine))?;
        if text_mode {
            execute!(out, cursor::Show)?;
            write!(out, "  > {text_buf}")?;
            out.flush()?;
            return Ok(());
        } else {
            execute!(out, cursor::Hide)?;
            let marker = if is_active { ">" } else { " " };
            write!(out, "  {marker} Type your own answer...\r\n")?;
        }
    }

    out.flush()
}

fn cleanup(out: &mut impl Write, total: usize) -> io::Result<()> {
    // Move to start of menu and clear all lines
    if total > 0 {
        execute!(out, cursor::MoveToColumn(0))?;
        for _ in 0..total {
            execute!(
                out,
                terminal::Clear(ClearType::CurrentLine),
                cursor::MoveDown(1)
            )?;
        }
        execute!(out, cursor::MoveUp(total as u16))?;
    }
    execute!(out, cursor::Show)?;
    out.flush()
}
