//! Interactive select widget with an inline text input option.
//!
//! Arrow keys navigate options. Enter selects. If the last option is a
//! free-text field, Enter activates it for typing. While typing, arrow
//! up/down exits the text field and returns to option navigation.

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute, queue,
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
    let mut first_draw = true;

    terminal::enable_raw_mode()?;
    let mut out = io::stderr();
    execute!(out, cursor::Hide)?;

    let result = loop {
        // Draw the menu
        if first_draw {
            first_draw = false;
        } else {
            // Move cursor back to top of menu to redraw
            execute!(out, cursor::MoveUp(total as u16), cursor::MoveToColumn(0))?;
        }
        render(
            &mut out,
            items,
            has_free_text,
            cursor_pos,
            text_mode,
            &text_buf,
        )?;

        if let Event::Key(key) = event::read()? {
            // Ctrl+C: clean exit
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                restore(&mut out, total)?;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "user cancelled"));
            }

            if text_mode {
                match key.code {
                    KeyCode::Enter => {
                        break SelectResult::FreeText(text_buf);
                    }
                    KeyCode::Up => {
                        text_mode = false;
                        cursor_pos = cursor_pos.saturating_sub(1);
                    }
                    KeyCode::Down => {
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
                            text_mode = true;
                        } else {
                            break SelectResult::Option(cursor_pos);
                        }
                    }
                    _ => {}
                }
            }
        }
    };

    restore(&mut out, total)?;
    Ok(result)
}

/// Render the menu at the current cursor position. After this call,
/// the cursor is at the start of the line after the last menu item.
fn render(
    out: &mut impl Write,
    items: &[String],
    has_free_text: bool,
    cursor_pos: usize,
    text_mode: bool,
    text_buf: &str,
) -> io::Result<()> {
    for (i, item) in items.iter().enumerate() {
        let marker = if i == cursor_pos && !text_mode {
            ">"
        } else {
            " "
        };
        queue!(out, terminal::Clear(ClearType::CurrentLine))?;
        write!(out, "  {marker} {item}\r\n")?;
    }

    if has_free_text {
        queue!(out, terminal::Clear(ClearType::CurrentLine))?;
        if text_mode {
            write!(out, "  > {text_buf}")?;
            execute!(out, cursor::Show)?;
        } else {
            let marker = if cursor_pos == items.len() { ">" } else { " " };
            write!(out, "  {marker} Type your own answer...\r\n")?;
        }
    }

    out.flush()
}

/// Restore terminal state: clear the menu, show cursor, disable raw mode.
fn restore(out: &mut impl Write, total: usize) -> io::Result<()> {
    // Move back to top of menu
    execute!(out, cursor::MoveUp(total as u16), cursor::MoveToColumn(0))?;

    // Clear all menu lines
    for _ in 0..total {
        queue!(out, terminal::Clear(ClearType::CurrentLine))?;
        write!(out, "\r\n")?;
    }

    // Move back to top and show cursor
    execute!(
        out,
        cursor::MoveUp(total as u16),
        cursor::MoveToColumn(0),
        cursor::Show
    )?;

    terminal::disable_raw_mode()?;
    out.flush()
}
