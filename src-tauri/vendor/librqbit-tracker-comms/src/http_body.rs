use anyhow::{Context, ensure};

/// Bound decoded bytes while streaming, regardless of Content-Length or transfer
/// encoding. Never allocate based on an untrusted advertised length.
pub async fn read_bounded_http_body(
    mut response: reqwest::Response,
    limit: usize,
) -> anyhow::Result<Vec<u8>> {
    ensure!(
        response.content_length().is_none_or(|n| n <= limit as u64),
        "HTTP body exceeds size limit"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.without_url())? {
        ensure!(
            chunk.len() <= limit.saturating_sub(body.len()),
            "HTTP body exceeds size limit"
        );
        body.try_reserve(chunk.len())
            .context("HTTP body allocation failed")?;
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn read_reply(raw: &[u8], limit: usize) -> anyhow::Result<Vec<u8>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/", listener.local_addr()?);
        let raw = raw.to_vec();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            socket.read(&mut request).await.unwrap();
            let _ = socket.write_all(&raw).await;
        });
        let response = reqwest::get(url).await?;
        let result = read_bounded_http_body(response, limit).await;
        server.await?;
        result
    }

    #[tokio::test]
    async fn bounds_declared_chunked_and_eof_bodies() {
        let cases: &[(&[u8], bool)] = &[
            (b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ntest", true),
            (b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\ntests", false),
            // Fail on the header without waiting for the advertised payload.
            (b"HTTP/1.1 200 OK\r\nContent-Length: 18446744073709551615\r\n\r\n", false),
            (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nte\r\n2\r\nst\r\n0\r\n\r\n", true),
            (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\ntes\r\n2\r\nts\r\n0\r\n\r\n", false),
            (b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\ntests", false),
            (b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nte", false),
        ];
        for (raw, succeeds) in cases {
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(2), read_reply(raw, 4))
                    .await
                    .unwrap();
            assert_eq!(result.is_ok(), *succeeds, "{result:?}");
        }
    }
}
