//! `mzp run -- <cmd…>` — ensure hub, stream child output, exit with child code.

use crate::hub;
use crate::ingest_forward;
use crate::mzp_meta::MzpMeta;
use crate::shell_attach;
use serde_json::json;
use std::process::Stdio;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct RunOpts {
    pub service: String,
    pub host: String,
    pub port: u16,
    pub no_open: bool,
    pub args: Vec<String>,
}

async fn forward_lines<R: AsyncRead + Unpin>(
    reader: R,
    client: &reqwest::Client,
    url: &str,
    service: &str,
    cmd: &str,
    mzp: &MzpMeta,
) {
    let mut lines = BufReader::new(reader).lines();
    let mut batch = Vec::new();
    while let Ok(Some(line)) = lines.next_line().await {
        batch.push(line);
        if batch.len() >= 32 {
            let _ = ingest_forward::post_batch(client, url, service, Some(cmd), mzp, &batch).await;
            batch.clear();
        }
    }
    if !batch.is_empty() {
        let _ = ingest_forward::post_batch(client, url, service, Some(cmd), mzp, &batch).await;
    }
}

/// Run a command, forwarding stdout/stderr lines into the hub.
pub async fn run_command(opts: RunOpts) -> i32 {
    if opts.args.is_empty() {
        eprintln!("error: provide a command after `--`, e.g. `mzp run -- npm test`");
        return 2;
    }

    if let Err(e) = hub::ensure_hub(&opts.host, opts.port, None, false).await {
        eprintln!("error: could not ensure hub: {e}");
        return 1;
    }

    if !opts.no_open {
        let _ = shell_attach::run_open(opts.host.clone(), opts.port).await;
    }

    let cmd_display = opts.args.join(" ");
    let program = &opts.args[0];
    let args = &opts.args[1..];

    let mut child = match Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to spawn {program}: {e}");
            return 1;
        }
    };

    let client = match ingest_forward::http_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: http client: {e}");
            return 1;
        }
    };
    let url = format!("{}/api/ingest/batch", hub::hub_url(&opts.host, opts.port));
    let mzp = MzpMeta::capture();
    let started = Instant::now();

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let out_task = {
        let client = client.clone();
        let url = url.clone();
        let service = opts.service.clone();
        let cmd = cmd_display.clone();
        let mzp = mzp.clone();
        async move {
            if let Some(out) = stdout {
                forward_lines(out, &client, &url, &service, &cmd, &mzp).await;
            }
        }
    };
    let err_task = {
        let client = client.clone();
        let url = url.clone();
        let service = opts.service.clone();
        let cmd = cmd_display.clone();
        let mzp = mzp.clone();
        async move {
            if let Some(err) = stderr {
                forward_lines(err, &client, &url, &service, &cmd, &mzp).await;
            }
        }
    };

    let (_, _) = tokio::join!(out_task, err_task);
    let status = match child.wait().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: waiting for child: {e}");
            return 1;
        }
    };

    let code = status.code().unwrap_or(1);
    let duration_ms = started.elapsed().as_millis() as u64;
    let exit_line = json!({
        "kind": "process.exit",
        "code": code,
        "duration_ms": duration_ms,
        "cmd": cmd_display,
    })
    .to_string();
    let _ = ingest_forward::post_batch(
        &client,
        &url,
        &opts.service,
        Some(&cmd_display),
        &mzp,
        &[exit_line],
    )
    .await;

    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::CompiledQuery;
    use crate::test_support::{env_lock, spawn_test_hub};

    #[tokio::test]
    async fn empty_args_returns_2() {
        let code = run_command(RunOpts {
            service: "test".into(),
            host: "127.0.0.1".into(),
            port: 1,
            no_open: true,
            args: vec![],
        })
        .await;
        assert_eq!(code, 2);
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn runs_echo_and_posts_exit_event() {
        let _guard = env_lock();
        std::env::set_var("MIZPAH_TEST_SKIP_BROWSER", "1");

        let (url, store) = spawn_test_hub().await;
        let parsed = url::Url::parse(&url).unwrap();
        let host = parsed.host_str().unwrap().to_string();
        let port = parsed.port().unwrap();

        let code = run_command(RunOpts {
            service: "run-test".into(),
            host,
            port,
            no_open: true,
            args: vec!["/bin/echo".into(), "hello-from-run".into()],
        })
        .await;
        assert_eq!(code, 0);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let (entries, _) = store
            .query_logs(
                Some("run-test"),
                None,
                50,
                &CompiledQuery::MatchAll,
                None,
                None,
            )
            .await;
        assert!(
            entries.iter().any(|e| {
                let s = e.data.to_string();
                s.contains("hello-from-run")
            }),
            "expected echoed line in store, got: {entries:?}"
        );
        assert!(
            entries.iter().any(|e| {
                e.data.get("kind").and_then(|v| v.as_str()) == Some("process.exit")
                    && e.data.get("code").and_then(|v| v.as_i64()) == Some(0)
            }),
            "expected process.exit event, got: {entries:?}"
        );

        std::env::remove_var("MIZPAH_TEST_SKIP_BROWSER");
    }
}
