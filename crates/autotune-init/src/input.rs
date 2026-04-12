use autotune_agent::protocol::QuestionOption;
use std::io::{self, IsTerminal, Write};

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
            // Interactive: arrow-key selection
            let mut items: Vec<String> = options.iter().map(|o| o.description.clone()).collect();

            if allow_free_response {
                items.push("Type your own answer...".to_string());
            }

            let selection = dialoguer::Select::new()
                .items(&items)
                .default(0)
                .interact()
                .map_err(io::Error::other)?;

            if allow_free_response && selection == options.len() {
                // User chose free text — open text input
                let input: String = dialoguer::Input::new()
                    .with_prompt("Your answer")
                    .interact_text()
                    .map_err(io::Error::other)?;
                Ok(input)
            } else {
                Ok(options[selection].key.clone())
            }
        } else {
            // Piped: numbered list, accept number or free text
            for (i, opt) in options.iter().enumerate() {
                println!("  {}) {}", i + 1, opt.description);
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
            let trimmed = input.to_lowercase();
            Ok(trimmed.is_empty() || trimmed == "yes" || trimmed == "y")
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
