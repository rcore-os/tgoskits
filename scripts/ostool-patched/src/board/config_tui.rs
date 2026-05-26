use std::{
    io::{self, Stdout},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, bail};
use crossterm::{
    event::{self, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::board::global_config::{BoardGlobalConfig, LoadedBoardGlobalConfig};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const FORM_CONTENT_HEIGHT: u16 = 14;
const FORM_MARGIN_Y: u16 = 2;
const FORM_BORDER_Y: u16 = 2;
const FORM_HEIGHT: u16 = FORM_CONTENT_HEIGHT + FORM_MARGIN_Y + FORM_BORDER_Y;
const FORM_MIN_WIDTH: u16 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveField {
    ServerIp,
    Port,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorOutcome {
    Saved,
    Cancelled,
}

#[derive(Debug, Clone)]
struct InputField {
    value: String,
    cursor: usize,
}

impl InputField {
    fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self { value, cursor }
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        self.cursor = self.clamp_cursor(self.cursor.saturating_add(1));
    }

    fn move_to_end(&mut self) {
        self.cursor = self.value.chars().count();
    }

    fn insert_char(&mut self, ch: char) {
        let index = self.byte_index();
        self.value.insert(index, ch);
        self.move_right();
    }

    fn delete_left(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let current_index = self.cursor;
        let left_index = current_index - 1;
        let before = self.value.chars().take(left_index);
        let after = self.value.chars().skip(current_index);
        self.value = before.chain(after).collect();
        self.move_left();
    }

    fn byte_index(&self) -> usize {
        self.value
            .char_indices()
            .map(|(index, _)| index)
            .nth(self.cursor)
            .unwrap_or(self.value.len())
    }

    fn clamp_cursor(&self, cursor: usize) -> usize {
        cursor.clamp(0, self.value.chars().count())
    }

    fn visible_text_and_cursor(&self, max_chars: usize) -> (String, u16) {
        if max_chars == 0 {
            return (String::new(), 0);
        }

        let total_chars = self.value.chars().count();
        if total_chars <= max_chars {
            return (self.value.clone(), self.cursor as u16);
        }

        let mut start = self.cursor.saturating_sub(max_chars.saturating_sub(1));
        if start + max_chars > total_chars {
            start = total_chars.saturating_sub(max_chars);
        }
        let end = (start + max_chars).min(total_chars);
        let visible = self
            .value
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect();
        let cursor = self.cursor.saturating_sub(start).min(max_chars) as u16;
        (visible, cursor)
    }
}

#[derive(Debug, Clone)]
struct BoardConfigApp {
    path: PathBuf,
    server_ip: InputField,
    port: InputField,
    active: ActiveField,
    error: Option<String>,
}

impl BoardConfigApp {
    fn new(path: PathBuf, config: BoardGlobalConfig) -> Self {
        Self {
            path,
            server_ip: InputField::new(config.server_ip),
            port: InputField::new(config.port.to_string()),
            active: ActiveField::ServerIp,
            error: None,
        }
    }

    fn from_loaded_config(config: LoadedBoardGlobalConfig) -> Self {
        Self::new(config.path, config.board)
    }

    fn run(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> anyhow::Result<EditorOutcome> {
        loop {
            terminal
                .draw(|frame| self.render(frame))
                .context("failed to draw board config tui")?;

            if !event::poll(EVENT_POLL_INTERVAL).context("failed to poll terminal event")? {
                continue;
            }

            if let Some(key) = event::read()
                .context("failed to read terminal event")?
                .as_key_press_event()
            {
                match self.handle_key_event(key) {
                    Ok(Some(outcome)) => return Ok(outcome),
                    Ok(None) => {}
                    Err(err) => self.error = Some(err.to_string()),
                }
            }
        }
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> anyhow::Result<Option<EditorOutcome>> {
        if key.kind == KeyEventKind::Release {
            return Ok(None);
        }

        match key.code {
            KeyCode::Esc => return Ok(Some(EditorOutcome::Cancelled)),
            KeyCode::Tab | KeyCode::Down => {
                self.focus_next();
                self.error = None;
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.focus_prev();
                self.error = None;
            }
            KeyCode::Left => {
                self.active_field_mut().move_left();
                self.error = None;
            }
            KeyCode::Right => {
                self.active_field_mut().move_right();
                self.error = None;
            }
            KeyCode::Home => {
                self.active_field_mut().cursor = 0;
                self.error = None;
            }
            KeyCode::End => {
                self.active_field_mut().move_to_end();
                self.error = None;
            }
            KeyCode::Backspace => {
                self.active_field_mut().delete_left();
                self.error = None;
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save()?;
                return Ok(Some(EditorOutcome::Saved));
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_char(ch);
                self.error = None;
            }
            _ => {}
        }

        Ok(None)
    }

    fn insert_char(&mut self, ch: char) {
        match self.active {
            ActiveField::ServerIp => self.server_ip.insert_char(ch),
            ActiveField::Port if ch.is_ascii_digit() => self.port.insert_char(ch),
            ActiveField::Port => {}
        }
    }

    fn save(&mut self) -> anyhow::Result<()> {
        let config = self.validate()?;
        LoadedBoardGlobalConfig {
            path: self.path.clone(),
            board: config,
            created: false,
        }
        .save()?;
        self.error = None;
        Ok(())
    }

    fn validate(&self) -> anyhow::Result<BoardGlobalConfig> {
        let server_ip = self.server_ip.value.trim().to_string();
        if server_ip.is_empty() {
            bail!("server_ip must not be empty");
        }

        let port: u16 = self
            .port
            .value
            .trim()
            .parse()
            .context("port must be a valid integer")?;
        if port == 0 {
            bail!("port must be in 1..=65535");
        }

        Ok(BoardGlobalConfig { server_ip, port })
    }

    fn focus_next(&mut self) {
        self.active = match self.active {
            ActiveField::ServerIp => ActiveField::Port,
            ActiveField::Port => ActiveField::ServerIp,
        };
    }

    fn focus_prev(&mut self) {
        self.focus_next();
    }

    fn active_field_mut(&mut self) -> &mut InputField {
        match self.active {
            ActiveField::ServerIp => &mut self.server_ip,
            ActiveField::Port => &mut self.port,
        }
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        frame.render_widget(
            Block::default().style(Style::default().bg(Color::Rgb(11, 19, 26))),
            area,
        );

        let outer = centered_rect(area);
        let block = Block::default()
            .title(Line::from(vec![Span::styled(
                " OSTool Board Config ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Gray));
        let inner = block.inner(outer);
        frame.render_widget(block, outer);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Length(2),
            ])
            .margin(1)
            .split(inner);

        let title =
            Paragraph::new("Edit the default ostool-server connection used by board commands.")
                .style(Style::default().fg(Color::White));
        frame.render_widget(title, chunks[0]);

        let server_cursor = draw_input(
            frame,
            chunks[1],
            "server_ip",
            &self.server_ip,
            self.active == ActiveField::ServerIp,
        );
        let port_cursor = draw_input(
            frame,
            chunks[2],
            "port",
            &self.port,
            self.active == ActiveField::Port,
        );

        let path = Paragraph::new(format!("config: {}", self.path.display()))
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(path, chunks[3]);

        let error = self.error.as_deref().unwrap_or(" ");
        let error_widget = Paragraph::new(error)
            .style(Style::default().fg(Color::LightRed))
            .wrap(Wrap { trim: true });
        frame.render_widget(error_widget, chunks[4]);

        let footer = Paragraph::new(Line::from(vec![
            Span::styled("Tab/↑↓", Style::default().fg(Color::Yellow)),
            Span::raw(" switch  "),
            Span::styled("←/→", Style::default().fg(Color::Yellow)),
            Span::raw(" move  "),
            Span::styled(
                "Ctrl+S",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" save  "),
            Span::styled("Esc", Style::default().fg(Color::Magenta)),
            Span::raw(" cancel"),
        ]))
        .style(Style::default().fg(Color::Gray));
        frame.render_widget(footer, chunks[5]);

        let (cursor_area, cursor_offset) = match self.active {
            ActiveField::ServerIp => (chunks[1], server_cursor),
            ActiveField::Port => (chunks[2], port_cursor),
        };
        frame.set_cursor_position(Position::new(
            cursor_area.x + cursor_offset + 1,
            cursor_area.y + 1,
        ));
    }
}

pub fn run_board_config_tui() -> anyhow::Result<()> {
    let loaded = LoadedBoardGlobalConfig::load_or_create()?;
    let saved_path = loaded.path.clone();
    let mut app = BoardConfigApp::from_loaded_config(loaded);

    let mut terminal = setup_terminal()?;
    let run_result = app.run(&mut terminal);
    let cleanup_result = restore_terminal(&mut terminal);

    let outcome = run_result?;
    cleanup_result?;

    if outcome == EditorOutcome::Saved {
        println!("Saved board config: {}", saved_path.display());
    }

    Ok(())
}

fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create terminal")?;
    terminal.hide_cursor().context("failed to hide cursor")?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> anyhow::Result<()> {
    disable_raw_mode().context("failed to disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("failed to leave alternate screen")?;
    terminal.show_cursor().context("failed to show cursor")?;
    Ok(())
}

fn draw_input(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    label: &str,
    field: &InputField,
    active: bool,
) -> u16 {
    let border_style = if active {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title_style = if active {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let text_style = if active {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::Rgb(210, 214, 220))
    };

    let max_chars = area.width.saturating_sub(2) as usize;
    let (visible_text, cursor_offset) = field.visible_text_and_cursor(max_chars);

    let paragraph = Paragraph::new(visible_text).style(text_style).block(
        Block::default()
            .title(Span::styled(format!(" {label} "), title_style))
            .borders(Borders::ALL)
            .border_style(border_style),
    );
    frame.render_widget(paragraph, area);
    cursor_offset
}

fn centered_rect(area: Rect) -> Rect {
    let desired_height = FORM_HEIGHT.min(area.height.saturating_sub(2).max(1));
    let desired_width = FORM_MIN_WIDTH.min(area.width.saturating_sub(2).max(1));

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(18),
            Constraint::Length(desired_height),
            Constraint::Percentage(18),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(12),
            Constraint::Length(desired_width),
            Constraint::Percentage(12),
        ])
        .split(vertical[1]);
    horizontal[1]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use tempfile::tempdir;

    use super::{ActiveField, BoardConfigApp, InputField};
    use crate::board::global_config::BoardGlobalConfig;

    #[test]
    fn app_initializes_from_existing_config() {
        let app = BoardConfigApp::new(
            PathBuf::from("/tmp/config.toml"),
            BoardGlobalConfig {
                server_ip: "10.0.0.2".into(),
                port: 9000,
            },
        );

        assert_eq!(app.server_ip.value, "10.0.0.2");
        assert_eq!(app.port.value, "9000");
        assert_eq!(app.active, ActiveField::ServerIp);
    }

    #[test]
    fn input_field_inserts_at_cursor() {
        let mut field = InputField::new("ac");
        field.cursor = 1;
        field.insert_char('b');

        assert_eq!(field.value, "abc");
        assert_eq!(field.cursor, 2);
    }

    #[test]
    fn handle_key_edits_server_ip() {
        let mut app = BoardConfigApp::new(
            PathBuf::from("/tmp/config.toml"),
            BoardGlobalConfig::default(),
        );
        app.server_ip = InputField::new("");

        let outcome = app
            .handle_key_event(KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: crossterm::event::KeyEventState::NONE,
            })
            .unwrap();

        assert!(outcome.is_none());
        assert_eq!(app.server_ip.value, "a");
    }

    #[test]
    fn handle_key_switches_active_field() {
        let mut app = BoardConfigApp::new(
            PathBuf::from("/tmp/config.toml"),
            BoardGlobalConfig::default(),
        );

        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.active, ActiveField::Port);
    }

    #[test]
    fn save_persists_valid_values() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(".ostool/config.toml");
        let mut app = BoardConfigApp::new(path.clone(), BoardGlobalConfig::default());
        app.server_ip = InputField::new("10.0.0.2");
        app.port = InputField::new("9000");

        app.save().unwrap();

        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("server_ip = \"10.0.0.2\""));
        assert!(content.contains("port = 9000"));
    }

    #[test]
    fn save_rejects_empty_server_ip() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(".ostool/config.toml");
        let mut app = BoardConfigApp::new(path.clone(), BoardGlobalConfig::default());
        app.server_ip = InputField::new("   ");

        let err = app.save().unwrap_err();
        assert!(err.to_string().contains("server_ip"));
        assert!(!path.exists());
    }

    #[test]
    fn save_rejects_invalid_port() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(".ostool/config.toml");
        let mut app = BoardConfigApp::new(path.clone(), BoardGlobalConfig::default());
        app.port = InputField::new("70000");

        let err = app.save().unwrap_err();
        assert!(err.to_string().contains("port"));
        assert!(!path.exists());
    }

    #[test]
    fn input_field_scrolls_visible_window_with_cursor() {
        let mut field = InputField::new("abcdefghij");
        field.cursor = 10;

        let (visible, cursor) = field.visible_text_and_cursor(5);

        assert_eq!(visible, "fghij");
        assert_eq!(cursor, 5);
    }
}
