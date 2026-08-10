use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub async fn read_message<R>(reader: &mut R) -> Result<Option<Value>>
where
    R: AsyncBufRead + Unpin,
{
    let mut content_length = None;
    loop {
        let mut header = String::new();
        let bytes = reader
            .read_line(&mut header)
            .await
            .context("failed to read LSP header")?;
        if bytes == 0 {
            return Ok(None);
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        if let Some(value) = header
            .strip_prefix("Content-Length:")
            .or_else(|| header.strip_prefix("content-length:"))
        {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("invalid Content-Length header")?,
            );
        }
    }

    let Some(content_length) = content_length else {
        bail!("LSP message did not include Content-Length");
    };
    let mut body = vec![0; content_length];
    reader
        .read_exact(&mut body)
        .await
        .context("failed to read LSP message body")?;
    serde_json::from_slice(&body)
        .context("failed to decode LSP JSON")
        .map(Some)
}

pub async fn write_message<W>(writer: &mut W, message: &Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(message).context("failed to encode LSP JSON")?;
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await
        .context("failed to write LSP header")?;
    writer
        .write_all(&body)
        .await
        .context("failed to write LSP body")?;
    writer.flush().await.context("failed to flush LSP message")
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::{BufReader, duplex};

    use super::{read_message, write_message};

    #[tokio::test]
    async fn round_trips_a_message() {
        let (mut writer, reader) = duplex(1024);
        let expected = json!({"jsonrpc": "2.0", "id": 7, "method": "initialize"});
        let write = tokio::spawn({
            let expected = expected.clone();
            async move { write_message(&mut writer, &expected).await }
        });
        let mut reader = BufReader::new(reader);
        let actual = read_message(&mut reader).await.unwrap().unwrap();
        write.await.unwrap().unwrap();
        assert_eq!(actual, expected);
    }
}
