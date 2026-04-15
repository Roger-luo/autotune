//! Interactive select prompt backed by dialoguer.

use std::io;

/// Label appended when a prompt allows falling back to free text.
pub const FREE_TEXT_SENTINEL: &str = "Type your own answer...";

/// Result of the select prompt.
pub enum SelectResult {
    /// User selected an option by index.
    Option(usize),
    /// User chose the free-text fallback.
    FreeText,
}

/// Show an interactive select menu with an optional free-text fallback.
pub fn interactive_select(items: &[String], has_free_text: bool) -> io::Result<SelectResult> {
    let _terminal_guard = autotune_agent::terminal::Guard::new();

    let mut displayed_items = items.to_vec();
    if has_free_text {
        displayed_items.push(FREE_TEXT_SENTINEL.to_string());
    }

    let selection = dialoguer::Select::new()
        .items(&displayed_items)
        .default(0)
        .interact_opt()
        .map_err(io::Error::other)?;

    match selection {
        Some(index) if has_free_text && index == items.len() => Ok(SelectResult::FreeText),
        Some(index) => Ok(SelectResult::Option(index)),
        None => Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "user cancelled",
        )),
    }
}
