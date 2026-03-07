use std::io::IsTerminal;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use dialoguer::console::{Style, style};
use dialoguer::theme::ColorfulTheme;

const HEADER_LINE: &str = "────────────────────────────────────────";
const MIN_CONTENT_WIDTH: usize = 24;

pub(crate) fn cli_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
        .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
        .literal(AnsiColor::Blue.on_default().effects(Effects::BOLD))
        .placeholder(AnsiColor::Yellow.on_default())
        .valid(AnsiColor::Green.on_default())
        .invalid(AnsiColor::Red.on_default().effects(Effects::BOLD))
        .context(AnsiColor::White.on_default().dimmed())
}

pub(crate) fn cli_prompt_theme() -> ColorfulTheme {
    ColorfulTheme {
        defaults_style: Style::new().for_stderr().cyan().bold(),
        prompt_style: Style::new().for_stderr().bold().white(),
        prompt_prefix: style("◆".to_string()).for_stderr().cyan().bold(),
        prompt_suffix: style("›".to_string()).for_stderr().cyan(),
        success_prefix: style("●".to_string()).for_stderr().green().bold(),
        success_suffix: style("·".to_string()).for_stderr().black().bright(),
        error_prefix: style("✕".to_string()).for_stderr().red().bold(),
        error_style: Style::new().for_stderr().red().bold(),
        hint_style: Style::new().for_stderr().black().bright(),
        values_style: Style::new().for_stderr().green().bold(),
        active_item_style: Style::new().for_stderr().cyan().bold(),
        inactive_item_style: Style::new().for_stderr().white(),
        active_item_prefix: style("›".to_string()).for_stderr().cyan().bold(),
        inactive_item_prefix: style(" ".to_string()).for_stderr(),
        checked_item_prefix: style("◉".to_string()).for_stderr().green().bold(),
        unchecked_item_prefix: style("○".to_string()).for_stderr().black().bright(),
        picked_item_prefix: style("›".to_string()).for_stderr().cyan().bold(),
        unpicked_item_prefix: style(" ".to_string()).for_stderr(),
    }
}

pub(crate) fn print_heading(title: &str) {
    println!("{}", style(title).bold().cyan());
    let width = terminal_width().min(HEADER_LINE.chars().count()).max(24);
    println!(
        "{}",
        style(HEADER_LINE.chars().take(width).collect::<String>()).dim()
    );
}

pub(crate) fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|w| *w >= 40)
        .unwrap_or_else(|| console::Term::stdout().size().1 as usize)
        .max(40)
}

pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                if word.chars().count() <= width {
                    current.push_str(word);
                } else {
                    let mut chunk = String::new();
                    for ch in word.chars() {
                        chunk.push(ch);
                        if chunk.chars().count() >= width {
                            lines.push(chunk);
                            chunk = String::new();
                        }
                    }
                    current = chunk;
                }
            } else if current.chars().count() + 1 + word.chars().count() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(current);
                if word.chars().count() <= width {
                    current = word.to_string();
                } else {
                    let mut chunk = String::new();
                    for ch in word.chars() {
                        chunk.push(ch);
                        if chunk.chars().count() >= width {
                            lines.push(chunk);
                            chunk = String::new();
                        }
                    }
                    current = chunk;
                }
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }

    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

pub(crate) fn print_wrapped_rows(
    prefix_text: &str,
    prefix_display: String,
    value: &str,
    width: usize,
    value_style: impl Fn(&str) -> console::StyledObject<&str>,
) {
    let content_width = width
        .saturating_sub(prefix_text.chars().count())
        .max(MIN_CONTENT_WIDTH);
    let lines = wrap_text(value, content_width);
    for (index, line) in lines.iter().enumerate() {
        if index == 0 {
            println!("{prefix_display}{}", value_style(line));
        } else {
            println!(
                "{}{}",
                " ".repeat(prefix_text.chars().count()),
                value_style(line)
            );
        }
    }
}

pub(crate) fn print_label_value(label: &str, value: impl std::fmt::Display) {
    let prefix_text = format!("  {:<8} ", label);
    print_wrapped_rows(
        &prefix_text,
        format!("{}", style(&prefix_text).dim().bold()),
        &value.to_string(),
        terminal_width(),
        |line| style(line),
    );
}

pub(crate) fn print_next_action(index: usize, command: &str, description: &str) {
    let index_label = format!("{index}.");
    let prefix_text = format!("  {index_label:<2} {command:<18} ");
    print_wrapped_rows(
        &prefix_text,
        format!(
            "{} {} ",
            style(format!("  {index_label:<2}")).dim().bold(),
            style(format!("{command:<18}")).cyan().bold()
        ),
        description,
        terminal_width(),
        |line| style(line),
    );
}

pub(crate) fn print_banner(icon: &str, title: &str, subtitle: &str) {
    println!(
        "{} {}",
        style(icon).cyan().bold(),
        style(title).bold().cyan()
    );
    println!("{}", style(subtitle).dim());
    println!();
}

pub(crate) fn status_marker(
    health: &plug_core::types::ServerHealth,
) -> console::StyledObject<&'static str> {
    match health {
        plug_core::types::ServerHealth::Healthy => style("●").green().bold(),
        plug_core::types::ServerHealth::Degraded => style("!").yellow().bold(),
        plug_core::types::ServerHealth::Failed => style("×").red().bold(),
    }
}

pub(crate) fn status_label(
    health: &plug_core::types::ServerHealth,
) -> console::StyledObject<&'static str> {
    match health {
        plug_core::types::ServerHealth::Healthy => style("Healthy").green(),
        plug_core::types::ServerHealth::Degraded => style("Degraded").yellow(),
        plug_core::types::ServerHealth::Failed => style("Failed").red(),
    }
}

pub(crate) fn print_info_line(message: impl std::fmt::Display) {
    println!("{} {}", style("›").cyan().bold(), message);
}

pub(crate) fn print_success_line(message: impl std::fmt::Display) {
    println!("{} {}", style("•").green().bold(), message);
}

pub(crate) fn print_warning_line(message: impl std::fmt::Display) {
    println!("{} {}", style("!").yellow().bold(), message);
}

pub(crate) fn can_prompt_interactively() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}
