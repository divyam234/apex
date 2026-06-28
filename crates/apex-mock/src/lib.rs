#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct MockResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub delay_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct MockRoute {
    pub method: String,
    pub path: String,
    pub responses: Vec<MockResponse>,
}
#[derive(Clone, Debug)]
pub struct MockConfig {
    pub bind: SocketAddr,
    pub allow_public_bind: bool,
    pub maximum_request_bytes: usize,
    pub maximum_response_bytes: usize,
    pub log_capacity: usize,
    pub tls: bool,
}
impl Default for MockConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            allow_public_bind: false,
            maximum_request_bytes: 1024 * 1024,
            maximum_response_bytes: 1024 * 1024,
            log_capacity: 1000,
            tls: false,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MockLogEntry {
    pub method: String,
    pub path: String,
    pub status: u16,
}

pub struct MockServer {
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    logs: Arc<Mutex<VecDeque<MockLogEntry>>>,
}
impl MockServer {
    pub fn start(config: MockConfig, routes: Vec<MockRoute>) -> Result<Self, String> {
        if !config.bind.ip().is_loopback() && !config.allow_public_bind {
            return Err("public mock binding requires explicit confirmation".into());
        }
        if config.tls {
            return Err("TLS requires an externally supplied certificate adapter".into());
        }
        if config.maximum_request_bytes == 0
            || config.maximum_response_bytes == 0
            || config.log_capacity == 0
        {
            return Err("mock limits must be non-zero".into());
        }
        for route in &routes {
            for response in &route.responses {
                if response.body.len() > config.maximum_response_bytes {
                    return Err("mock response exceeds configured byte limit".into());
                }
            }
        }
        let listener = TcpListener::bind(config.bind)
            .map_err(|e| format!("could not bind mock server: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("could not configure mock server: {e}"))?;
        let address = listener
            .local_addr()
            .map_err(|e| format!("could not inspect mock address: {e}"))?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        let logs = Arc::new(Mutex::new(VecDeque::new()));
        let thread_logs = logs.clone();
        let state = Arc::new(Mutex::new(
            routes.into_iter().map(|r| (r, 0usize)).collect::<Vec<_>>(),
        ));
        let thread = thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = handle(&mut stream, &state, &thread_logs, &config);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            address,
            shutdown,
            thread: Some(thread),
            logs,
        })
    }
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn logs(&self) -> Vec<MockLogEntry> {
        self.logs.lock().expect("logs").iter().cloned().collect()
    }
    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for MockServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn handle(
    stream: &mut TcpStream,
    state: &Arc<Mutex<Vec<(MockRoute, usize)>>>,
    logs: &Arc<Mutex<VecDeque<MockLogEntry>>>,
    config: &MockConfig,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.len() > config.maximum_request_bytes {
            return Err("request exceeds configured byte limit".into());
        }
        if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&bytes);
    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default();
    let mut guard = state
        .lock()
        .map_err(|_| "route state unavailable".to_owned())?;
    let (status, headers, body, delay) = if let Some((route, index)) = guard
        .iter_mut()
        .find(|(r, _)| r.method.eq_ignore_ascii_case(method) && r.path == path)
    {
        let response = route
            .responses
            .get((*index).min(route.responses.len().saturating_sub(1)))
            .cloned()
            .ok_or_else(|| "matched route has no responses".to_owned())?;
        *index = index.saturating_add(1);
        (
            response.status,
            response.headers,
            template(&response.body, method, path),
            response.delay_ms,
        )
    } else {
        (404, Vec::new(), "not found".into(), 0)
    };
    drop(guard);
    if delay > 0 {
        thread::sleep(Duration::from_millis(delay.min(60_000)));
    }
    let reason = if status == 200 {
        "OK"
    } else if status == 404 {
        "Not Found"
    } else {
        "Mock"
    };
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        response.push_str(&format!("{k}: {v}\r\n"));
    }
    response.push_str("\r\n");
    response.push_str(&body);
    stream
        .write_all(response.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut log = logs.lock().map_err(|_| "log unavailable".to_owned())?;
    if log.len() == config.log_capacity {
        log.pop_front();
    }
    log.push_back(MockLogEntry {
        method: method.into(),
        path: path.into(),
        status,
    });
    Ok(())
}
fn template(body: &str, method: &str, path: &str) -> String {
    body.replace("{{request.method}}", method)
        .replace("{{request.path}}", path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_bind_requires_confirmation() {
        let c = MockConfig {
            bind: "0.0.0.0:0".parse().unwrap(),
            ..Default::default()
        };
        assert!(MockServer::start(c, vec![]).is_err());
    }
    #[test]
    fn real_loopback_server_matches_sequences_and_logs() {
        let route = MockRoute {
            method: "GET".into(),
            path: "/hello".into(),
            responses: vec![
                MockResponse {
                    status: 200,
                    headers: vec![],
                    body: "{{request.method}} {{request.path}}".into(),
                    delay_ms: 0,
                },
                MockResponse {
                    status: 503,
                    headers: vec![],
                    body: "down".into(),
                    delay_ms: 0,
                },
            ],
        };
        let mut server = MockServer::start(
            MockConfig {
                log_capacity: 1,
                ..Default::default()
            },
            vec![route],
        )
        .expect("server");
        for expected in ["200", "503"] {
            let mut s = TcpStream::connect(server.address()).unwrap();
            s.write_all(b"GET /hello HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            assert!(out.contains(expected));
        }
        assert_eq!(server.logs().len(), 1);
        server.shutdown();
    }
    #[test]
    fn payload_limits_are_enforced() {
        let route = MockRoute {
            method: "GET".into(),
            path: "/".into(),
            responses: vec![MockResponse {
                status: 200,
                headers: vec![],
                body: "large".into(),
                delay_ms: 0,
            }],
        };
        assert!(
            MockServer::start(
                MockConfig {
                    maximum_response_bytes: 2,
                    ..Default::default()
                },
                vec![route]
            )
            .is_err()
        );
    }
}
