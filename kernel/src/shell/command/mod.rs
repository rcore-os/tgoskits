mod base;
mod history;
mod vm;

pub use base::*;
pub use history::*;
pub use vm::*;

use std::io::prelude::*;
use std::string::String;
use std::vec::Vec;
use std::{collections::BTreeMap, string::ToString};
use std::{print, println};

lazy_static::lazy_static! {
    pub static ref COMMAND_TREE: BTreeMap<String, CommandNode> = build_command_tree();
}

#[derive(Debug, Clone)]
pub struct CommandNode {
    handler: Option<fn(&ParsedCommand)>,
    subcommands: BTreeMap<String, CommandNode>,
    description: &'static str,
    usage: Option<&'static str>,
    #[allow(dead_code)]
    log_level: log::LevelFilter,
    options: Vec<OptionDef>,
    flags: Vec<FlagDef>,
}

#[derive(Debug, Clone)]
pub struct OptionDef {
    name: &'static str,
    short: Option<char>,
    long: Option<&'static str>,
    description: &'static str,
    required: bool,
}

#[derive(Debug, Clone)]
pub struct FlagDef {
    name: &'static str,
    short: Option<char>,
    long: Option<&'static str>,
    description: &'static str,
}

#[derive(Debug, Clone)]
pub struct ParsedCommand {
    pub command_path: Vec<String>,
    pub options: BTreeMap<String, String>,
    pub flags: BTreeMap<String, bool>,
    pub positional_args: Vec<String>,
}

#[derive(Debug)]
pub enum ParseError {
    UnknownCommand(String),
    UnknownOption(String),
    MissingValue(String),
    MissingRequiredOption(String),
    NoHandler(String),
}

impl CommandNode {
    pub fn new(description: &'static str) -> Self {
        Self {
            handler: None,
            subcommands: BTreeMap::new(),
            description,
            usage: None,
            log_level: log::LevelFilter::Off,
            options: Vec::new(),
            flags: Vec::new(),
        }
    }

    pub fn with_handler(mut self, handler: fn(&ParsedCommand)) -> Self {
        self.handler = Some(handler);
        self
    }

    pub fn with_usage(mut self, usage: &'static str) -> Self {
        self.usage = Some(usage);
        self
    }

    #[allow(dead_code)]
    pub fn with_log_level(mut self, level: log::LevelFilter) -> Self {
        self.log_level = level;
        self
    }

    pub fn with_option(mut self, option: OptionDef) -> Self {
        self.options.push(option);
        self
    }

    pub fn with_flag(mut self, flag: FlagDef) -> Self {
        self.flags.push(flag);
        self
    }

    pub fn add_subcommand<S: Into<String>>(mut self, name: S, node: CommandNode) -> Self {
        self.subcommands.insert(name.into(), node);
        self
    }
}

impl OptionDef {
    pub fn new(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            short: None,
            long: None,
            description,
            required: false,
        }
    }

    #[allow(dead_code)]
    pub fn with_short(mut self, short: char) -> Self {
        self.short = Some(short);
        self
    }

    pub fn with_long(mut self, long: &'static str) -> Self {
        self.long = Some(long);
        self
    }

    #[allow(dead_code)]
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
}

impl FlagDef {
    pub fn new(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            short: None,
            long: None,
            description,
        }
    }

    pub fn with_short(mut self, short: char) -> Self {
        self.short = Some(short);
        self
    }

    pub fn with_long(mut self, long: &'static str) -> Self {
        self.long = Some(long);
        self
    }
}

// Command Parser
pub struct CommandParser;

impl CommandParser {
    pub fn parse(input: &str) -> Result<ParsedCommand, ParseError> {
        let tokens = Self::tokenize(input);
        if tokens.is_empty() {
            return Err(ParseError::UnknownCommand("empty command".to_string()));
        }

        // Find the command path
        let (command_path, command_node, remaining_tokens) = Self::find_command(&tokens)?;

        // Parse the arguments
        let (options, flags, positional_args) = Self::parse_args(remaining_tokens, command_node)?;

        // Validate required options
        Self::validate_required_options(command_node, &options)?;

        Ok(ParsedCommand {
            command_path,
            options,
            flags,
            positional_args,
        })
    }

    fn tokenize(input: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut current_token = String::new();
        let mut in_quotes = false;
        let mut escape_next = false;

        for ch in input.chars() {
            if escape_next {
                current_token.push(ch);
                escape_next = false;
            } else if ch == '\\' {
                escape_next = true;
            } else if ch == '"' {
                in_quotes = !in_quotes;
            } else if ch.is_whitespace() && !in_quotes {
                if !current_token.is_empty() {
                    tokens.push(current_token.clone());
                    current_token.clear();
                }
            } else {
                current_token.push(ch);
            }
        }

        if !current_token.is_empty() {
            tokens.push(current_token);
        }

        tokens
    }

    fn find_command(
        tokens: &[String],
    ) -> Result<(Vec<String>, &CommandNode, &[String]), ParseError> {
        let mut current_node = COMMAND_TREE
            .get(&tokens[0])
            .ok_or_else(|| ParseError::UnknownCommand(tokens[0].clone()))?;

        let mut command_path = vec![tokens[0].clone()];
        let mut token_index = 1;

        // Traverse to find the deepest command node
        while token_index < tokens.len() {
            if let Some(subcommand) = current_node.subcommands.get(&tokens[token_index]) {
                current_node = subcommand;
                command_path.push(tokens[token_index].clone());
                token_index += 1;
            } else {
                break;
            }
        }

        Ok((command_path, current_node, &tokens[token_index..]))
    }

    #[allow(clippy::type_complexity)]
    fn parse_args(
        tokens: &[String],
        command_node: &CommandNode,
    ) -> Result<
        (
            BTreeMap<String, String>,
            BTreeMap<String, bool>,
            Vec<String>,
        ),
        ParseError,
    > {
        let mut options = BTreeMap::new();
        let mut flags = BTreeMap::new();
        let mut positional_args = Vec::new();
        let mut i = 0;

        while i < tokens.len() {
            let token = &tokens[i];

            if let Some(name) = token.strip_prefix("--") {
                // Long options/flags
                if let Some(eq_pos) = name.find('=') {
                    // --option=value format
                    let (opt_name, value) = name.split_at(eq_pos);
                    let value = &value[1..]; // Skip '='
                    if Self::is_option(opt_name, command_node) {
                        options.insert(opt_name.to_string(), value.to_string());
                    } else {
                        return Err(ParseError::UnknownOption(format!("--{opt_name}")));
                    }
                } else if Self::is_flag(name, command_node) {
                    flags.insert(name.to_string(), true);
                } else if Self::is_option(name, command_node) {
                    // --option value format
                    if i + 1 >= tokens.len() {
                        return Err(ParseError::MissingValue(format!("--{name}")));
                    }
                    options.insert(name.to_string(), tokens[i + 1].clone());
                    i += 1; // Skip value
                } else {
                    return Err(ParseError::UnknownOption(format!("--{name}")));
                }
            } else if token.starts_with('-') && token.len() > 1 {
                // Short options/flags
                let chars: Vec<char> = token[1..].chars().collect();
                for (j, &ch) in chars.iter().enumerate() {
                    if Self::is_short_flag(ch, command_node) {
                        flags.insert(
                            Self::get_flag_name_by_short(ch, command_node)
                                .unwrap()
                                .to_string(),
                            true,
                        );
                    } else if Self::is_short_option(ch, command_node) {
                        let opt_name = Self::get_option_name_by_short(ch, command_node).unwrap();
                        if j == chars.len() - 1 && i + 1 < tokens.len() {
                            // Last character and there is a next token as value
                            options.insert(opt_name.to_string(), tokens[i + 1].clone());
                            i += 1; // Skip value
                        } else {
                            return Err(ParseError::MissingValue(format!("-{ch}")));
                        }
                    } else {
                        return Err(ParseError::UnknownOption(format!("-{ch}")));
                    }
                }
            } else {
                // Positional arguments
                positional_args.push(token.clone());
            }
            i += 1;
        }

        Ok((options, flags, positional_args))
    }

    fn is_option(name: &str, node: &CommandNode) -> bool {
        node.options
            .iter()
            .any(|opt| (opt.long == Some(name)) || opt.name == name)
    }

    fn is_flag(name: &str, node: &CommandNode) -> bool {
        node.flags
            .iter()
            .any(|flag| (flag.long == Some(name)) || flag.name == name)
    }

    fn is_short_option(ch: char, node: &CommandNode) -> bool {
        node.options.iter().any(|opt| opt.short == Some(ch))
    }

    fn is_short_flag(ch: char, node: &CommandNode) -> bool {
        node.flags.iter().any(|flag| flag.short == Some(ch))
    }

    fn get_option_name_by_short(ch: char, node: &CommandNode) -> Option<&str> {
        node.options
            .iter()
            .find(|opt| opt.short == Some(ch))
            .map(|opt| opt.name)
    }

    fn get_flag_name_by_short(ch: char, node: &CommandNode) -> Option<&str> {
        node.flags
            .iter()
            .find(|flag| flag.short == Some(ch))
            .map(|flag| flag.name)
    }

    fn validate_required_options(
        node: &CommandNode,
        options: &BTreeMap<String, String>,
    ) -> Result<(), ParseError> {
        for option in &node.options {
            if option.required && !options.contains_key(option.name) {
                return Err(ParseError::MissingRequiredOption(option.name.to_string()));
            }
        }
        Ok(())
    }
}

// Command execution function
pub fn execute_command(input: &str) -> Result<(), ParseError> {
    let parsed = CommandParser::parse(input)?;

    // Find the corresponding command node
    let mut current_node = COMMAND_TREE.get(&parsed.command_path[0]).unwrap();
    for cmd in &parsed.command_path[1..] {
        current_node = current_node.subcommands.get(cmd).unwrap();
    }

    // Execute the command
    if let Some(handler) = current_node.handler {
        handler(&parsed);
        Ok(())
    } else {
        Err(ParseError::NoHandler(parsed.command_path.join(" ")))
    }
}

// Build command tree
fn build_command_tree() -> BTreeMap<String, CommandNode> {
    let mut tree = BTreeMap::new();

    build_base_cmd(&mut tree);
    build_vm_cmd(&mut tree);

    tree
}

// Helper function: Display command help
pub fn show_help(command_path: &[String]) -> Result<(), ParseError> {
    let mut current_node = COMMAND_TREE
        .get(&command_path[0])
        .ok_or_else(|| ParseError::UnknownCommand(command_path[0].clone()))?;

    for cmd in &command_path[1..] {
        current_node = current_node
            .subcommands
            .get(cmd)
            .ok_or_else(|| ParseError::UnknownCommand(cmd.clone()))?;
    }

    println!("Command: {}", command_path.join(" "));
    println!("Description: {}", current_node.description);

    if let Some(usage) = current_node.usage {
        println!("Usage: {}", usage);
    }

    if !current_node.options.is_empty() {
        println!("\nOptions:");
        for option in &current_node.options {
            let mut opt_str = String::new();
            if let Some(short) = option.short {
                opt_str.push_str(&format!("-{short}"));
            }
            if let Some(long) = option.long {
                if !opt_str.is_empty() {
                    opt_str.push_str(", ");
                }
                opt_str.push_str(&format!("--{long}"));
            }
            if opt_str.is_empty() {
                opt_str = option.name.to_string();
            }

            let required_str = if option.required { " (required)" } else { "" };
            println!("  {:<20} {}{}", opt_str, option.description, required_str);
        }
    }

    if !current_node.flags.is_empty() {
        println!("\nFlags:");
        for flag in &current_node.flags {
            let mut flag_str = String::new();
            if let Some(short) = flag.short {
                flag_str.push_str(&format!("-{short}"));
            }
            if let Some(long) = flag.long {
                if !flag_str.is_empty() {
                    flag_str.push_str(", ");
                }
                flag_str.push_str(&format!("--{long}"));
            }
            if flag_str.is_empty() {
                flag_str = flag.name.to_string();
            }

            println!("  {:<20} {}", flag_str, flag.description);
        }
    }

    if !current_node.subcommands.is_empty() {
        println!("\nSubcommands:");
        for (name, node) in &current_node.subcommands {
            println!("  {:<20} {}", name, node.description);
        }
    }

    Ok(())
}

pub fn print_prompt() {
    #[cfg(feature = "fs")]
    print!("axvisor:{}$ ", std::env::current_dir().unwrap());
    #[cfg(not(feature = "fs"))]
    print!("axvisor:$ ");
    std::io::stdout().flush().unwrap();
}

pub fn run_cmd_bytes(cmd_bytes: &[u8]) {
    match str::from_utf8(cmd_bytes) {
        Ok(cmd_str) => {
            let trimmed = cmd_str.trim();
            if trimmed.is_empty() {
                return;
            }

            match execute_command(trimmed) {
                Ok(_) => {
                    // Command executed successfully
                }
                Err(ParseError::UnknownCommand(cmd)) => {
                    println!("Error: Unknown command '{}'", cmd);
                    println!("Type 'help' to see available commands");
                }
                Err(ParseError::UnknownOption(opt)) => {
                    println!("Error: Unknown option '{}'", opt);
                }
                Err(ParseError::MissingValue(opt)) => {
                    println!("Error: Option '{}' is missing a value", opt);
                }
                Err(ParseError::MissingRequiredOption(opt)) => {
                    println!("Error: Missing required option '{}'", opt);
                }
                Err(ParseError::NoHandler(cmd)) => {
                    println!("Error: Command '{}' has no handler function", cmd);
                }
            }
        }
        Err(_) => {
            println!("Error: Input contains invalid UTF-8 characters");
        }
    }
}

// Built-in command handler
pub fn handle_builtin_commands(input: &str) -> bool {
    match input.trim() {
        "help" => {
            show_available_commands();
            true
        }
        "exit" | "quit" => {
            println!("Goodbye!");
            std::process::exit(0);
        }
        "clear" => {
            print!("\x1b[2J\x1b[H"); // ANSI clear screen sequence
            std::io::stdout().flush().unwrap();
            true
        }
        _ if input.starts_with("help ") => {
            let cmd_parts: Vec<String> = input[5..]
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            if let Err(e) = show_help(&cmd_parts) {
                println!("Error: {:?}", e);
            }
            true
        }
        _ => false,
    }
}

pub fn show_available_commands() {
    println!("ArceOS Shell - Available Commands:");
    println!();

    // Display all top-level commands
    for (name, node) in COMMAND_TREE.iter() {
        println!("  {:<15} {}", name, node.description);

        // Display subcommands
        if !node.subcommands.is_empty() {
            for (sub_name, sub_node) in &node.subcommands {
                println!("    {:<13} {}", sub_name, sub_node.description);
            }
        }
    }

    println!();
    println!("Built-in Commands:");
    println!("  help            Show help information");
    println!("  help <command>  Show help for a specific command");
    println!("  clear           Clear the screen");
    println!("  exit/quit       Exit the shell");
    println!();
    println!("Tip: Use 'help <command>' to see detailed usage of a command");
}
