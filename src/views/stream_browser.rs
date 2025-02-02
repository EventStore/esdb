use crate::views::{centered_rect, render_line_numbers, Env, Request, View, ViewCtx, B};
use chrono::{DateTime, Utc};
use crossterm::event::KeyCode;
use eventstore::{RecordedEvent, ResolvedEvent, StreamPosition};
use std::ops::Add;
use std::time::{Duration, SystemTime};
use tui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use tui::style::{Color, Modifier, Style};
use tui::text::Text;
use tui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};
use tui::Frame;

static HEADERS: &[&'static str] = &["Recently Created Streams", "Recently Changed Streams"];
static STREAM_HEADERS: &[&'static str] = &["Event #", "Name", "Type", "Created Date"];

#[derive(Copy, Clone, Eq, PartialEq)]
enum Stage {
    Main,
    Stream,
    StreamPreview,
    Search,
}

pub struct StreamsView {
    selected_tab: usize,
    selected: usize,
    main_table_states: Vec<TableState>,
    stream_table_state: TableState,
    model: Model,
    stage: Stage,
    scroll: u16,
    buffer: String,
    last_error: Option<eventstore::Error>,
}

impl Default for StreamsView {
    fn default() -> Self {
        Self {
            selected_tab: 0,
            selected: 0,
            main_table_states: vec![TableState::default(), TableState::default()],
            stream_table_state: Default::default(),
            model: Default::default(),
            stage: Stage::Main,
            scroll: 0,
            buffer: Default::default(),
            last_error: None,
        }
    }
}

#[derive(Default)]
struct Model {
    last_created: Vec<String>,
    recently_changed: Vec<String>,
    selected_stream: Option<String>,
    selected_stream_events: Vec<ResolvedEvent>,
}

impl Model {
    fn clear(&mut self) {
        self.last_created.clear();
        self.recently_changed.clear();
        self.selected_stream = None;
        self.selected_stream_events.clear();
    }
}

impl StreamsView {
    fn load_streams(&mut self, env: &Env) -> eventstore::Result<()> {
        let client = env.client.clone();
        self.model = env.handle.block_on(async move {
            let mut model = Model::default();
            let options_1 = eventstore::ReadStreamOptions::default()
                .max_count(20)
                .position(StreamPosition::End)
                .backwards();

            let options_2 = eventstore::ReadAllOptions::default()
                .max_count(20)
                .position(StreamPosition::End)
                .backwards();

            let mut stream_names = client.read_stream("$streams", &options_1).await?;
            let mut all_stream = client.read_all(&options_2).await?;

            while let Some(event) = read_stream_next(&mut stream_names).await? {
                let (_, stream_name) =
                    std::str::from_utf8(event.get_original_event().data.as_ref())
                        .expect("UTF-8 formatted text")
                        .rsplit_once('@')
                        .unwrap_or_default();

                model.last_created.push(stream_name.to_string());
            }

            while let Some(event) = read_stream_next(&mut all_stream).await? {
                let stream_id = &event.get_original_event().stream_id;
                if model.recently_changed.contains(stream_id) {
                    continue;
                }

                model.recently_changed.push(stream_id.clone());
            }

            Ok::<_, eventstore::Error>(model)
        })?;

        Ok(())
    }
}

impl View for StreamsView {
    fn load(&mut self, env: &Env) -> eventstore::Result<()> {
        self.load_streams(env)
    }

    fn unload(&mut self, _env: &Env) {
        self.selected = 0;
        self.selected_tab = 0;
        self.scroll = 0;
        self.stage = Stage::Main;
        self.model.clear();
        self.last_error = None;
    }

    fn refresh(&mut self, env: &Env) -> eventstore::Result<()> {
        if self.stage == Stage::StreamPreview {
            return Ok(());
        }

        if let Some(stream_name) = self.model.selected_stream.clone() {
            let client = env.client.clone();
            let result = env.handle.block_on(async move {
                let mut stream = if stream_name.trim() == "$all" {
                    let options = eventstore::ReadAllOptions::default()
                        .max_count(500)
                        .resolve_link_tos()
                        .position(StreamPosition::End)
                        .backwards();

                    client.read_all(&options).await?
                } else {
                    let options = eventstore::ReadStreamOptions::default()
                        .max_count(500)
                        .resolve_link_tos()
                        .position(StreamPosition::End)
                        .backwards();

                    client.read_stream(stream_name, &options).await?
                };

                let mut events = Vec::new();

                while let Some(event) = stream.next().await? {
                    events.push(event);
                }

                Ok::<_, eventstore::Error>(events)
            });

            match result {
                Err(e) => {
                    self.last_error = Some(e);
                    self.model.selected_stream_events.clear();
                }
                Ok(xs) => self.model.selected_stream_events = xs,
            }

            Ok(())
        } else {
            self.load_streams(env)
        }
    }

    fn draw(&mut self, ctx: ViewCtx, frame: &mut Frame<B>, area: Rect) {
        match self.stage {
            Stage::Main | Stage::Search => {
                let rects = Layout::default()
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
                    .direction(Direction::Horizontal)
                    .margin(2)
                    .split(area);

                for (idx, name) in HEADERS.iter().enumerate() {
                    let header_cells =
                        vec![Cell::from(*name).style(Style::default().fg(Color::Green))];
                    let header = Row::new(header_cells)
                        .style(ctx.normal_style)
                        .height(1)
                        .bottom_margin(1);

                    let cells = match idx {
                        0 => self.model.last_created.iter(),
                        _ => self.model.recently_changed.iter(),
                    };

                    if self.selected_tab == idx {
                        self.main_table_states[idx].select(Some(self.selected));
                    } else {
                        self.main_table_states[idx].select(None);
                    }

                    let rows = cells
                        .map(|c| {
                            Row::new(vec![
                                Cell::from(c.as_str()).style(Style::default().fg(Color::Gray))
                            ])
                        })
                        .collect::<Vec<_>>();

                    let border_type = if idx == 0 {
                        Borders::TOP | Borders::RIGHT
                    } else {
                        Borders::TOP
                    };

                    let table = Table::new(rows)
                        .header(header)
                        .block(Block::default().borders(border_type))
                        .highlight_style(ctx.selected_style)
                        .widths(&[Constraint::Percentage(100)]);

                    frame.render_stateful_widget(
                        table,
                        rects[idx],
                        &mut self.main_table_states[idx],
                    );

                    if let Stage::Search = self.stage {
                        let block = Block::default()
                            .title("Search")
                            .borders(Borders::ALL)
                            .style(Style::default().add_modifier(Modifier::REVERSED));
                        let area = centered_rect(40, 15, frame.size());
                        frame.render_widget(Clear, area);
                        frame.render_widget(block, area);

                        let layout = Layout::default()
                            .margin(2)
                            .constraints([Constraint::Length(13), Constraint::Max(100)])
                            .direction(Direction::Horizontal)
                            .split(area);

                        let label =
                            Paragraph::new("Stream name: ").style(Style::default().fg(Color::Gray));

                        frame.render_widget(label, layout[0]);

                        let mut input = std::iter::repeat('_').take(100).collect::<String>();

                        let char_count = self.buffer.chars().count();
                        input.replace_range(..char_count, self.buffer.as_str());

                        let input = Paragraph::new(input).style(Style::default().fg(Color::Gray));

                        frame.render_widget(input, layout[1]);
                    }
                }
            }
            Stage::Stream => {
                let rects = Layout::default()
                    .constraints([Constraint::Percentage(100)].as_ref())
                    .margin(2)
                    .split(area);

                let stream_name = self.model.selected_stream.clone().unwrap_or_default();

                let header_cells = STREAM_HEADERS
                    .iter()
                    .map(|h| Cell::from(*h).style(Style::default().fg(Color::Green)));

                let header = Row::new(header_cells)
                    .style(ctx.normal_style)
                    .height(1)
                    .bottom_margin(1);

                let mut rows = Vec::new();

                for event in self.model.selected_stream_events.iter() {
                    let rev = event.get_original_event().revision;
                    let event = event.event.as_ref().unwrap();
                    let mut cols = Vec::new();

                    cols.push(Cell::from(rev.to_string()).style(Style::default().fg(Color::Gray)));

                    let name = format!("{}@{}", event.revision, event.stream_id);
                    cols.push(Cell::from(name).style(Style::default().fg(Color::Gray)));
                    cols.push(
                        Cell::from(event.event_type.clone())
                            .style(Style::default().fg(Color::Gray)),
                    );
                    cols.push(
                        Cell::from(event.created.to_string())
                            .style(Style::default().fg(Color::Gray)),
                    );

                    rows.push(Row::new(cols));
                }

                let table = Table::new(rows)
                    .header(header)
                    .block(
                        Block::default()
                            .borders(Borders::TOP)
                            .title(format!("Event Stream '{}'", stream_name))
                            .title_alignment(Alignment::Right),
                    )
                    .highlight_style(ctx.selected_style)
                    .widths(&[
                        Constraint::Percentage(25),
                        Constraint::Percentage(25),
                        Constraint::Percentage(25),
                        Constraint::Percentage(25),
                    ]);

                self.stream_table_state.select(Some(self.selected));

                frame.render_stateful_widget(table, rects[0], &mut self.stream_table_state);
            }
            Stage::StreamPreview => {
                let rects = Layout::default()
                    .constraints([Constraint::Length(4), Constraint::Min(0)].as_ref())
                    .margin(2)
                    .split(area);

                let header_cells = STREAM_HEADERS
                    .iter()
                    .map(|h| Cell::from(*h).style(Style::default().fg(Color::Green)));

                let header = Row::new(header_cells)
                    .style(ctx.normal_style)
                    .height(1)
                    .bottom_margin(1);

                let mut rows = Vec::new();
                let event = &self.model.selected_stream_events[self.selected];
                let target_event = event.event.as_ref().unwrap();
                let mut cols = Vec::new();

                cols.push(
                    Cell::from(event.get_original_event().revision.to_string())
                        .style(Style::default().fg(Color::Gray)),
                );

                let name = format!(
                    "{}@{}",
                    event.get_original_event().revision,
                    event.get_original_event().stream_id
                );
                cols.push(Cell::from(name.as_str()).style(Style::default().fg(Color::Gray)));
                cols.push(
                    Cell::from(target_event.event_type.clone())
                        .style(Style::default().fg(Color::Gray)),
                );
                cols.push(
                    Cell::from(target_event.created.to_string())
                        .style(Style::default().fg(Color::Gray)),
                );

                rows.push(Row::new(cols));

                let table = Table::new(rows)
                    .header(header)
                    .block(
                        Block::default()
                            .borders(Borders::TOP)
                            .title(format!("Event '{}'", name))
                            .title_alignment(Alignment::Right),
                    )
                    .highlight_style(ctx.selected_style)
                    .widths(&[
                        Constraint::Percentage(25),
                        Constraint::Percentage(25),
                        Constraint::Percentage(25),
                        Constraint::Percentage(25),
                    ]);

                self.stream_table_state.select(Some(self.selected));

                frame.render_stateful_widget(table, rects[0], &mut Default::default());

                let content = if event.event.as_ref().unwrap().is_json {
                    let json =
                        serde_json::from_slice::<serde_json::Value>(target_event.data.as_ref())
                            .unwrap();

                    let content = serde_json::to_string_pretty(&json).unwrap();
                    render_line_numbers(content.as_str())
                } else {
                    "<BINARY>".to_string()
                };

                let text = Text::from(content);

                if rects[1].height >= 2 + text.height() as u16 {
                    // We lock scrolling as everything is visible.
                    self.scroll = 0;
                } else if self.scroll > (2 + text.height() as u16) - rects[1].height {
                    // We cap how much we can scroll. It will be difficult to do that part during
                    // the refresh call as the user might have resized the terminal.
                    self.scroll = (2 + text.height() as u16) - rects[1].height;
                }

                let paragraph = Paragraph::new(text)
                    .alignment(Alignment::Left)
                    .block(Block::default().borders(Borders::BOTTOM | Borders::TOP))
                    .scroll((self.scroll, 0));

                frame.render_widget(paragraph, rects[1])
            }
        }

        if let Some(e) = self.last_error.as_ref() {
            let stream_name = self.model.selected_stream.clone().unwrap_or_default();
            let block = Block::default()
                .title("Error")
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL)
                .style(Style::default().bg(Color::Black).fg(Color::Yellow));
            let area = centered_rect(40, 20, frame.size());
            frame.render_widget(Clear, area);
            frame.render_widget(block, area);

            let rect = Layout::default()
                .margin(2)
                .constraints([Constraint::Percentage(100)])
                .direction(Direction::Horizontal)
                .split(area)[0];

            let message = format!("Stream '{}': {}. Press 'q' to close.", stream_name, e);

            let label = Paragraph::new(message)
                .style(Style::default().fg(Color::Gray))
                .wrap(Wrap { trim: false });

            frame.render_widget(label, rect);
        }
    }

    fn on_key_pressed(&mut self, key: KeyCode) -> Request {
        if self.last_error.is_some() {
            if let KeyCode::Char('q' | 'Q') = key {
                self.last_error = None;
                self.stage = Stage::Main;
                self.model.selected_stream = None;
            }

            return Request::Noop;
        }

        if self.stage == Stage::Search {
            match key {
                KeyCode::Esc => self.stage = Stage::Main,
                KeyCode::Backspace => {
                    self.buffer.pop();
                }
                KeyCode::Enter => {
                    self.selected = 0;
                    self.stage = Stage::Stream;
                    self.model.selected_stream =
                        Some(std::mem::replace(&mut self.buffer, Default::default()));
                    return Request::Refresh;
                }
                KeyCode::Char(c) if c.is_ascii() => self.buffer.push(c),
                _ => {}
            }

            return Request::Noop;
        }

        match key {
            KeyCode::Char('q' | 'Q') => {
                return match self.stage {
                    Stage::Main => Request::Exit,
                    Stage::Search => Request::Noop,
                    Stage::Stream => {
                        self.stage = Stage::Main;
                        self.selected = 0;
                        Request::Noop
                    }
                    Stage::StreamPreview => {
                        self.scroll = 0;
                        self.stage = Stage::Stream;
                        Request::Noop
                    }
                }
            }

            KeyCode::Char('/') => {
                if self.stage == Stage::Main {
                    self.stage = Stage::Search;
                }
            }
            KeyCode::Left | KeyCode::Right => {
                self.selected_tab = (self.selected_tab + 1) % 2;
                self.selected = 0;
            }

            KeyCode::Up => {
                if self.stage == Stage::StreamPreview {
                    if self.scroll > 0 {
                        self.scroll -= 1;
                    }
                } else if self.selected > 0 {
                    self.selected -= 1;
                }
            }

            KeyCode::Down => match self.stage {
                Stage::Main => {
                    let len = if self.selected_tab == 0 {
                        self.model.last_created.len()
                    } else {
                        self.model.recently_changed.len()
                    };

                    if self.selected < len - 1 {
                        self.selected += 1;
                    }
                }
                Stage::Stream => {
                    if self.selected < self.model.selected_stream_events.len() - 1 {
                        self.selected += 1;
                    }
                }
                Stage::StreamPreview => {
                    self.scroll += 1;
                }

                _ => {}
            },

            KeyCode::Enter => {
                if self.stage == Stage::Main {
                    self.stage = Stage::Stream;

                    let rows = if self.selected_tab == 0 {
                        &self.model.last_created
                    } else {
                        &self.model.recently_changed
                    };

                    self.model.selected_stream = Some(rows[self.selected].clone());
                    self.selected = 0;

                    return Request::Refresh;
                } else if self.stage == Stage::Stream {
                    self.stage = Stage::StreamPreview;

                    return Request::Refresh;
                }
            }

            _ => {}
        }

        Request::Noop
    }

    fn keybindings(&self) -> &[(&str, &str)] {
        match self.stage {
            Stage::StreamPreview => &[("↑", "Scroll up"), ("↓", "Scroll down"), ("q", "Close")],
            Stage::Stream => &[
                ("↑", "Scroll up"),
                ("↓", "Scroll down"),
                ("Enter", "Select"),
                ("q", "Close"),
            ],
            Stage::Main | Stage::Search => &[
                ("↑", "Scroll up"),
                ("↓", "Scroll down"),
                ("→", "Move right"),
                ("← ", "Move left"),
                ("/", "Search"),
                ("Enter", "Select"),
            ],
        }
    }
}

async fn read_stream_next(
    stream: &mut eventstore::ReadStream,
) -> eventstore::Result<Option<eventstore::ResolvedEvent>> {
    match stream.next().await {
        Err(e) => {
            if let eventstore::Error::ResourceNotFound = e {
                return Ok(None);
            }

            Err(e)
        }
        Ok(v) => Ok(v),
    }
}
