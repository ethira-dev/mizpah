//! Minimal ratatui client against the live hub (Phase J).

use crate::keymap::Keymap;
use crate::mcp::HubClient;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Terminal;
use serde_json::Value;
use std::io::{self, stdout};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub async fn run_tui(host: &str, port: u16) -> Result<(), TuiError> {
    #[cfg(test)]
    if let Some(events) = test_tui_events::take() {
        return run_tui_with_events(host, port, events).await;
    }
    run_tui_with_events(host, port, CrosstermEvents).await
}

#[cfg(test)]
mod test_tui_events {
    use super::ScriptedEvents;
    use std::sync::{Mutex, OnceLock};

    static EVENTS: OnceLock<Mutex<Option<ScriptedEvents>>> = OnceLock::new();

    fn cell() -> &'static Mutex<Option<ScriptedEvents>> {
        EVENTS.get_or_init(|| Mutex::new(None))
    }

    pub(super) fn set(events: ScriptedEvents) {
        *cell().lock().unwrap() = Some(events);
    }

    pub(super) fn take() -> Option<ScriptedEvents> {
        cell().lock().ok().and_then(|mut g| g.take())
    }
}

/// Shared entry used by production (crossterm) and tests (scripted events + TestBackend).
async fn run_tui_with_events<E: EventSource>(
    host: &str,
    port: u16,
    events: E,
) -> Result<(), TuiError> {
    let keymap = Keymap::load();
    let client = HubClient::new(format!("http://{host}:{port}"));
    let mut entries = fetch_entries(&client).await?;
    let mut selected = 0usize;
    let mut status = String::new();

    if E::uses_crossterm_terminal() {
        enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(out);
        let mut terminal = Terminal::new(backend)?;
        let result = run_tui_loop(
            &mut terminal,
            &client,
            &keymap,
            host,
            port,
            &mut entries,
            &mut selected,
            &mut status,
            events,
        )
        .await;
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;
        result
    } else {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;
        run_tui_loop(
            &mut terminal,
            &client,
            &keymap,
            host,
            port,
            &mut entries,
            &mut selected,
            &mut status,
            events,
        )
        .await
    }
}

/// Source of terminal events for the TUI loop (production: crossterm poll; tests: scripted).
trait EventSource {
    /// When true, `None` from [`next_event`] ends the loop (scripted tests).
    /// When false, `None` means idle/timeout and the loop continues (crossterm).
    fn ends_on_empty(&self) -> bool {
        false
    }
    /// Production crossterm path needs raw mode + alternate screen.
    fn uses_crossterm_terminal() -> bool {
        false
    }
    async fn next_event(&mut self) -> Result<Option<Event>, TuiError>;
}

struct CrosstermEvents;

impl EventSource for CrosstermEvents {
    fn uses_crossterm_terminal() -> bool {
        true
    }
    async fn next_event(&mut self) -> Result<Option<Event>, TuiError> {
        if event::poll(Duration::from_millis(200))? {
            Ok(Some(event::read()?))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
struct ScriptedEvents {
    events: std::vec::IntoIter<Event>,
}

#[cfg(test)]
impl EventSource for ScriptedEvents {
    fn ends_on_empty(&self) -> bool {
        true
    }
    async fn next_event(&mut self) -> Result<Option<Event>, TuiError> {
        Ok(self.events.next())
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_tui_loop<B: Backend, E: EventSource>(
    terminal: &mut Terminal<B>,
    client: &HubClient,
    keymap: &Keymap,
    host: &str,
    port: u16,
    entries: &mut Vec<Value>,
    selected: &mut usize,
    status: &mut String,
    mut events: E,
) -> Result<(), TuiError> {
    loop {
        draw_frame(terminal, keymap, host, port, entries, *selected, status)?;

        let Some(ev) = events.next_event().await? else {
            if events.ends_on_empty() {
                break Ok(());
            }
            continue;
        };

        let Event::Key(key) = ev else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match classify_key(keymap, key.code) {
            KeyAction::Quit => break Ok(()),
            KeyAction::Down => {
                if *selected + 1 < entries.len() {
                    *selected += 1;
                }
            }
            KeyAction::Up => {
                *selected = selected.saturating_sub(1);
            }
            KeyAction::NextError => match nav_error(client, entries, *selected, "next").await {
                Ok(Some(i)) => {
                    *selected = i;
                    status.clear();
                }
                Ok(None) => *status = "no next error/warn".into(),
                Err(e) => *status = e.to_string(),
            },
            KeyAction::PrevError => match nav_error(client, entries, *selected, "prev").await {
                Ok(Some(i)) => {
                    *selected = i;
                    status.clear();
                }
                Ok(None) => *status = "no prev error/warn".into(),
                Err(e) => *status = e.to_string(),
            },
            KeyAction::ShowTrace => match load_trace(client, entries, *selected).await {
                Ok(Some(trace)) => {
                    *entries = trace;
                    *selected = 0;
                    *status = format!("trace · {} rows", entries.len());
                }
                Ok(None) => *status = "no opid on selected row".into(),
                Err(e) => *status = e.to_string(),
            },
            KeyAction::Refresh => {
                *entries = fetch_entries(client).await?;
                *selected = (*selected).min(entries.len().saturating_sub(1));
                *status = "refreshed".into();
            }
            KeyAction::Ignore => {}
        }
    }
}

fn draw_frame<B: Backend>(
    terminal: &mut Terminal<B>,
    keymap: &Keymap,
    host: &str,
    port: u16,
    entries: &[Value],
    selected: usize,
    status: &str,
) -> Result<(), TuiError> {
    terminal.draw(|f| {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(2)])
            .split(f.area());

        let items: Vec<ListItem> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let id = e.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                let svc = e.get("service").and_then(|v| v.as_str()).unwrap_or("?");
                let data = e.get("data").cloned().unwrap_or(Value::Null);
                let level = data
                    .get("level")
                    .or_else(|| data.get("severity"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let msg = data
                    .get("msg")
                    .or_else(|| data.get("message"))
                    .or_else(|| data.get("_raw"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let line = format!("{id:>6} {svc} [{level}] {msg}");
                let style = if i == selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(Span::styled(line, style)))
            })
            .collect();

        let list = List::new(items).block(Block::default().borders(Borders::ALL).title(format!(
            " Mizpah TUI ({} quit · {}/{} error · {}/{} move · {} trace) ",
            keymap.quit,
            keymap.next_error,
            keymap.prev_error,
            keymap.down,
            keymap.up,
            keymap.show_trace
        )));
        f.render_widget(list, chunks[0]);

        let help = Paragraph::new(format!(
            "hub http://{host}:{port} · {} entries{}",
            entries.len(),
            if status.is_empty() {
                String::new()
            } else {
                format!(" · {status}")
            }
        ));
        f.render_widget(help, chunks[1]);
    })?;
    Ok(())
}

async fn fetch_entries(client: &HubClient) -> Result<Vec<Value>, TuiError> {
    let resp = client
        .search_logs(None, None, Some(100), None)
        .await
        .map_err(|e| TuiError::Message(e.to_string()))?;
    Ok(resp.entries)
}

async fn nav_error(
    client: &HubClient,
    entries: &[Value],
    selected: usize,
    direction: &str,
) -> Result<Option<usize>, TuiError> {
    if let Some(i) = find_level(entries, selected, if direction == "prev" { -1 } else { 1 }) {
        return Ok(Some(i));
    }
    let from_id = entries
        .get(selected)
        .and_then(|e| e.get("id"))
        .and_then(|v| v.as_u64())
        .unwrap_or(u64::MAX);
    let hit = client
        .nav_level(from_id, direction, &["error", "warn"])
        .await
        .map_err(|e| TuiError::Message(e.to_string()))?;
    let Some(entry) = hit else {
        return Ok(None);
    };
    let id = entry.get("id").and_then(|v| v.as_u64());
    if let Some(id) = id {
        if let Some(i) = entries
            .iter()
            .position(|e| e.get("id").and_then(|v| v.as_u64()) == Some(id))
        {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

async fn load_trace(
    client: &HubClient,
    entries: &[Value],
    selected: usize,
) -> Result<Option<Vec<Value>>, TuiError> {
    let data = entries
        .get(selected)
        .and_then(|e| e.get("data"))
        .cloned()
        .unwrap_or(Value::Null);
    let opid = resolve_opid(&data);
    let Some(opid) = opid else {
        return Ok(None);
    };
    let resp = client
        .get_trace(&opid, None)
        .await
        .map_err(|e| TuiError::Message(e.to_string()))?;
    Ok(Some(resp.entries))
}

fn resolve_opid(data: &Value) -> Option<String> {
    const KEYS: &[&str] = &[
        "opid",
        "operation_id",
        "operationId",
        "trace_id",
        "traceId",
        "request_id",
        "requestId",
        "correlation_id",
        "correlationId",
    ];
    for k in KEYS {
        if let Some(s) = data.get(*k).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn find_level(entries: &[Value], from: usize, dir: i32) -> Option<usize> {
    let mut i = from as i64 + i64::from(dir);
    while i >= 0 && (i as usize) < entries.len() {
        let data = entries[i as usize]
            .get("data")
            .cloned()
            .unwrap_or(Value::Null);
        let level = data
            .get("level")
            .or_else(|| data.get("severity"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            level.as_str(),
            "error" | "err" | "fatal" | "warn" | "warning"
        ) {
            return Some(i as usize);
        }
        i += i64::from(dir);
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyAction {
    Quit,
    Down,
    Up,
    NextError,
    PrevError,
    ShowTrace,
    Refresh,
    Ignore,
}

fn classify_key(keymap: &Keymap, code: KeyCode) -> KeyAction {
    match code {
        KeyCode::Esc => KeyAction::Quit,
        KeyCode::Down => KeyAction::Down,
        KeyCode::Up => KeyAction::Up,
        KeyCode::Char(c) => {
            let s = c.to_string();
            if s == keymap.quit {
                KeyAction::Quit
            } else if s == keymap.down {
                KeyAction::Down
            } else if s == keymap.up {
                KeyAction::Up
            } else if s == keymap.next_error {
                KeyAction::NextError
            } else if s == keymap.prev_error {
                KeyAction::PrevError
            } else if s == keymap.show_trace {
                KeyAction::ShowTrace
            } else if s == "r" {
                KeyAction::Refresh
            } else {
                KeyAction::Ignore
            }
        }
        _ => KeyAction::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::spawn_test_hub;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn run_tui_with_scripted_quit() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"info","msg":"hi"}"#)
            .await;
        let parsed = url::Url::parse(&url).unwrap();
        let host = parsed.host_str().unwrap();
        let port = parsed.port().unwrap();
        let k = Keymap::load();
        let quit_c = k.quit.chars().next().unwrap_or('q');
        test_tui_events::set(ScriptedEvents {
            events: vec![key(KeyCode::Char(quit_c))].into_iter(),
        });
        run_tui(host, port).await.unwrap();
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn scripted_loop_covers_actions() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"info","msg":"a"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"error","msg":"b","trace_id":"t-1"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"info","msg":"c","trace_id":"t-1"}"#)
            .await;

        let client = HubClient::new(url);
        let keymap = Keymap::load();
        let mut entries = fetch_entries(&client).await.unwrap();
        let mut selected = 0usize;
        let mut status = String::new();
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        let next_c = keymap.next_error.chars().next().unwrap_or('n');
        let prev_c = keymap.prev_error.chars().next().unwrap_or('p');
        let trace_c = keymap.show_trace.chars().next().unwrap_or('t');
        let quit_c = keymap.quit.chars().next().unwrap_or('q');

        let scripted = ScriptedEvents {
            events: vec![
                key(KeyCode::Down),
                key(KeyCode::Up),
                key(KeyCode::Char(next_c)),
                key(KeyCode::Char(prev_c)),
                key(KeyCode::Char('r')),
                key(KeyCode::Char('z')), // ignore
                key(KeyCode::Char(trace_c)),
                key(KeyCode::Char(quit_c)),
            ]
            .into_iter(),
        };

        run_tui_loop(
            &mut terminal,
            &client,
            &keymap,
            "127.0.0.1",
            3149,
            &mut entries,
            &mut selected,
            &mut status,
            scripted,
        )
        .await
        .unwrap();
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn scripted_loop_status_paths() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"info","msg":"only"}"#)
            .await;
        let client = HubClient::new(url);
        let keymap = Keymap::load();
        let mut entries = fetch_entries(&client).await.unwrap();
        let mut selected = 0usize;
        let mut status = String::new();
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let next_c = keymap.next_error.chars().next().unwrap_or('n');
        let trace_c = keymap.show_trace.chars().next().unwrap_or('t');
        let scripted = ScriptedEvents {
            events: vec![
                key(KeyCode::Char(next_c)),
                key(KeyCode::Char(trace_c)),
                key(KeyCode::Esc),
            ]
            .into_iter(),
        };
        run_tui_loop(
            &mut terminal,
            &client,
            &keymap,
            "127.0.0.1",
            1,
            &mut entries,
            &mut selected,
            &mut status,
            scripted,
        )
        .await
        .unwrap();
        assert!(!status.is_empty() || selected == 0);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn fetch_entries_and_nav_from_hub() {
        let service = format!("tui-nav-{}", std::process::id());
        let (url, store) = spawn_test_hub().await;
        store
            .push_line(&service, r#"{"level":"error","msg":"a"}"#)
            .await;
        store
            .push_line(&service, r#"{"level":"info","msg":"mid"}"#)
            .await;
        store
            .push_line(&service, r#"{"level":"warn","msg":"b"}"#)
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let client = HubClient::new(url);
        let resp = client
            .search_logs(Some(&service), None, Some(100), None)
            .await
            .unwrap();
        let entries = resp.entries;
        assert_eq!(entries.len(), 3);
        let mid = entries
            .iter()
            .position(|e| {
                e.get("data")
                    .and_then(|d| d.get("msg"))
                    .and_then(|m| m.as_str())
                    == Some("mid")
            })
            .expect("mid entry");
        let next = nav_error(&client, &entries, mid, "next").await.unwrap();
        assert!(next.is_some());
        let prev = nav_error(&client, &entries, mid, "prev").await.unwrap();
        assert!(prev.is_some());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn load_trace_without_opid() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"info","msg":"x"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"error","msg":"c"}"#)
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let client = HubClient::new(url);
        let entries = fetch_entries(&client).await.unwrap();
        let trace = load_trace(&client, &entries, 0).await.unwrap();
        assert!(trace.is_none());
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn load_trace_with_opid() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"error","msg":"a","trace_id":"t-1"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"info","msg":"b","trace_id":"t-1"}"#)
            .await;
        store
            .push_line("api", r#"{"level":"error","msg":"c"}"#)
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let client = HubClient::new(url);
        let entries = fetch_entries(&client).await.unwrap();
        let selected = entries
            .iter()
            .position(|e| {
                e.get("data")
                    .and_then(|d| d.get("msg"))
                    .and_then(|m| m.as_str())
                    == Some("a")
            })
            .unwrap_or(0);
        let trace = load_trace(&client, &entries, selected).await.unwrap();
        assert!(trace.is_some());
        assert!(!trace.unwrap().is_empty());
    }

    #[test]
    fn find_level_next_error() {
        let entries = vec![
            serde_json::json!({"id":1,"data":{"level":"info"}}),
            serde_json::json!({"id":2,"data":{"level":"error","msg":"x"}}),
            serde_json::json!({"id":3,"data":{"level":"warn"}}),
        ];
        assert_eq!(find_level(&entries, 0, 1), Some(1));
        assert_eq!(find_level(&entries, 1, 1), Some(2));
        assert_eq!(find_level(&entries, 2, -1), Some(1));
    }

    #[test]
    fn classify_key_actions() {
        let k = Keymap::load();
        assert_eq!(classify_key(&k, KeyCode::Char('q')), KeyAction::Quit);
        assert_eq!(classify_key(&k, KeyCode::Esc), KeyAction::Quit);
        assert_eq!(classify_key(&k, KeyCode::Down), KeyAction::Down);
        assert_eq!(classify_key(&k, KeyCode::Up), KeyAction::Up);
        assert_eq!(classify_key(&k, KeyCode::Char('r')), KeyAction::Refresh);
        assert_eq!(classify_key(&k, KeyCode::Char('z')), KeyAction::Ignore);
        if !k.next_error.is_empty() {
            let c = k.next_error.chars().next().unwrap();
            assert_eq!(classify_key(&k, KeyCode::Char(c)), KeyAction::NextError);
        }
        if !k.prev_error.is_empty() {
            let c = k.prev_error.chars().next().unwrap();
            assert_eq!(classify_key(&k, KeyCode::Char(c)), KeyAction::PrevError);
        }
        if !k.show_trace.is_empty() {
            let c = k.show_trace.chars().next().unwrap();
            assert_eq!(classify_key(&k, KeyCode::Char(c)), KeyAction::ShowTrace);
        }
    }

    #[test]
    fn resolve_opid_from_trace_id() {
        let data = serde_json::json!({"traceId":"abc123","msg":"hi"});
        assert_eq!(resolve_opid(&data).as_deref(), Some("abc123"));
    }

    #[test]
    fn resolve_opid_tries_all_keys() {
        let data1 = serde_json::json!({"opid":"op1"});
        assert_eq!(resolve_opid(&data1).as_deref(), Some("op1"));
        let data2 = serde_json::json!({"operation_id":"op2"});
        assert_eq!(resolve_opid(&data2).as_deref(), Some("op2"));
        let data3 = serde_json::json!({"operationId":"op3"});
        assert_eq!(resolve_opid(&data3).as_deref(), Some("op3"));
        let data4 = serde_json::json!({"request_id":"req1"});
        assert_eq!(resolve_opid(&data4).as_deref(), Some("req1"));
        let data5 = serde_json::json!({"requestId":"req2"});
        assert_eq!(resolve_opid(&data5).as_deref(), Some("req2"));
        let data6 = serde_json::json!({"correlation_id":"cor1"});
        assert_eq!(resolve_opid(&data6).as_deref(), Some("cor1"));
        let data7 = serde_json::json!({"correlationId":"cor2"});
        assert_eq!(resolve_opid(&data7).as_deref(), Some("cor2"));
    }

    #[test]
    fn resolve_opid_skips_empty() {
        let data = serde_json::json!({"opid":""});
        assert_eq!(resolve_opid(&data), None);
    }

    #[test]
    fn resolve_opid_no_match() {
        let data = serde_json::json!({"other":"value"});
        assert_eq!(resolve_opid(&data), None);
    }

    #[test]
    fn find_level_boundary_cases() {
        let entries = vec![
            serde_json::json!({"id":1,"data":{"level":"info"}}),
            serde_json::json!({"id":2,"data":{"level":"fatal","msg":"x"}}),
            serde_json::json!({"id":3,"data":{"severity":"err"}}),
            serde_json::json!({"id":4,"data":{"severity":"warning"}}),
        ];
        assert_eq!(find_level(&entries, 0, 1), Some(1));
        assert_eq!(find_level(&entries, 1, 1), Some(2));
        assert_eq!(find_level(&entries, 2, 1), Some(3));
        assert_eq!(find_level(&entries, 3, -1), Some(2));
        assert_eq!(find_level(&entries, 2, -1), Some(1));
    }

    #[test]
    fn find_level_no_match() {
        let entries = vec![
            serde_json::json!({"id":1,"data":{"level":"info"}}),
            serde_json::json!({"id":2,"data":{"level":"debug"}}),
        ];
        assert_eq!(find_level(&entries, 0, 1), None);
    }

    #[test]
    fn classify_key_ignores_unmapped() {
        let k = Keymap::load();
        assert_eq!(classify_key(&k, KeyCode::Enter), KeyAction::Ignore);
        assert_eq!(classify_key(&k, KeyCode::Tab), KeyAction::Ignore);
        assert_eq!(classify_key(&k, KeyCode::Char('x')), KeyAction::Ignore);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn nav_error_when_no_match_in_current_entries() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"info","msg":"a"}"#)
            .await;
        let client = HubClient::new(url);
        let entries = fetch_entries(&client).await.unwrap();
        let result = nav_error(&client, &entries, 0, "next").await.unwrap();
        assert_eq!(result, None);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn nav_error_prev_direction() {
        let service = format!("tui-prev-{}", std::process::id());
        let (url, store) = spawn_test_hub().await;
        store
            .push_line(&service, r#"{"level":"error","msg":"a"}"#)
            .await;
        store
            .push_line(&service, r#"{"level":"info","msg":"b"}"#)
            .await;
        store
            .push_line(&service, r#"{"level":"warn","msg":"c"}"#)
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let client = HubClient::new(url);
        let resp = client
            .search_logs(Some(&service), None, Some(100), None)
            .await
            .unwrap();
        let entries = resp.entries;
        assert!(entries.len() >= 2);
        // Find the warn entry and navigate backwards to find the error entry
        let warn_idx = entries
            .iter()
            .position(|e| {
                e.get("data")
                    .and_then(|d| d.get("msg"))
                    .and_then(|m| m.as_str())
                    == Some("c")
            })
            .expect("warn row");
        let err_idx = entries
            .iter()
            .position(|e| {
                e.get("data")
                    .and_then(|d| d.get("msg"))
                    .and_then(|m| m.as_str())
                    == Some("a")
            })
            .expect("error row");
        // Navigate backwards from warn should find error
        let result = nav_error(&client, &entries, warn_idx, "prev")
            .await
            .unwrap();
        assert_eq!(result, Some(err_idx));
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn scripted_loop_handles_key_release() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"info","msg":"x"}"#)
            .await;
        let client = HubClient::new(url);
        let keymap = Keymap::load();
        let mut entries = fetch_entries(&client).await.unwrap();
        let mut selected = 0usize;
        let mut status = String::new();
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();

        // Send a key release event (should be ignored)
        let scripted = ScriptedEvents {
            events: vec![
                Event::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    modifiers: KeyModifiers::NONE,
                    kind: KeyEventKind::Release,
                    state: crossterm::event::KeyEventState::empty(),
                }),
                key(KeyCode::Esc),
            ]
            .into_iter(),
        };

        run_tui_loop(
            &mut terminal,
            &client,
            &keymap,
            "127.0.0.1",
            3149,
            &mut entries,
            &mut selected,
            &mut status,
            scripted,
        )
        .await
        .unwrap();
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn scripted_loop_handles_non_key_events() {
        let (url, store) = spawn_test_hub().await;
        store
            .push_line("api", r#"{"level":"info","msg":"x"}"#)
            .await;
        let client = HubClient::new(url);
        let keymap = Keymap::load();
        let mut entries = fetch_entries(&client).await.unwrap();
        let mut selected = 0usize;
        let mut status = String::new();
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();

        let scripted = ScriptedEvents {
            events: vec![
                Event::Mouse(crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: 0,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                }),
                key(KeyCode::Esc),
            ]
            .into_iter(),
        };

        run_tui_loop(
            &mut terminal,
            &client,
            &keymap,
            "127.0.0.1",
            3149,
            &mut entries,
            &mut selected,
            &mut status,
            scripted,
        )
        .await
        .unwrap();
    }
}
