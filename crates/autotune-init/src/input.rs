use autotune_agent::protocol::QuestionOption;
use std::io::{self, IsTerminal, Write};

/// Format a question option for display: "label — description" or just "label".
fn format_option(opt: &QuestionOption) -> String {
    match &opt.description {
        Some(desc) if !desc.is_empty() => format!("{} — {}", opt.label, desc),
        _ => opt.label.clone(),
    }
}

const FREE_RESPONSE_SENTINEL: &str = "Type your own answer...";

enum SelectOutcome {
    SelectedKey(String),
    PromptForText,
}

fn build_select_items(options: &[QuestionOption], allow_free_response: bool) -> Vec<String> {
    let mut items: Vec<String> = options.iter().map(format_option).collect();

    if allow_free_response {
        items.push(FREE_RESPONSE_SENTINEL.to_string());
    }

    items
}

fn resolve_select_outcome(
    selection: usize,
    options: &[QuestionOption],
    allow_free_response: bool,
) -> SelectOutcome {
    if allow_free_response && selection == options.len() {
        SelectOutcome::PromptForText
    } else {
        SelectOutcome::SelectedKey(options[selection].key.clone())
    }
}

fn parse_approval_text(input: &str) -> bool {
    let trimmed = input.trim().to_ascii_lowercase();
    trimmed.is_empty() || trimmed == "yes" || trimmed == "y"
}

/// Trait for user interaction during the init conversation.
/// Implementations handle text prompts, option selection, and approval.
pub trait UserInput {
    /// Show a message and read a free-form text response.
    fn prompt_text(&self, message: &str) -> Result<String, io::Error>;

    /// Show a question with selectable options. Returns the selected option's key
    /// or a free-text response.
    fn prompt_select(
        &self,
        question: &str,
        options: &[QuestionOption],
        allow_free_response: bool,
    ) -> Result<String, io::Error>;

    /// Show a yes/no approval prompt. Returns true if approved.
    fn prompt_approve(&self, message: &str) -> Result<bool, io::Error>;
}

/// Interactive terminal input.
/// Uses dialoguer arrow-key selection when stdin is a TTY.
/// Falls back to line-based input when stdin is piped.
pub struct TerminalInput;

impl TerminalInput {
    fn read_line() -> Result<String, io::Error> {
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        Ok(input.trim().to_string())
    }

    /// Parse user input against options. Returns the option key if matched,
    /// or the raw input as free text.
    fn match_option(input: &str, options: &[QuestionOption]) -> String {
        // Match by key (case-insensitive)
        if let Some(opt) = options.iter().find(|o| o.key.eq_ignore_ascii_case(input)) {
            return opt.key.clone();
        }
        // Match by 1-based index
        if let Ok(idx) = input.parse::<usize>()
            && idx >= 1
            && idx <= options.len()
        {
            return options[idx - 1].key.clone();
        }
        // Return as free text
        input.to_string()
    }
}

impl UserInput for TerminalInput {
    fn prompt_text(&self, message: &str) -> Result<String, io::Error> {
        println!("\n{}", message);
        print!("> ");
        io::stdout().flush()?;
        Self::read_line()
    }

    fn prompt_select(
        &self,
        question: &str,
        options: &[QuestionOption],
        allow_free_response: bool,
    ) -> Result<String, io::Error> {
        println!("\n{}", question);

        if io::stdin().is_terminal() {
            let _terminal_guard = autotune_agent::terminal::Guard::new();
            let items = build_select_items(options, allow_free_response);

            let selection = dialoguer::Select::new()
                .items(&items)
                .default(0)
                .interact()
                .map_err(io::Error::other)?;

            match resolve_select_outcome(selection, options, allow_free_response) {
                SelectOutcome::PromptForText => {
                    let text = dialoguer::Input::<String>::new()
                        .with_prompt("Type your answer")
                        .interact_text()
                        .map_err(io::Error::other)?;
                    Ok(text)
                }
                SelectOutcome::SelectedKey(key) => Ok(key),
            }
        } else {
            // Piped: numbered list, accept number or free text
            for (i, opt) in options.iter().enumerate() {
                println!("  {}) {}", i + 1, format_option(opt));
            }
            if allow_free_response {
                println!("  (or type your own answer)");
            }
            print!("> ");
            io::stdout().flush()?;
            let input = Self::read_line()?;
            Ok(Self::match_option(&input, options))
        }
    }

    fn prompt_approve(&self, message: &str) -> Result<bool, io::Error> {
        println!("\n{}", message);

        if io::stdin().is_terminal() {
            // Ensure any terminal state dialoguer leaves behind is restored
            // even if we short-circuit on error or unwind.
            let _terminal_guard = autotune_agent::terminal::Guard::new();
            let confirmed = dialoguer::Confirm::new()
                .with_prompt("Approve this config?")
                .default(true)
                .interact()
                .map_err(io::Error::other)?;
            Ok(confirmed)
        } else {
            // Piped: read yes/no from stdin
            println!("Approve this config? [Y/n]");
            print!("> ");
            io::stdout().flush()?;
            let input = Self::read_line()?;
            Ok(parse_approval_text(&input))
        }
    }
}

/// Simple input for testing. Always returns the same response.
pub struct MockInput {
    response: String,
}

impl MockInput {
    pub fn new(response: &str) -> Self {
        MockInput {
            response: response.to_string(),
        }
    }
}

impl UserInput for MockInput {
    fn prompt_text(&self, _message: &str) -> Result<String, io::Error> {
        Ok(self.response.clone())
    }

    fn prompt_select(
        &self,
        _question: &str,
        options: &[QuestionOption],
        _allow_free_response: bool,
    ) -> Result<String, io::Error> {
        if let Some(first) = options.first() {
            Ok(first.key.clone())
        } else {
            Ok(self.response.clone())
        }
    }

    fn prompt_approve(&self, _message: &str) -> Result<bool, io::Error> {
        Ok(self.response.to_lowercase() == "yes" || self.response.to_lowercase() == "y")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autotune_agent::protocol::QuestionOption;

    fn opt(key: &str, label: &str, description: Option<&str>) -> QuestionOption {
        QuestionOption {
            key: key.to_string(),
            label: label.to_string(),
            description: description.map(|s| s.to_string()),
        }
    }

    #[test]
    fn format_option_with_description() {
        let o = opt("k", "Label", Some("Desc"));
        assert_eq!(format_option(&o), "Label \u{2014} Desc");
    }

    #[test]
    fn format_option_without_description() {
        let o = opt("k", "Label", None);
        assert_eq!(format_option(&o), "Label");
    }

    #[test]
    fn format_option_with_empty_description() {
        let o = opt("k", "Label", Some(""));
        assert_eq!(format_option(&o), "Label");
    }

    #[test]
    fn build_select_items_without_free_response() {
        let items = build_select_items(&[opt("k", "Label", Some("Desc"))], false);
        assert_eq!(items, vec!["Label — Desc"]);
    }

    #[test]
    fn build_select_items_with_free_response() {
        let items = build_select_items(&[opt("k", "Label", None)], true);
        assert_eq!(items, vec!["Label", FREE_RESPONSE_SENTINEL]);
    }

    #[test]
    fn resolve_select_outcome_returns_option_key() {
        let options = vec![opt("first", "First", None), opt("second", "Second", None)];
        match resolve_select_outcome(1, &options, true) {
            SelectOutcome::SelectedKey(key) => assert_eq!(key, "second"),
            SelectOutcome::PromptForText => panic!("expected selected key"),
        }
    }

    #[test]
    fn resolve_select_outcome_returns_prompt_for_free_response() {
        let options = vec![opt("first", "First", None), opt("second", "Second", None)];
        assert!(matches!(
            resolve_select_outcome(2, &options, true),
            SelectOutcome::PromptForText
        ));
    }

    #[test]
    fn parse_approval_text_accepts_default_yes() {
        assert!(parse_approval_text(""));
    }

    #[test]
    fn parse_approval_text_accepts_explicit_yes() {
        assert!(parse_approval_text("YeS"));
        assert!(parse_approval_text(" y "));
    }

    #[test]
    fn parse_approval_text_rejects_other_values() {
        assert!(!parse_approval_text("n"));
        assert!(!parse_approval_text("no"));
        assert!(!parse_approval_text("anything else"));
    }

    #[test]
    fn match_option_by_exact_key() {
        let options = vec![opt("opt1", "Option 1", None)];
        assert_eq!(TerminalInput::match_option("opt1", &options), "opt1");
    }

    #[test]
    fn match_option_case_insensitive() {
        let options = vec![opt("opt1", "Option 1", None)];
        assert_eq!(TerminalInput::match_option("OPT1", &options), "opt1");
    }

    #[test]
    fn match_option_by_1based_index() {
        let options = vec![opt("first", "First", None), opt("second", "Second", None)];
        assert_eq!(TerminalInput::match_option("2", &options), "second");
    }

    #[test]
    fn match_option_index_out_of_range() {
        let options = vec![opt("first", "First", None), opt("second", "Second", None)];
        assert_eq!(TerminalInput::match_option("99", &options), "99");
    }

    #[test]
    fn match_option_free_text_when_no_match() {
        let options = vec![opt("opt1", "Option 1", None)];
        assert_eq!(
            TerminalInput::match_option("custom answer", &options),
            "custom answer"
        );
    }

    #[test]
    fn mock_input_prompt_text_returns_configured_response() {
        let mock = MockInput::new("hello");
        assert_eq!(mock.prompt_text("q").unwrap(), "hello");
    }

    #[test]
    fn mock_input_prompt_select_returns_first_option_key() {
        let mock = MockInput::new("x");
        let options = vec![opt("k", "L", None)];
        assert_eq!(mock.prompt_select("q", &options, false).unwrap(), "k");
    }

    #[test]
    fn mock_input_prompt_select_with_no_options_returns_response() {
        let mock = MockInput::new("custom");
        assert_eq!(mock.prompt_select("q", &[], false).unwrap(), "custom");
    }

    #[test]
    fn mock_input_prompt_approve_yes() {
        let mock = MockInput::new("yes");
        assert!(mock.prompt_approve("msg").unwrap());
    }

    #[test]
    fn mock_input_prompt_approve_y() {
        let mock = MockInput::new("y");
        assert!(mock.prompt_approve("msg").unwrap());
    }

    #[test]
    fn mock_input_prompt_approve_no() {
        let mock = MockInput::new("no");
        assert!(!mock.prompt_approve("msg").unwrap());
    }
}
