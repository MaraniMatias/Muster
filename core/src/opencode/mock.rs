//! A loopback stand-in for the OpenCode service.
//!
//! The bridge is checked against the wire it really speaks: a registration file, a
//! basic-auth header, a `directory` query on every request, a `data` envelope and a
//! transcript to search. Nothing here is a stub of [`super::Client`] — it is a
//! listener that answers request lines, so a test that asserts on a request sees the
//! bytes the client wrote.
//!
//! Tests, not the app: the module is compiled only under `cfg(test)`.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// Binds a port, waiting for it while another mock is holding it.
///
/// A test that stops a mock and starts another where it was frees a port that the
/// operating system may hand to a test running beside it. Waiting is the answer:
/// the test wants that port, and the mock that took it is not going to keep it.
fn bind_port(port: u16) -> TcpListener {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => return listener,
            Err(error)
                if error.kind() == std::io::ErrorKind::AddrInUse && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("the mock should bind the port: {error}"),
        }
    }
}

/// What the service answers with. `body` is sent verbatim, so a test can answer with
/// something that is not JSON at all.
pub(crate) struct Reply {
    status: u16,
    body: String,
    /// Take the request and close without answering, which is what a service that
    /// accepted a prompt and then lost the connection looks like from here.
    silent: bool,
    /// Headers this reply needs beyond the two it always carries.
    headers: Vec<(&'static str, String)>,
}

impl Reply {
    /// A reply with a header of its own, such as the `location` a redirect carries.
    pub(crate) fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

pub(crate) fn reply(status: u16, body: serde_json::Value) -> Reply {
    Reply {
        status,
        body: body.to_string(),
        silent: false,
        headers: Vec::new(),
    }
}

/// A redirect to somewhere else, which is the one answer a loopback client must not
/// act on: the test asserts the other address is never contacted.
pub(crate) fn redirect(location: String) -> Reply {
    reply(302, serde_json::json!({})).header("location", location)
}

/// Accepts the request and answers nothing, so the client cannot tell whether the
/// service took it.
pub(crate) fn silent() -> Reply {
    Reply {
        status: 0,
        body: String::new(),
        silent: true,
        headers: Vec::new(),
    }
}

/// One request as the service received it.
#[derive(Clone)]
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Request {
    /// A header by name, case-insensitively, as HTTP names are.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// A query parameter as the service would parse it, so a test asserts on the
    /// value rather than on its percent-encoding.
    pub fn query_param(&self, name: &str) -> Option<String> {
        let wanted = format!("{name}=");
        let query = self.query.as_deref()?;
        let pair = query.split('&').find(|pair| pair.starts_with(&wanted))?;
        percent_decode(pair.strip_prefix(&wanted)?)
    }

    /// The request body as JSON, so a test asserts on the fields and not on how the
    /// client chose to lay them out.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("a JSON request body")
    }
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = value.get(index + 1..index + 3)?;
                decoded.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

pub(crate) struct Mock {
    port: u16,
    requests: Arc<Mutex<Vec<Request>>>,
    stopped: Arc<AtomicBool>,
    server: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
}

impl Mock {
    /// Serves until [`Mock::stop`] or until the test drops it.
    pub(crate) fn start(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        Self::serve(
            TcpListener::bind(("127.0.0.1", 0)).expect("the mock should bind"),
            handler,
        )
    }

    /// Serves on a port the test already knows, which is what stopping one service and
    /// starting another where it was needs.
    ///
    /// The port is waited for rather than refused: the mock that was stopped freed it,
    /// and the operating system may have handed it to another mock in the meantime.
    pub(crate) fn start_on(
        port: u16,
        handler: impl Fn(&Request) -> Reply + Send + Sync + 'static,
    ) -> Self {
        Self::serve(bind_port(port), handler)
    }

    fn serve(
        listener: TcpListener,
        handler: impl Fn(&Request) -> Reply + Send + Sync + 'static,
    ) -> Self {
        listener
            .set_nonblocking(true)
            .expect("the mock should be able to poll for a stop");
        let port = listener
            .local_addr()
            .expect("the mock should have an address")
            .port();
        let requests: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(handler);
        let (seen, stop) = (Arc::clone(&requests), Arc::clone(&stopped));
        let server = Arc::new(Mutex::new(Some(thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("the mock should serve one request at a time");
                        let Ok(request) = read_request(&stream) else {
                            continue;
                        };
                        seen.lock()
                            .expect("the mock records should not be poisoned")
                            .push(request.clone());
                        let answer = handler(&request);
                        let _ = write_reply(stream, &answer);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        }))));
        Self {
            port,
            requests,
            stopped,
            server,
        }
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// A copy of everything the service has been asked so far.
    pub(crate) fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .expect("the mock records should not be poisoned")
            .clone()
    }

    /// Stops serving and waits for the listener to be gone, so a later connection is
    /// refused rather than left sitting in a backlog nobody accepts.
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(server) = self
            .server
            .lock()
            .expect("the mock handle should not be poisoned")
            .take()
        {
            let _ = server.join();
        }
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop();
    }
}

fn read_request(stream: &TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), Some(query.to_string())),
        None => (target, None),
    };
    let mut headers = Vec::new();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                length = value.parse().unwrap_or(0);
            }
            headers.push((name.trim().to_ascii_lowercase(), value.to_string()));
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Request {
        method,
        path,
        query,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn write_reply(mut stream: TcpStream, reply: &Reply) -> std::io::Result<()> {
    if reply.silent {
        // Dropping the socket without a status line is the uncertainty: the request was
        // received, and whether it was acted on is unknowable from the client.
        drop(stream.try_clone()?);
        return Ok(());
    }
    let extra = reply
        .headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    write!(
        stream,
        "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: \
         close\r\n{extra}\r\n{}",
        reply.status,
        reason(reply.status),
        reply.body.len(),
        reply.body
    )?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}
