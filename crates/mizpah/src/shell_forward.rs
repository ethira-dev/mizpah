//! Resilient shell stdout/stderr forwarder: drain stdin without backpressure.

use crate::hub;
use crate::ingest_forward::{
    self, drain_on_client_error, post_batch, reset_backoff, sleep_backoff, BatchError, BATCH_FLUSH,
    BATCH_MAX, QUEUE_CAPACITY,
};
use crate::mzp_meta::MzpMeta;
use crate::shell_attach::{self, AttachState};
use base64::Engine;
use std::future::Future;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Control frame: `\x1eMZP\x1e<cwd>\x1e<base64(cmd)>\n`
const CTRL_PREFIX: &str = "\x1eMZP\x1e";

#[derive(Debug)]
pub(crate) enum ForwardMsg {
    Line(String),
    Meta { cwd: String, cmd: String },
}

#[derive(Clone, Debug)]
struct StreamMeta {
    cwd: String,
    cmd: Option<String>,
}

/// Parse a shell-attach control frame into `(cwd, cmd)`.
pub fn parse_control_frame(line: &str) -> Option<(String, String)> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let rest = line.strip_prefix(CTRL_PREFIX)?;
    let (cwd, b64) = rest.split_once('\x1e')?;
    if cwd.is_empty() {
        return None;
    }
    let cmd = if b64.is_empty() {
        String::new()
    } else {
        decode_cmd_b64(b64)?
    };
    Some((cwd.to_string(), cmd))
}

fn decode_cmd_b64(b64: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    String::from_utf8(bytes).ok()
}

fn classify_line(line: &str) -> ForwardMsg {
    match parse_control_frame(line) {
        Some((cwd, cmd)) => ForwardMsg::Meta { cwd, cmd },
        None => ForwardMsg::Line(line.to_string()),
    }
}

/// Forward stdin lines to the hub while respecting live attach state.
///
/// Never blocks the shell on network I/O: full queues drop lines.
pub async fn run_shell_forward(tty_service: String) -> Result<(), String> {
    run_shell_forward_with_drain(tty_service, drain_stdin).await
}

pub(crate) async fn run_shell_forward_with_drain<F, Fut>(
    tty_service: String,
    drain: F,
) -> Result<(), String>
where
    F: FnOnce(mpsc::Sender<ForwardMsg>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<ForwardMsg>(QUEUE_CAPACITY);

    let reader = tokio::spawn(drain(tx));

    let worker = tokio::spawn(async move {
        forward_worker(rx, tty_service).await;
    });

    let _ = reader.await;
    let _ = worker.await;
    Ok(())
}

async fn drain_stdin(tx: mpsc::Sender<ForwardMsg>) {
    drain_stdin_with(tx, tokio::io::stdin()).await;
}

async fn drain_stdin_with(tx: mpsc::Sender<ForwardMsg>, stdin: impl tokio::io::AsyncRead + Unpin) {
    let mut lines = BufReader::new(stdin).lines();
    drain_lines(&mut lines, tx).await;
}

async fn drain_lines<B: AsyncBufReadExt + Unpin>(
    lines: &mut tokio::io::Lines<B>,
    tx: mpsc::Sender<ForwardMsg>,
) {
    let mut dropped: u64 = 0;

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.is_empty() {
                    continue;
                }
                let msg = classify_line(&line);
                match tx.try_send(msg) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        dropped = dropped.saturating_add(1);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
            Ok(None) => break,
            Err(err) => {
                warn!(error = %err, "shell forward: stdin read failed");
                break;
            }
        }
    }

    if dropped > 0 {
        debug!(
            dropped,
            "shell forward: dropped lines due to full queue while draining"
        );
    }
    // drop tx → worker sees EOF
}

async fn forward_worker(rx: mpsc::Receiver<ForwardMsg>, tty_service: String) {
    forward_worker_with_client(rx, tty_service, None).await;
}

async fn forward_worker_with_client(
    mut rx: mpsc::Receiver<ForwardMsg>,
    tty_service: String,
    client_override: Option<Result<reqwest::Client, String>>,
) {
    let client = match client_override {
        Some(result) => match result {
            Ok(c) => c,
            Err(err) => {
                drain_on_client_error("shell forward", &err, &mut rx).await;
                return;
            }
        },
        None => match ingest_forward::http_client() {
            Ok(c) => c,
            Err(err) => {
                drain_on_client_error("shell forward", &err, &mut rx).await;
                return;
            }
        },
    };

    let receiver = MzpMeta::capture();
    let mut meta = StreamMeta {
        cwd: tty_service,
        cmd: None,
    };
    let mut dropped_since_ok: u64 = 0;
    let mut backoff = Duration::from_millis(100);
    let mut buf: Vec<String> = Vec::new();

    loop {
        let msg = if buf.is_empty() {
            match rx.recv().await {
                Some(m) => m,
                None => break,
            }
        } else {
            let deadline = tokio::time::Instant::now() + BATCH_FLUSH;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(m)) => m,
                Ok(None) => {
                    flush_batch(
                        &client,
                        &mut buf,
                        &meta,
                        &receiver,
                        &mut dropped_since_ok,
                        &mut backoff,
                    )
                    .await;
                    break;
                }
                Err(_) => {
                    flush_batch(
                        &client,
                        &mut buf,
                        &meta,
                        &receiver,
                        &mut dropped_since_ok,
                        &mut backoff,
                    )
                    .await;
                    continue;
                }
            }
        };

        match msg {
            ForwardMsg::Meta { cwd, cmd } => {
                if !buf.is_empty() {
                    flush_batch(
                        &client,
                        &mut buf,
                        &meta,
                        &receiver,
                        &mut dropped_since_ok,
                        &mut backoff,
                    )
                    .await;
                }
                meta.cwd = cwd;
                meta.cmd = if cmd.is_empty() { None } else { Some(cmd) };
            }
            ForwardMsg::Line(line) => {
                buf.push(line);
                if buf.len() >= BATCH_MAX {
                    flush_batch(
                        &client,
                        &mut buf,
                        &meta,
                        &receiver,
                        &mut dropped_since_ok,
                        &mut backoff,
                    )
                    .await;
                }
            }
        }
    }
}

async fn flush_batch(
    client: &reqwest::Client,
    buf: &mut Vec<String>,
    meta: &StreamMeta,
    receiver: &MzpMeta,
    dropped_since_ok: &mut u64,
    backoff: &mut Duration,
) {
    let state = shell_attach::load_state().unwrap_or_default();
    flush_batch_for_state(
        client,
        &state,
        buf,
        meta,
        receiver,
        dropped_since_ok,
        backoff,
    )
    .await;
}

async fn flush_batch_for_state(
    client: &reqwest::Client,
    state: &AttachState,
    buf: &mut Vec<String>,
    meta: &StreamMeta,
    receiver: &MzpMeta,
    dropped_since_ok: &mut u64,
    backoff: &mut Duration,
) {
    if buf.is_empty() {
        return;
    }
    if !state.enabled {
        buf.clear();
        return;
    }

    let service = resolve_service(state, &meta.cwd);
    let url = format!("{}/api/ingest/batch", hub::hub_url(&state.host, state.port));

    if *dropped_since_ok > 0 {
        let n = *dropped_since_ok;
        buf.push(format!(
            "{{\"level\":\"warn\",\"msg\":\"mizpah shell attach dropped {n} lines due to backpressure\"}}"
        ));
    }

    let n = buf.len() as u64;
    let cmd = meta.cmd.as_deref();
    let mzp = receiver.clone().with_cwd(meta.cwd.clone());
    match post_batch(client, &url, &service, cmd, &mzp, buf).await {
        Ok(()) => {
            *dropped_since_ok = 0;
            reset_backoff(backoff);
        }
        Err(BatchError::Disconnected) => {
            warn!(%service, "shell forward: service disconnected; dropping batch");
            *dropped_since_ok = 0;
            reset_backoff(backoff);
        }
        Err(BatchError::Other(err)) => {
            warn!(error = %err, %service, "shell forward: batch ingest failed");
            *dropped_since_ok = dropped_since_ok.saturating_add(n);
            sleep_backoff(backoff).await;
        }
    }
    buf.clear();
}

fn resolve_service(state: &AttachState, fallback_service: &str) -> String {
    if let Some(s) = state.service.as_deref() {
        let t = s.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    let t = fallback_service.trim();
    if t.is_empty() {
        "unknown".into()
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest_forward::BATCH_MAX;
    use crate::shell_attach::{save_state, AttachState};
    use crate::test_support::env_lock;
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::Router;
    use base64::Engine;
    use std::io;
    use std::net::SocketAddr;
    use tokio::io::BufReader;
    use tokio::net::TcpListener;

    fn attach_state_for_hub(hub_url: &str, enabled: bool, service: Option<&str>) -> AttachState {
        let url = url::Url::parse(hub_url).unwrap();
        AttachState {
            enabled,
            service: service.map(str::to_string),
            host: url.host_str().unwrap_or("127.0.0.1").into(),
            port: url.port().unwrap_or(80),
        }
    }

    #[test]
    fn resolve_service_prefers_state() {
        let state = AttachState {
            enabled: true,
            service: Some("api".into()),
            host: "127.0.0.1".into(),
            port: 3149,
        };
        assert_eq!(resolve_service(&state, "/Users/me/app"), "api");
    }

    #[test]
    fn resolve_service_falls_back_to_cwd_arg() {
        let state = AttachState::default();
        assert_eq!(
            resolve_service(&state, "/Users/me/project"),
            "/Users/me/project"
        );
    }

    #[test]
    fn resolve_service_empty_fallbacks_to_unknown() {
        let state = AttachState {
            service: Some("   ".into()),
            ..AttachState::default()
        };
        assert_eq!(resolve_service(&state, "  "), "unknown");
    }

    #[test]
    fn resolve_service_whitespace_state_service_uses_fallback() {
        let state = AttachState {
            service: Some("\t".into()),
            ..AttachState::default()
        };
        assert_eq!(resolve_service(&state, "my-service"), "my-service");
    }

    #[test]
    fn parse_control_frame_decodes_cmd() {
        let cmd = "npm test -- --watch";
        let b64 = base64::engine::general_purpose::STANDARD.encode(cmd.as_bytes());
        let line = format!("\x1eMZP\x1e/Users/me/app\x1e{b64}");
        let (cwd, parsed) = parse_control_frame(&line).expect("frame");
        assert_eq!(cwd, "/Users/me/app");
        assert_eq!(parsed, cmd);
    }

    #[test]
    fn parse_control_frame_strips_cr_suffix() {
        let line = "\x1eMZP\x1e/tmp\x1e\r";
        let (cwd, cmd) = parse_control_frame(line).expect("frame");
        assert_eq!(cwd, "/tmp");
        assert_eq!(cmd, "");
    }

    #[test]
    fn parse_control_frame_empty_cmd() {
        let line = "\x1eMZP\x1e/tmp\x1e";
        let (cwd, cmd) = parse_control_frame(line).expect("frame");
        assert_eq!(cwd, "/tmp");
        assert_eq!(cmd, "");
    }

    #[test]
    fn parse_control_frame_rejects_invalid_b64() {
        let line = "\x1eMZP\x1e/tmp\x1enot!!!valid!!!";
        assert!(parse_control_frame(line).is_none());
    }

    #[test]
    fn decode_cmd_b64_invalid_utf8() {
        // Valid base64 for bytes that are not UTF-8
        let invalid_utf8_b64 = base64::engine::general_purpose::STANDARD.encode([0xFF_u8, 0xFE_u8]);
        let line = format!("\x1eMZP\x1e/tmp\x1e{invalid_utf8_b64}");
        assert!(parse_control_frame(&line).is_none());
    }

    #[test]
    fn parse_control_frame_rejects_empty_cwd() {
        assert!(parse_control_frame("\x1eMZP\x1e\x1e").is_none());
    }

    #[test]
    fn parse_control_frame_rejects_normal_lines() {
        assert!(parse_control_frame(r#"{"level":"info"}"#).is_none());
        assert!(parse_control_frame("plain text").is_none());
        assert!(parse_control_frame("\x1eMZP\x1e").is_none());
    }

    #[test]
    fn classify_line_control_vs_log() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"ls");
        let frame = format!("\x1eMZP\x1e/cwd\x1e{b64}");
        match classify_line(&frame) {
            ForwardMsg::Meta { cwd, cmd } => {
                assert_eq!(cwd, "/cwd");
                assert_eq!(cmd, "ls");
            }
            _ => panic!("expected meta"),
        }
        match classify_line("hello") {
            ForwardMsg::Line(s) => assert_eq!(s, "hello"),
            _ => panic!("expected line"),
        }
    }

    #[tokio::test]
    async fn drain_lines_skips_empty_and_parses_control() {
        let (tx, mut rx) = mpsc::channel(8);
        let reader = BufReader::new(b"\n\x1eMZP\x1e/tmp\x1e\nlog line\n".as_slice());
        let mut lines = reader.lines();
        drain_lines(&mut lines, tx).await;
        assert!(matches!(rx.recv().await, Some(ForwardMsg::Meta { .. })));
        assert!(matches!(rx.recv().await, Some(ForwardMsg::Line(_))));
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn drain_lines_drops_on_full_queue() {
        let (tx, mut rx) = mpsc::channel(1);
        let reader = BufReader::new(b"a\nb\nc\n".as_slice());
        let mut lines = reader.lines();
        drain_lines(&mut lines, tx).await;
        let first = rx.recv().await;
        assert!(first.is_some());
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn drain_lines_read_error_stops() {
        use std::pin::Pin;
        use std::task::{Context, Poll};
        use tokio::io::{AsyncRead, ReadBuf};

        struct FailReader;
        impl AsyncRead for FailReader {
            fn poll_read(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Poll::Ready(Err(io::Error::other("read fail")))
            }
        }

        let (tx, _rx) = mpsc::channel(4);
        let reader = BufReader::new(FailReader);
        let mut lines = reader.lines();
        drain_lines(&mut lines, tx).await;
    }

    #[tokio::test]
    async fn drain_lines_stops_when_channel_closed() {
        let (tx, _rx) = mpsc::channel::<ForwardMsg>(1);
        drop(_rx);
        let reader = BufReader::new(b"line\n".as_slice());
        let mut lines = reader.lines();
        drain_lines(&mut lines, tx).await;
    }

    #[tokio::test]
    async fn flush_batch_noop_when_empty() {
        let client = ingest_forward::http_client().unwrap();
        let mut buf = Vec::new();
        let meta = StreamMeta {
            cwd: "svc".into(),
            cmd: None,
        };
        let receiver = MzpMeta::capture();
        let mut dropped = 0u64;
        let mut backoff = Duration::from_millis(100);
        flush_batch(
            &client,
            &mut buf,
            &meta,
            &receiver,
            &mut dropped,
            &mut backoff,
        )
        .await;
        assert!(buf.is_empty());
    }

    // Real TCP / reqwest sockets are unsupported under Miri.
    #[cfg(not(miri))]
    mod hub {
        use super::*;

        async fn spawn_error_hub(status: StatusCode) -> String {
            let app = Router::new().route(
                "/api/ingest/batch",
                post(move || async move { (status, "nope") }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr: SocketAddr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            format!("http://{addr}")
        }

        #[tokio::test]
        async fn flush_batch_clears_when_attach_disabled() {
            let _guard = env_lock();
            let dir = std::env::temp_dir().join(format!("mizpah-fwd-off-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let old = std::env::var_os("MIZPAH_CONFIG_DIR");
            std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
            save_state(&AttachState {
                enabled: false,
                service: None,
                host: "127.0.0.1".into(),
                port: 3149,
            })
            .unwrap();

            let client = ingest_forward::http_client().unwrap();
            let mut buf = vec!["line".into()];
            let meta = StreamMeta {
                cwd: "svc".into(),
                cmd: None,
            };
            let receiver = MzpMeta::capture();
            let mut dropped = 0u64;
            let mut backoff = Duration::from_millis(100);
            flush_batch(
                &client,
                &mut buf,
                &meta,
                &receiver,
                &mut dropped,
                &mut backoff,
            )
            .await;
            assert!(buf.is_empty());

            match old {
                Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
                None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn flush_batch_success_posts_to_hub() {
            let (hub_url, store) = crate::test_support::spawn_test_hub().await;
            let _guard = env_lock();
            let dir = std::env::temp_dir().join(format!("mizpah-fwd-ok-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let old = std::env::var_os("MIZPAH_CONFIG_DIR");
            std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
            let url = url::Url::parse(&hub_url).unwrap();
            let port = url.port().unwrap_or(80);
            save_state(&AttachState {
                enabled: true,
                service: Some("shell-svc".into()),
                host: url.host_str().unwrap_or("127.0.0.1").into(),
                port,
            })
            .unwrap();

            let client = ingest_forward::http_client().unwrap();
            let mut buf = vec![r#"{"msg":"hi"}"#.into()];
            let meta = StreamMeta {
                cwd: "/tmp/project".into(),
                cmd: Some("npm test".into()),
            };
            let receiver = MzpMeta::capture();
            let mut dropped = 5u64;
            let mut backoff = Duration::from_millis(100);
            flush_batch(
                &client,
                &mut buf,
                &meta,
                &receiver,
                &mut dropped,
                &mut backoff,
            )
            .await;
            assert_eq!(dropped, 0);
            assert!(buf.is_empty());
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(store.stats().await.count >= 1);

            match old {
                Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
                None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn flush_batch_disconnect_resets_dropped() {
            let (hub_url, _store) = crate::test_support::spawn_test_hub().await;
            let client = ingest_forward::http_client().unwrap();
            let disconnect_url = format!("{hub_url}/api/services/disconnect");
            client
                .post(&disconnect_url)
                .json(&serde_json::json!({"service": "shell-svc"}))
                .send()
                .await
                .unwrap();

            let _guard = env_lock();
            let dir = std::env::temp_dir().join(format!("mizpah-fwd-disc-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let old = std::env::var_os("MIZPAH_CONFIG_DIR");
            std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
            let url = url::Url::parse(&hub_url).unwrap();
            save_state(&AttachState {
                enabled: true,
                service: Some("shell-svc".into()),
                host: url.host_str().unwrap_or("127.0.0.1").into(),
                port: url.port().unwrap_or(80),
            })
            .unwrap();

            let mut buf = vec!["line".into()];
            let meta = StreamMeta {
                cwd: "cwd".into(),
                cmd: None,
            };
            let receiver = MzpMeta::capture();
            let mut dropped = 3u64;
            let mut backoff = Duration::from_millis(100);
            flush_batch(
                &client,
                &mut buf,
                &meta,
                &receiver,
                &mut dropped,
                &mut backoff,
            )
            .await;
            assert_eq!(dropped, 0);

            match old {
                Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
                None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn flush_batch_other_error_increments_dropped() {
            tokio::time::pause();
            let err_url = spawn_error_hub(StatusCode::INTERNAL_SERVER_ERROR).await;
            let state = attach_state_for_hub(&err_url, true, None);
            let client = ingest_forward::http_client().unwrap();
            let mut buf = vec!["a".into(), "b".into()];
            let meta = StreamMeta {
                cwd: "my-svc".into(),
                cmd: Some("cmd".into()),
            };
            let receiver = MzpMeta::capture();
            let mut dropped = 0u64;
            let mut backoff = Duration::from_millis(100);
            flush_batch_for_state(
                &client,
                &state,
                &mut buf,
                &meta,
                &receiver,
                &mut dropped,
                &mut backoff,
            )
            .await;
            assert_eq!(dropped, 2);
            assert!(buf.is_empty());
            assert_eq!(backoff, Duration::from_millis(200));
        }

        #[tokio::test]
        async fn forward_worker_with_disabled_attach_drops_lines() {
            let (hub_url, store) = crate::test_support::spawn_test_hub().await;
            let _guard = env_lock();
            let dir = std::env::temp_dir().join(format!("mizpah-fwd-drop-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let old = std::env::var_os("MIZPAH_CONFIG_DIR");
            std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
            save_state(&attach_state_for_hub(&hub_url, false, None)).unwrap();

            let (tx, rx) = mpsc::channel(4);
            tx.send(ForwardMsg::Line("dropped".into())).await.unwrap();
            drop(tx);
            forward_worker(rx, "svc".into()).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(store.stats().await.count, 0);

            match old {
                Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
                None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn forward_worker_flushes_lines_and_meta() {
            let (hub_url, store) = crate::test_support::spawn_test_hub().await;
            let _guard = env_lock();
            let dir = std::env::temp_dir().join(format!("mizpah-fwd-wk-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let old = std::env::var_os("MIZPAH_CONFIG_DIR");
            std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
            let url = url::Url::parse(&hub_url).unwrap();
            save_state(&AttachState {
                enabled: true,
                service: Some("wk".into()),
                host: url.host_str().unwrap_or("127.0.0.1").into(),
                port: url.port().unwrap_or(80),
            })
            .unwrap();

            let (tx, rx) = mpsc::channel(16);
            tx.send(ForwardMsg::Meta {
                cwd: "/proj".into(),
                cmd: "build".into(),
            })
            .await
            .unwrap();
            for i in 0..3 {
                tx.send(ForwardMsg::Line(format!("line{i}"))).await.unwrap();
            }
            drop(tx);

            forward_worker(rx, "/fallback".into()).await;
            tokio::time::sleep(Duration::from_millis(80)).await;
            assert!(store.stats().await.count >= 1);

            match old {
                Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
                None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn forward_worker_batch_max_flush() {
            let (hub_url, store) = crate::test_support::spawn_test_hub().await;
            let _guard = env_lock();
            let dir = std::env::temp_dir().join(format!("mizpah-fwd-max-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let old = std::env::var_os("MIZPAH_CONFIG_DIR");
            std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
            let url = url::Url::parse(&hub_url).unwrap();
            save_state(&AttachState {
                enabled: true,
                service: Some("batch".into()),
                host: url.host_str().unwrap_or("127.0.0.1").into(),
                port: url.port().unwrap_or(80),
            })
            .unwrap();

            let (tx, rx) = mpsc::channel(BATCH_MAX + 4);
            for i in 0..BATCH_MAX {
                tx.send(ForwardMsg::Line(format!("l{i}"))).await.unwrap();
            }
            tx.send(ForwardMsg::Line("extra".into())).await.unwrap();
            drop(tx);

            forward_worker(rx, "svc".into()).await;
            tokio::time::sleep(Duration::from_millis(80)).await;
            assert!(store.stats().await.count >= BATCH_MAX as u64);

            match old {
                Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
                None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn forward_worker_drains_on_empty_meta_cmd() {
            let (hub_url, _store) = crate::test_support::spawn_test_hub().await;
            let _guard = env_lock();
            let dir = std::env::temp_dir().join(format!("mizpah-fwd-meta-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let old = std::env::var_os("MIZPAH_CONFIG_DIR");
            std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
            let url = url::Url::parse(&hub_url).unwrap();
            save_state(&AttachState {
                enabled: true,
                service: Some("m".into()),
                host: url.host_str().unwrap_or("127.0.0.1").into(),
                port: url.port().unwrap_or(80),
            })
            .unwrap();

            let (tx, rx) = mpsc::channel(4);
            tx.send(ForwardMsg::Meta {
                cwd: "/x".into(),
                cmd: String::new(),
            })
            .await
            .unwrap();
            tx.send(ForwardMsg::Line("after-meta".into()))
                .await
                .unwrap();
            drop(tx);
            forward_worker(rx, "fb".into()).await;

            match old {
                Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
                None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn flush_batch_reads_attach_state_from_env() {
            let (hub_url, store) = crate::test_support::spawn_test_hub().await;
            let _guard = env_lock();
            let dir = std::env::temp_dir().join(format!("mizpah-fwd-env-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let old = std::env::var_os("MIZPAH_CONFIG_DIR");
            std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
            save_state(&attach_state_for_hub(&hub_url, true, Some("env-svc"))).unwrap();

            let client = ingest_forward::http_client().unwrap();
            let mut buf = vec!["via flush_batch".into()];
            let meta = StreamMeta {
                cwd: "/tmp".into(),
                cmd: None,
            };
            let receiver = MzpMeta::capture();
            let mut dropped = 0u64;
            let mut backoff = Duration::from_millis(100);
            flush_batch(
                &client,
                &mut buf,
                &meta,
                &receiver,
                &mut dropped,
                &mut backoff,
            )
            .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(store.stats().await.count >= 1);

            match old {
                Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
                None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn forward_worker_flushes_on_channel_close() {
            let (hub_url, store) = crate::test_support::spawn_test_hub().await;
            let _guard = env_lock();
            let dir = std::env::temp_dir().join(format!("mizpah-fwd-eof-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let old = std::env::var_os("MIZPAH_CONFIG_DIR");
            std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
            save_state(&attach_state_for_hub(&hub_url, true, Some("eof"))).unwrap();

            let (tx, rx) = mpsc::channel(4);
            tx.send(ForwardMsg::Line("eof-line".into())).await.unwrap();
            drop(tx);
            forward_worker(rx, "svc".into()).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(store.stats().await.count >= 1);

            match old {
                Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
                None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn forward_worker_times_out_batch_flush() {
            let (hub_url, store) = crate::test_support::spawn_test_hub().await;
            let _guard = env_lock();
            let dir = std::env::temp_dir().join(format!("mizpah-fwd-time-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let old = std::env::var_os("MIZPAH_CONFIG_DIR");
            std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
            save_state(&attach_state_for_hub(&hub_url, true, Some("time"))).unwrap();

            let (tx, rx) = mpsc::channel(4);
            tx.send(ForwardMsg::Line("wait-flush".into()))
                .await
                .unwrap();
            let worker = tokio::spawn(forward_worker(rx, "svc".into()));
            tokio::time::sleep(crate::ingest_forward::BATCH_FLUSH + Duration::from_millis(80))
                .await;
            drop(tx);
            worker.await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(store.stats().await.count >= 1);

            match old {
                Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
                None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn flush_batch_corrupt_attach_state_drops_lines() {
            let _guard = env_lock();
            let dir = std::env::temp_dir().join(format!("mizpah-fwd-bad-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let old = std::env::var_os("MIZPAH_CONFIG_DIR");
            std::env::set_var("MIZPAH_CONFIG_DIR", &dir);
            std::fs::write(dir.join("attach.json"), "{bad").unwrap();

            let client = ingest_forward::http_client().unwrap();
            let mut buf = vec!["line".into()];
            let meta = StreamMeta {
                cwd: "svc".into(),
                cmd: None,
            };
            let receiver = MzpMeta::capture();
            let mut dropped = 0u64;
            let mut backoff = Duration::from_millis(100);
            flush_batch(
                &client,
                &mut buf,
                &meta,
                &receiver,
                &mut dropped,
                &mut backoff,
            )
            .await;
            assert!(buf.is_empty());

            match old {
                Some(v) => std::env::set_var("MIZPAH_CONFIG_DIR", v),
                None => std::env::remove_var("MIZPAH_CONFIG_DIR"),
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn drain_stdin_with_empty_input() {
            let (tx, _rx) = mpsc::channel(4);
            drain_stdin_with(tx, &b""[..]).await;
        }

        #[tokio::test]
        async fn run_shell_forward_with_drain_completes() {
            run_shell_forward_with_drain("svc".into(), |tx| async move {
                drop(tx);
            })
            .await
            .unwrap();
        }

        #[tokio::test]
        async fn forward_worker_drains_on_client_error() {
            let (tx, rx) = mpsc::channel(4);
            tx.send(ForwardMsg::Line("x".into())).await.unwrap();
            drop(tx);
            forward_worker_with_client(rx, "svc".into(), Some(Err("build failed".into()))).await;
        }
    }
}
