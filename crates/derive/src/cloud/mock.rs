//! A loopback HTTP server for the backend tests: canned responses in
//! order, one connection each, the raw requests handed back.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

/// Serve `responses` in order on a loopback port, one connection each;
/// returns the base URL and the captured requests (head, blank line, body).
pub(super) fn mock(responses: Vec<String>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (base, serve(listener, responses))
}

pub(super) fn serve(listener: TcpListener, responses: Vec<String>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for resp in responses {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let body_start;
            loop {
                let n = s.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    body_start = i + 4;
                    break;
                }
            }
            let head = String::from_utf8_lossy(&buf[..body_start]).to_string();
            let len: usize = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap_or(0);
            while buf.len() < body_start + len {
                let n = s.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            let body = String::from_utf8_lossy(&buf[body_start..]).to_string();
            let _ = tx.send(format!("{head}\n{body}"));
            s.write_all(resp.as_bytes()).unwrap();
            s.flush().unwrap();
        }
    });
    rx
}

pub(super) fn http(status: &str, extra: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-length: {}\r\n{extra}connection: close\r\n\r\n{body}",
        body.len()
    )
}
