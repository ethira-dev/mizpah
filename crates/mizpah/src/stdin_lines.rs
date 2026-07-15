use std::future::Future;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::warn;

/// Read stdin line-by-line, skip empties, and invoke `on_line` for each.
///
/// Returns when stdin closes or a read error occurs. Fatal errors from `on_line`
/// are forwarded to the caller; EOF/read errors are logged and treated as stop.
pub async fn for_each_stdin_line<F, Fut, E>(mut on_line: F) -> Result<(), E>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<(), E>>,
{
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                if line.is_empty() {
                    continue;
                }
                on_line(line).await?;
            }
            Ok(None) => break,
            Err(err) => {
                warn!(error = %err, "failed reading stdin");
                break;
            }
        }
    }

    Ok(())
}
