use std::future::Future;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tracing::warn;

/// Read stdin line-by-line, skip empties, and invoke `on_line` for each.
///
/// Returns when stdin closes or a read error occurs. Fatal errors from `on_line`
/// are forwarded to the caller; EOF/read errors are logged and treated as stop.
///
/// Production paths use [`for_each_line`] with an explicit reader; this wrapper
/// exists for direct stdin coverage in tests.
#[cfg(test)]
pub async fn for_each_stdin_line<F, Fut, E>(on_line: F) -> Result<(), E>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<(), E>>,
{
    use tokio::io::BufReader;
    let stdin = tokio::io::stdin();
    for_each_line(BufReader::new(stdin), on_line).await
}

/// Read lines from an arbitrary buffered async reader.
pub async fn for_each_line<R, F, Fut, E>(reader: R, mut on_line: F) -> Result<(), E>
where
    R: AsyncBufRead + Unpin,
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<(), E>>,
{
    let mut lines = reader.lines();

    loop {
        match lines.next_line().await {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};
    use tokio::io::BufReader;

    #[tokio::test]
    async fn skips_empty_and_collects_lines() {
        let data = b"a\n\nb\n";
        let got = Arc::new(Mutex::new(Vec::new()));
        let got2 = Arc::clone(&got);
        for_each_line(BufReader::new(Cursor::new(&data[..])), move |line| {
            let got2 = Arc::clone(&got2);
            async move {
                got2.lock().unwrap().push(line);
                Ok::<(), ()>(())
            }
        })
        .await
        .unwrap();
        assert_eq!(*got.lock().unwrap(), vec!["a".to_string(), "b".to_string()]);
    }

    #[tokio::test]
    async fn forwards_callback_error() {
        let data = b"x\n";
        let err = for_each_line(BufReader::new(Cursor::new(&data[..])), |_line| async {
            Err("boom")
        })
        .await
        .unwrap_err();
        assert_eq!(err, "boom");
    }

    #[tokio::test]
    async fn eof_returns_ok() {
        let data = b"";
        for_each_line(BufReader::new(Cursor::new(&data[..])), |_line| async {
            Ok::<(), ()>(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn read_error_stops_cleanly() {
        struct FailRead;
        impl tokio::io::AsyncRead for FailRead {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Err(std::io::Error::other("x")))
            }
        }
        for_each_line(BufReader::new(FailRead), |_line| async { Ok::<(), ()>(()) })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn for_each_stdin_line_eof() {
        // Empty process stdin → immediate EOF.
        for_each_stdin_line(|_line| async { Ok::<(), ()>(()) })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn preserves_whitespace_only_lines() {
        let data = b"  \nline\n";
        let got = Arc::new(Mutex::new(Vec::new()));
        let got2 = Arc::clone(&got);
        for_each_line(BufReader::new(Cursor::new(&data[..])), move |line| {
            let got2 = Arc::clone(&got2);
            async move {
                got2.lock().unwrap().push(line);
                Ok::<(), ()>(())
            }
        })
        .await
        .unwrap();
        assert_eq!(
            *got.lock().unwrap(),
            vec!["  ".to_string(), "line".to_string()]
        );
    }
}
