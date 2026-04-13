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
pub fn interactive_select(items: &[String], has_free_text: bool) -> io::Result<SelectResult> {
    let total = items.len() + if has_free_text { 1 } else { 0 };
    let mut cursor_pos: usize = 0;
    let mut text_mode = false;
    let mut text_buf = String::new();
    let mut first_draw = true;
    // Track the mode of the *previous* render for correct cursor repositioning
    let mut prev_text_mode = false;

    // RAII safety net: if we panic or `?`-early-return between here and the
    // end-of-function disable_raw_mode(), this guard still restores terminal
    // modes on drop.
    let _terminal_guard = autotune_agent::terminal::Guard::new();
    terminal::enable_raw_mode()?;
    let mut out = io::stderr();
    execute!(out, cursor::Hide)?;

    let result = loop {
        if first_draw {
            first_draw = false;
        } else {
            // Move back to the top of the menu using the *previous* render's mode
            move_to_menu_top(&mut out, total, prev_text_mode)?;
        }
        prev_text_mode = text_mode;
        render(
            &mut out,
            items,
            has_free_text,
            cursor_pos,
            text_mode,
            &text_buf,
        )?;

        if let Event::Key(key) = event::read()? {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                move_to_menu_top(&mut out, total, text_mode)?;
                clear_lines(&mut out, total)?;
                execute!(out, cursor::Show)?;
                terminal::disable_raw_mode()?;
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

    // Clean up: move to top, clear, restore
    move_to_menu_top(&mut out, total, text_mode)?;
    clear_lines(&mut out, total)?;
    execute!(out, cursor::Show)?;
    terminal::disable_raw_mode()?;
    Ok(result)
}

/// Move cursor back to the first line of the menu.
/// In text mode, the cursor is on the last line without a trailing newline,
/// so we move up (total - 1). In normal mode, we wrote total newlines,
/// so we move up total.
fn move_to_menu_top(out: &mut impl Write, total: usize, text_mode: bool) -> io::Result<()> {
    let lines_to_move = if text_mode {
        total.saturating_sub(1)
    } else {
        total
    };
    if lines_to_move > 0 {
        execute!(out, cursor::MoveUp(lines_to_move as u16))?;
    }
    write!(out, "\r")?;
    Ok(())
}

/// Clear `n` lines starting from the current position, then move back.
fn clear_lines(out: &mut impl Write, n: usize) -> io::Result<()> {
    for _ in 0..n {
        queue!(out, terminal::Clear(ClearType::CurrentLine))?;
        write!(out, "\r\n")?;
    }
    if n > 0 {
        execute!(out, cursor::MoveUp(n as u16))?;
    }
    write!(out, "\r")?;
    out.flush()
}

/// Get the terminal width, defaulting to 80 if unavailable.
fn term_width() -> usize {
    terminal::size().map(|(w, _)| w as usize).unwrap_or(80)
}

/// Truncate a string to fit within `max` columns, appending "…" if truncated.
fn truncate_to_width(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".to_string();
    }
    format!("{}…", &s[..max - 1])
}

/// Render the menu. After this, cursor is either:
/// - past the last `\r\n` (normal mode)
/// - at the end of the text input line (text mode, no trailing newline)
fn render(
    out: &mut impl Write,
    items: &[String],
    has_free_text: bool,
    cursor_pos: usize,
    text_mode: bool,
    text_buf: &str,
) -> io::Result<()> {
    let width = term_width();
    // "  > " prefix is 4 chars
    let max_item_width = width.saturating_sub(4);

    for (i, item) in items.iter().enumerate() {
        let marker = if i == cursor_pos && !text_mode {
            ">"
        } else {
            " "
        };
        let display = truncate_to_width(item, max_item_width);
        queue!(out, terminal::Clear(ClearType::CurrentLine))?;
        write!(out, "  {marker} {display}\r\n")?;
    }

    if has_free_text {
        queue!(out, terminal::Clear(ClearType::CurrentLine))?;
        if text_mode {
            // No \r\n — cursor stays at end of text for editing
            write!(out, "  > {text_buf}")?;
            execute!(out, cursor::Show)?;
        } else {
            execute!(out, cursor::Hide)?;
            let marker = if cursor_pos == items.len() { ">" } else { " " };
            write!(out, "  {marker} Type your own answer...\r\n")?;
        }
    }

    out.flush()
}
