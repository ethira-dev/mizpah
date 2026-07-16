//! Resilient shell stdout/stderr forwarder: drain stdin without backpressure.

use crate::mzp_meta::MzpMeta;
use crate::shell_attach::{self, AttachState};
use base64::Engine;
use serde::Serialize;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, warn};

const QUEUE_CAPACITY: usize = 4096;
const BATCH_MAX: usize = 128;
const BATCH_FLUSH: Duration = Duration::from_millis(50);
const HTTP_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// Control frame: `\x1eMZP\x1e<cwd>\x1e<base64(cmd)>\n`
const CTRL_PREFIX: &str = "\x1eMZP\x1e";

#[derive(Debug)]
enum ForwardMsg {
    Line(String),
    Meta { cwd: String, cmd: String },
}

#[derive(Clone, Debug)]
struct StreamMeta {
    cwd: String,
    cmd: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchBody<'a> {
    service: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cmd: Option<&'a str>,
    mzp: &'a MzpMeta,
    lines: &'a [String],
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

/// Forward stdin lines to the hub while respecting live attach state.
///
/// Never blocks the shell on network I/O: full queues drop lines.
pub async fn run_shell_forward(tty_service: String) -> Result<(), String> {
    let (tx, rx) = mpsc::channel::<ForwardMsg>(QUEUE_CAPACITY);

    let reader = tokio::spawn(async move {
        drain_stdin(tx).await;
    });

    let worker = tokio::spawn(async move {
        forward_worker(rx, tty_service).await;
    });

    let _ = reader.await;
    let _ = worker.await;
    Ok(())
}

async fn drain_stdin(tx: mpsc::Sender<ForwardMsg>) {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut dropped: u64 = 0;

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.is_empty() {
                    continue;
                }
                let msg = match parse_control_frame(&line) {
                    Some((cwd, cmd)) => ForwardMsg::Meta { cwd, cmd },
                    None => ForwardMsg::Line(line),
                };
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

async fn forward_worker(mut rx: mpsc::Receiver<ForwardMsg>, tty_service: String) {
    let client = match reqwest::Client::builder().timeout(HTTP_TIMEOUT).build() {
        Ok(c) => c,
        Err(err) => {
            warn!(error = %err, "shell forward: http client failed");
            while rx.recv().await.is_some() {}
            return;
        }
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
    if buf.is_empty() {
        return;
    }

    let state = shell_attach::load_state().unwrap_or_default();
    if !state.enabled {
        buf.clear();
        return;
    }

    let service = resolve_service(&state, &meta.cwd);
    let url = format!(
        "{}/api/ingest/batch",
        shell_attach::hub_url(&state.host, state.port)
    );

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
            *backoff = Duration::from_millis(100);
        }
        Err(BatchError::Disconnected) => {
            warn!(%service, "shell forward: service disconnected; dropping batch");
            *dropped_since_ok = 0;
            *backoff = Duration::from_millis(100);
        }
        Err(BatchError::Other(err)) => {
            warn!(error = %err, %service, "shell forward: batch ingest failed");
            *dropped_since_ok = dropped_since_ok.saturating_add(n);
            tokio::time::sleep(*backoff).await;
            *backoff = (*backoff * 2).min(MAX_BACKOFF);
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

#[derive(Debug)]
enum BatchError {
    Disconnected,
    Other(String),
}

async fn post_batch(
    client: &reqwest::Client,
    url: &str,
    service: &str,
    cmd: Option<&str>,
    mzp: &MzpMeta,
    lines: &[String],
) -> Result<(), BatchError> {
    if lines.is_empty() {
        return Ok(());
    }
    let body = BatchBody {
        service,
        cmd,
        mzp,
        lines,
    };
    let resp = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| BatchError::Other(e.to_string()))?;
    if resp.status() == reqwest::StatusCode::CONFLICT {
        return Err(BatchError::Disconnected);
    }
    if !resp.status().is_success() {
        return Err(BatchError::Other(format!("status {}", resp.status())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn resolve_service_prefers_state() {
        let state = AttachState {
            enabled: true,
            service: Some("api".into()),
            host: "127.0.0.1".into(),
            port: 1738,
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
    fn parse_control_frame_decodes_cmd() {
        let cmd = "npm test -- --watch";
        let b64 = base64::engine::general_purpose::STANDARD.encode(cmd.as_bytes());
        let line = format!("\x1eMZP\x1e/Users/me/app\x1e{b64}");
        let (cwd, parsed) = parse_control_frame(&line).expect("frame");
        assert_eq!(cwd, "/Users/me/app");
        assert_eq!(parsed, cmd);
    }

    #[test]
    fn parse_control_frame_empty_cmd() {
        let line = "\x1eMZP\x1e/tmp\x1e";
        let (cwd, cmd) = parse_control_frame(line).expect("frame");
        assert_eq!(cwd, "/tmp");
        assert_eq!(cmd, "");
    }

    #[test]
    fn parse_control_frame_rejects_normal_lines() {
        assert!(parse_control_frame(r#"{"level":"info"}"#).is_none());
        assert!(parse_control_frame("plain text").is_none());
        assert!(parse_control_frame("\x1eMZP\x1e").is_none());
    }
}
