//! Byte-capped HTTP body read for cli, client, and proxy.
//!
//! `Response::chunk` keeps this module off `futures-util`, which the cli
//! feature does not depend on. Error text never includes the body or a
//! reqwest `Display` (that Display can embed the request URL).

#[derive(Debug)]
pub(crate) enum CappedBodyError {
    /// The body is over the cap. The string is safe to show: length and cap only.
    TooLarge(String),
    /// Transport failure. No reqwest `Display`, so the URL stays out of the text.
    Read,
}

impl std::fmt::Display for CappedBodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(message) => f.write_str(message),
            Self::Read => f.write_str("response body read failed"),
        }
    }
}

pub(crate) async fn read_capped_text(
    mut resp: reqwest::Response,
    cap: usize,
) -> Result<String, CappedBodyError> {
    if let Some(len) = resp.content_length()
        && len > cap as u64
    {
        return Err(CappedBodyError::TooLarge(format!(
            "response body too large (Content-Length: {len} bytes, max {cap})"
        )));
    }
    let mut buf = Vec::new();
    loop {
        let chunk = match resp.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => return Err(CappedBodyError::Read),
        };
        if buf.len().saturating_add(chunk.len()) > cap {
            return Err(CappedBodyError::TooLarge(format!(
                "response body exceeds {cap} bytes"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    fn serve(raw: String) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
            stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
            let mut buf = [0u8; 1024];
            let mut seen = Vec::new();
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        seen.extend_from_slice(&buf[..n]);
                        if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = stream.write_all(raw.as_bytes());
        });
        (format!("http://{addr}/body"), handle)
    }

    async fn get(url: &str) -> reqwest::Response {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client")
            .get(url)
            .send()
            .await
            .expect("send")
    }

    #[tokio::test]
    async fn content_length_over_cap_is_an_error() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nTAILMARK";
        let (url, handle) = serve(raw.to_string());
        let err = read_capped_text(get(&url).await, 64)
            .await
            .expect_err("cap")
            .to_string();
        assert!(err.contains("too large"), "{err}");
        assert!(!err.contains("TAILMARK"), "{err}");
        let _ = handle.join();
    }

    #[tokio::test]
    async fn chunk_over_cap_is_an_error() {
        let mut body = "a".repeat(70);
        body.push_str("TAILMARK");
        let raw = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{body}");
        let (url, handle) = serve(raw);
        let err = read_capped_text(get(&url).await, 64)
            .await
            .expect_err("cap")
            .to_string();
        assert!(err.contains("exceeds"), "{err}");
        assert!(!err.contains("TAILMARK"), "{err}");
        let _ = handle.join();
    }
}
