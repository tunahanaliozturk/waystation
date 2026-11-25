//! The pieces every end-to-end test needs: the proxy on a runtime of its own,
//! an upstream that can misbehave on purpose, and a client that writes raw
//! bytes, so that nothing between the test and the proxy hides what was sent.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Runtime;
use waystation::Config;

/// Long enough that only a hang reaches it.
pub(crate) const PATIENCE: Duration = Duration::from_secs(20);
const CHUNK: usize = 64 * 1024;

/// The proxy, on a runtime of its own, so that the runtime's task count is
/// the proxy's and nobody else's.
pub(crate) struct Proxy {
    pub(crate) addr: SocketAddr,
    runtime: Option<Runtime>,
}

impl Proxy {
    pub(crate) fn start(config: Config) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        runtime.spawn(async move {
            let listener = TcpListener::from_std(listener).unwrap();
            match waystation::serve(listener, config).await {}
        });
        Self {
            addr,
            runtime: Some(runtime),
        }
    }

    /// Tasks alive on the proxy's runtime. The accept loop is one of them.
    pub(crate) fn tasks(&self) -> usize {
        self.runtime.as_ref().unwrap().metrics().num_alive_tasks()
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        // A runtime cannot be dropped the ordinary way inside another one.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// What the upstream does with each request.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Fault {
    /// Nothing wrong: reads the whole request and answers 200 with a summary
    /// of the body it got.
    Echo,
    /// Answers 200 with `len` bytes of the test pattern.
    Download(u64),
    /// Waits this long before it sends the response head.
    SlowHead(Duration),
    /// Sends a head for `len` bytes and then `sent` of them, and then nothing
    /// more, keeping the connection open.
    Stall { len: u64, sent: u64 },
    /// Sends `len` bytes, one small chunk every `every`, so it is never idle
    /// for long but takes `len / 1024 * every` in all.
    Trickle { len: u64, every: Duration },
    /// Starts a chunked response, sends `sent` bytes of it and resets the
    /// connection.
    Reset { sent: u64 },
    /// Sends a head for `len` bytes, then `sent` of them, then closes.
    WrongLength { len: u64, sent: u64 },
    /// Reads the request and never answers.
    Hang,
    /// Sends these bytes as the response and closes its sending side, which
    /// ends a response that has no length.
    Raw(&'static [u8]),
}

/// An upstream on 127.0.0.1 that records what reached it.
pub(crate) struct Upstream {
    pub(crate) addr: SocketAddr,
    state: Arc<State>,
}

#[derive(Default)]
struct State {
    open: AtomicUsize,
    accepted: AtomicUsize,
    received: AtomicU64,
    heads: Mutex<Vec<String>>,
}

impl Upstream {
    /// Starts listening on the test's runtime.
    pub(crate) async fn start(fault: Fault) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(State::default());
        let shared = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                shared.accepted.fetch_add(1, Ordering::SeqCst);
                shared.open.fetch_add(1, Ordering::SeqCst);
                let shared = Arc::clone(&shared);
                tokio::spawn(async move {
                    // Errors are expected: half the faults end in one.
                    let _ = serve(stream, fault, &shared).await;
                    shared.open.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Self { addr, state }
    }

    /// Connections open right now. One closes when the proxy closes it.
    pub(crate) fn open(&self) -> usize {
        self.state.open.load(Ordering::SeqCst)
    }

    /// Connections accepted so far.
    pub(crate) fn accepted(&self) -> usize {
        self.state.accepted.load(Ordering::SeqCst)
    }

    /// Request body bytes received so far, over all connections.
    pub(crate) fn received(&self) -> u64 {
        self.state.received.load(Ordering::SeqCst)
    }

    /// The last request head received, as text.
    pub(crate) fn last_head(&self) -> String {
        self.state.heads.lock().unwrap().last().cloned().unwrap()
    }
}

async fn serve(mut stream: TcpStream, fault: Fault, state: &State) -> std::io::Result<()> {
    let (head, rest) = read_head(&mut stream).await?;
    state.heads.lock().unwrap().push(head.clone());
    let length = header(&head, "content-length").map_or(0, |v| v.parse().unwrap());
    let mut check = PatternCheck::default();
    check.feed(&rest);
    state.received.fetch_add(wide(rest.len()), Ordering::SeqCst);
    let mut buf = vec![0; CHUNK];
    while check.len < length {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        check.feed(&buf[..n]);
        state.received.fetch_add(wide(n), Ordering::SeqCst);
    }

    match fault {
        Fault::Echo => {
            let body = format!("bytes={} pattern={}", check.len, check.verdict());
            stream
                .write_all(ok_head(wide(body.len())).as_bytes())
                .await?;
            stream.write_all(body.as_bytes()).await?;
        }
        Fault::Download(len) => {
            stream.write_all(ok_head(len).as_bytes()).await?;
            write_pattern(&mut stream, len).await?;
        }
        Fault::SlowHead(wait) => {
            tokio::time::sleep(wait).await;
            stream.write_all(ok_head(2).as_bytes()).await?;
            stream.write_all(b"ok").await?;
        }
        Fault::Stall { len, sent } => {
            stream.write_all(ok_head(len).as_bytes()).await?;
            write_pattern(&mut stream, sent).await?;
        }
        Fault::Trickle { len, every } => {
            stream.write_all(ok_head(len).as_bytes()).await?;
            let pattern = pattern();
            let mut offset = 0;
            while offset < len {
                let n = (len - offset).min(1024);
                let start = narrow(offset % wide(pattern.len()));
                stream.write_all(&pattern[start..start + narrow(n)]).await?;
                offset += n;
                tokio::time::sleep(every).await;
            }
        }
        Fault::Reset { sent } => {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n")
                .await?;
            stream.write_all(format!("{sent:x}\r\n").as_bytes()).await?;
            write_pattern(&mut stream, sent).await?;
            stream.write_all(b"\r\n").await?;
            stream.flush().await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
            stream.set_zero_linger()?;
            return Ok(());
        }
        Fault::WrongLength { len, sent } => {
            stream.write_all(ok_head(len).as_bytes()).await?;
            write_pattern(&mut stream, sent).await?;
            stream.shutdown().await?;
        }
        Fault::Hang => {}
        Fault::Raw(bytes) => {
            stream.write_all(bytes).await?;
            stream.shutdown().await?;
        }
    }
    // Hold the connection until the proxy closes it, so that `open` counts it.
    while stream.read(&mut buf).await? > 0 {}
    Ok(())
}

fn wide(n: usize) -> u64 {
    u64::try_from(n).unwrap()
}

fn narrow(n: u64) -> usize {
    usize::try_from(n).unwrap()
}

fn ok_head(len: u64) -> String {
    format!("HTTP/1.1 200 OK\r\ncontent-length: {len}\r\n\r\n")
}

/// Reads up to the blank line after a head. Returns the head and whatever
/// was read past it.
pub(crate) async fn read_head(
    stream: &mut (impl AsyncRead + Unpin),
) -> std::io::Result<(String, Vec<u8>)> {
    let mut data = Vec::new();
    let mut buf = vec![0; CHUNK];
    loop {
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = data.split_off(end + 4);
            return Ok((String::from_utf8(data).unwrap(), rest));
        }
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        data.extend_from_slice(&buf[..n]);
    }
}

/// The value of the first header called `name` in `head`.
pub(crate) fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    headers(head, name).into_iter().next()
}

/// The values of every header called `name` in `head`.
pub(crate) fn headers<'a>(head: &'a str, name: &str) -> Vec<&'a str> {
    head.lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .filter(|(n, _)| n.trim().eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
        .collect()
}

/// The bytes every large body is made of: a fixed pseudo-random block whose
/// length is prime, so that chunk boundaries keep landing in new places.
pub(crate) fn pattern() -> &'static [u8] {
    static PATTERN: OnceLock<Vec<u8>> = OnceLock::new();
    PATTERN.get_or_init(|| {
        let mut x: u32 = 0x9e37_79b9;
        (0..65_521)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x.to_le_bytes()[0]
            })
            .collect()
    })
}

/// Writes the first `len` bytes of the endless repetition of the pattern.
pub(crate) async fn write_pattern(
    w: &mut (impl AsyncWrite + Unpin),
    len: u64,
) -> std::io::Result<()> {
    let pattern = pattern();
    let mut offset = 0;
    while offset < len {
        let start = narrow(offset % wide(pattern.len()));
        let n = (pattern.len() - start).min(narrow((len - offset).min(wide(CHUNK))));
        w.write_all(&pattern[start..start + n]).await?;
        offset += wide(n);
    }
    w.flush().await
}

/// Checks bytes against the pattern as they arrive, without keeping them.
#[derive(Default)]
pub(crate) struct PatternCheck {
    pub(crate) len: u64,
    pub(crate) intact: bool,
    started: bool,
}

impl PatternCheck {
    pub(crate) fn feed(&mut self, mut chunk: &[u8]) {
        if !self.started {
            self.started = true;
            self.intact = true;
        }
        let pattern = pattern();
        while !chunk.is_empty() {
            let start = narrow(self.len % wide(pattern.len()));
            let n = chunk.len().min(pattern.len() - start);
            self.intact &= chunk[..n] == pattern[start..start + n];
            self.len += wide(n);
            chunk = &chunk[n..];
        }
    }

    pub(crate) fn verdict(&self) -> &'static str {
        if self.intact || self.len == 0 {
            "ok"
        } else {
            "bad"
        }
    }
}

/// A response as the client saw it.
#[derive(Debug)]
pub(crate) struct Reply {
    /// 0 if the connection closed before a whole head arrived.
    pub(crate) status: u16,
    pub(crate) head: String,
    pub(crate) body: Vec<u8>,
    /// Whether the body ended the way its framing says it should: all of a
    /// `Content-Length`, or the last chunk of a chunked body.
    pub(crate) complete: bool,
}

impl Reply {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        header(&self.head, name)
    }

    pub(crate) fn text(&self) -> &str {
        std::str::from_utf8(&self.body).unwrap()
    }
}

/// Sends `request` on a new connection, then reads until the proxy closes it.
pub(crate) async fn exchange(proxy: SocketAddr, request: &[u8]) -> Reply {
    let mut stream = TcpStream::connect(proxy).await.unwrap();
    stream.write_all(request).await.unwrap();
    read_reply(&mut stream).await
}

/// Reads a whole response, and then on until the connection closes.
pub(crate) async fn read_reply(stream: &mut TcpStream) -> Reply {
    let mut data = Vec::new();
    let read = async {
        // A reset after part of a response counts as the end of it.
        let _ = stream.read_to_end(&mut data).await;
    };
    tokio::time::timeout(PATIENCE, read)
        .await
        .expect("the proxy neither finished the response nor closed the connection");
    let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") else {
        // The connection closed before a whole head arrived.
        return Reply {
            status: 0,
            head: String::new(),
            body: data,
            complete: false,
        };
    };
    let head = String::from_utf8(data[..end].to_vec()).unwrap();
    let raw = &data[end + 4..];
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let (body, complete) = if header(&head, "transfer-encoding") == Some("chunked") {
        dechunk(raw)
    } else if let Some(len) = header(&head, "content-length") {
        let len: usize = len.parse().unwrap();
        (raw[..raw.len().min(len)].to_vec(), raw.len() >= len)
    } else {
        (raw.to_vec(), true)
    };
    Reply {
        status,
        head,
        body,
        complete,
    }
}

fn dechunk(mut raw: &[u8]) -> (Vec<u8>, bool) {
    let mut body = Vec::new();
    loop {
        let Some(line_end) = raw.windows(2).position(|w| w == b"\r\n") else {
            return (body, false);
        };
        let size = std::str::from_utf8(&raw[..line_end]).unwrap();
        let size = usize::from_str_radix(size.split(';').next().unwrap().trim(), 16).unwrap();
        raw = &raw[line_end + 2..];
        if size == 0 {
            return (body, true);
        }
        if raw.len() < size + 2 {
            body.extend_from_slice(&raw[..raw.len().min(size)]);
            return (body, false);
        }
        body.extend_from_slice(&raw[..size]);
        raw = &raw[size + 2..];
    }
}

/// A plain GET that asks for the connection to be closed after it.
pub(crate) fn get(path: &str) -> Vec<u8> {
    format!("GET {path} HTTP/1.1\r\nhost: example.test\r\nconnection: close\r\n\r\n").into_bytes()
}

/// Waits until `done` holds, and fails the test if it does not within `limit`.
/// Returns how long it took.
pub(crate) async fn eventually(
    limit: Duration,
    what: &str,
    mut done: impl FnMut() -> bool,
) -> Duration {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < limit, "not within {limit:?}: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    start.elapsed()
}
