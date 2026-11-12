//! The network, in-process, with WinHTTP: Windows' own HTTP stack, with its TLS, the certificates
//! Windows trusts, the system's proxy, and no dependency. A request waits for its answer, so it's
//! made on a worker thread by whoever needs one (a Home Assistant service, a calendar's download).
//! A WebSocket is asynchronous: WinHTTP's own threads call back, and the callback only queues what
//! came and sets the owner's event, so a connection open for hours costs no thread of the bar's
//! and no wake-up while nothing comes; WinHTTP keeps it alive by itself (a ping every 30 s, on its
//! own threads).

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING, WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE,
    WINHTTP_CALLBACK_STATUS_READ_COMPLETE, WINHTTP_CALLBACK_STATUS_REQUEST_ERROR, WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE,
    WINHTTP_CALLBACK_STATUS_WRITE_COMPLETE, WINHTTP_FLAG_ASYNC, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_OPTION_CONTEXT_VALUE,
    WINHTTP_OPTION_DECOMPRESSION, WINHTTP_OPTION_RECEIVE_TIMEOUT, WINHTTP_OPTION_UPGRADE_TO_WEB_SOCKET, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WINHTTP_WEB_SOCKET_BINARY_MESSAGE_BUFFER_TYPE, WINHTTP_WEB_SOCKET_CLOSE_BUFFER_TYPE, WINHTTP_WEB_SOCKET_STATUS,
    WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption, WinHttpSetStatusCallback,
    WinHttpSetTimeouts, WinHttpWebSocketCompleteUpgrade, WinHttpWebSocketReceive, WinHttpWebSocketSend,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::wake::Wake;

/// gzip, which WinHTTP undoes by itself. Not deflate: servers send it raw or wrapped in zlib, and
/// WinHTTP fails on one of them (`E_ABORT` at the first read, from Open-Meteo); asking for gzip
/// alone, they answer gzip or plain.
const DECOMPRESS_GZIP: u32 = 1;
/// The most a request reads: a calendar of years is a few MB.
const MAX_BODY: usize = 32 << 20;
/// The receive buffer of a WebSocket: a message longer than this comes in pieces.
const CHUNK: usize = 64 << 10;

/// An address split as WinHTTP takes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Url {
    pub secure: bool,
    pub host: String,
    pub port: u16,
    /// The path and the query, from the `/`.
    pub path: String,
}

/// `http(s)://host[:port][/path]`; `ws(s)://` too, as HTTP's.
pub fn parse(url: &str) -> Option<Url> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let secure = match scheme.to_ascii_lowercase().as_str() {
        "https" | "wss" => true,
        "http" | "ws" => false,
        _ => return None,
    };
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) if rest[i..].starts_with('/') => (&rest[..i], rest[i..].to_string()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_string()),
    };
    // A user and password before `@` aren't taken: tokens go in headers
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || authority.starts_with('[') => match p.parse::<u16>() {
            Ok(p) => (h, p),
            Err(_) => (authority, 0),
        },
        _ => (authority, 0),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return None;
    }
    let port = if port != 0 { port } else if secure { 443 } else { 80 };
    Some(Url { secure, host: host.to_string(), port, path })
}

/// A WinHTTP handle, closed when dropped.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn check(h: *mut c_void, what: &str) -> Result<Handle, String> {
    if h.is_null() { Err(format!("{what}: {}", windows::core::Error::from_thread())) } else { Ok(Handle(h)) }
}

/// A request and its answer: the status and the body.
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Makes a request and waits for the whole answer, at most `timeout` ms for each step: for a
/// worker thread. `headers` are `Name: value` pairs.
pub fn request(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>, timeout: i32) -> Result<Response, String> {
    let u = parse(url).ok_or_else(|| format!("not an address: {url}"))?;
    unsafe {
        let session = check(WinHttpOpen(w!("RustyBar"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0), "session")?;
        let _ = WinHttpSetTimeouts(session.0, timeout, timeout, timeout, timeout);
        let connect = check(WinHttpConnect(session.0, &HSTRING::from(&u.host), u.port, 0), "connect")?;
        let flags = if u.secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let req = check(
            WinHttpOpenRequest(connect.0, &HSTRING::from(method), &HSTRING::from(&u.path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags),
            "request",
        )?;
        let _ = WinHttpSetOption(Some(req.0), WINHTTP_OPTION_DECOMPRESSION, Some(&DECOMPRESS_GZIP.to_ne_bytes()));
        let lines: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
        let lines: Vec<u16> = lines.encode_utf16().collect();
        let (data, len) = match body {
            Some(b) => (Some(b.as_ptr() as *const c_void), b.len() as u32),
            None => (None, 0),
        };
        WinHttpSendRequest(req.0, (!lines.is_empty()).then_some(&lines[..]), data, len, len, 0).map_err(|e| format!("send: {e}"))?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|e| format!("receive: {e}"))?;
        let status = status_code(req.0);
        let mut out = Vec::new();
        loop {
            let mut available = 0u32;
            WinHttpQueryDataAvailable(req.0, &mut available).map_err(|e| format!("read: {e}"))?;
            if available == 0 {
                break;
            }
            let start = out.len();
            out.resize(start + available as usize, 0);
            let mut read = 0u32;
            WinHttpReadData(req.0, out[start..].as_mut_ptr() as *mut c_void, available, &mut read).map_err(|e| format!("read: {e}"))?;
            out.truncate(start + read as usize);
            if out.len() > MAX_BODY {
                return Err("answer too long".into());
            }
        }
        Ok(Response { status, body: out })
    }
}

unsafe fn status_code(req: *mut c_void) -> u16 {
    let mut status = 0u32;
    let mut size = 4u32;
    let _ = unsafe {
        WinHttpQueryHeaders(
            req,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )
    };
    status as u16
}

// ---- WebSocket ---------------------------------------------------------------------------------

/// A WebSocket's state, shared with WinHTTP's callbacks.
struct Conn {
    wake: Arc<Wake>,
    /// The receive buffer, `CHUNK` bytes: WinHTTP writes into it while a receive is out, so it's
    /// outside the mutex.
    buffer: *mut u8,
    state: Mutex<State>,
}

unsafe impl Send for Conn {}
unsafe impl Sync for Conn {}

impl Drop for Conn {
    fn drop(&mut self) {
        unsafe { drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(self.buffer, CHUNK))) };
    }
}

#[derive(Default)]
struct State {
    /// The handles, as addresses: session, connection, the upgrade request, the WebSocket.
    session: usize,
    connect: usize,
    request: usize,
    socket: usize,
    closed: bool,
    inbox: Vec<String>,
    /// A message that came in pieces, so far.
    partial: Vec<u8>,
    /// The message being sent (WinHTTP reads it until it's sent) and those waiting for it.
    sending: Option<Vec<u8>>,
    queue: VecDeque<Vec<u8>>,
}

fn lock(c: &Conn) -> std::sync::MutexGuard<'_, State> {
    c.state.lock().unwrap_or_else(|e| e.into_inner())
}

impl Conn {
    /// The connection is over: the owner hears it once.
    fn fail(&self) {
        let mut s = lock(self);
        if !s.closed {
            s.closed = true;
            drop(s);
            self.wake.set();
        }
    }

    /// Asks for the next message. Never with the lock held: WinHTTP may call back at once, on
    /// this thread.
    fn receive(&self) {
        let socket = {
            let s = lock(self);
            if s.closed { 0 } else { s.socket }
        };
        if socket == 0 {
            return;
        }
        let (mut read, mut kind) = (0u32, Default::default());
        let error = unsafe { WinHttpWebSocketReceive(socket as *const c_void, self.buffer as *mut c_void, CHUNK as u32, &mut read, &mut kind) };
        if error != 0 {
            self.fail();
        }
    }

    /// Sends the next message waiting, if none is out.
    fn send_next(&self) {
        let (socket, ptr, len) = {
            let mut s = lock(self);
            if s.closed || s.socket == 0 || s.sending.is_some() {
                return;
            }
            let Some(next) = s.queue.pop_front() else { return };
            let (ptr, len) = (next.as_ptr(), next.len());
            s.sending = Some(next);
            (s.socket, ptr, len)
        };
        let error = unsafe { WinHttpWebSocketSend(socket as *const c_void, WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE, Some(std::slice::from_raw_parts(ptr, len))) };
        if error != 0 {
            self.fail();
        }
    }
}

unsafe extern "system" fn callback(handle: *mut c_void, context: usize, status: u32, info: *mut c_void, _len: u32) {
    if context == 0 {
        return;
    }
    let conn = unsafe { &*(context as *const Conn) };
    match status {
        WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE => {
            if unsafe { WinHttpReceiveResponse(handle, std::ptr::null_mut()) }.is_err() {
                conn.fail();
            }
        }
        WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE => {
            if lock(conn).closed {
                return;
            }
            if unsafe { status_code(handle) } != 101 {
                return conn.fail();
            }
            // The WebSocket's own handle: another reference to `conn`, let go when it closes
            let socket = unsafe { WinHttpWebSocketCompleteUpgrade(handle, Some(context)) };
            if socket.is_null() {
                return conn.fail();
            }
            unsafe { Arc::increment_strong_count(context as *const Conn) };
            // Waits for messages as long as there are none, unlike the upgrade's request
            let _ = unsafe { WinHttpSetOption(Some(socket), WINHTTP_OPTION_RECEIVE_TIMEOUT, Some(&0u32.to_ne_bytes())) };
            let request = {
                let mut s = lock(conn);
                s.socket = socket as usize;
                std::mem::take(&mut s.request)
            };
            if request != 0 {
                unsafe {
                    let _ = WinHttpCloseHandle(request as *mut c_void);
                }
            }
            conn.send_next();
            conn.receive();
        }
        WINHTTP_CALLBACK_STATUS_READ_COMPLETE => {
            let ws = unsafe { &*(info as *const WINHTTP_WEB_SOCKET_STATUS) };
            let piece = unsafe { std::slice::from_raw_parts(conn.buffer, ws.dwBytesTransferred as usize) };
            if ws.eBufferType == WINHTTP_WEB_SOCKET_CLOSE_BUFFER_TYPE {
                return conn.fail();
            }
            let whole = {
                let mut s = lock(conn);
                s.partial.extend_from_slice(piece);
                if ws.eBufferType == WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE || ws.eBufferType == WINHTTP_WEB_SOCKET_BINARY_MESSAGE_BUFFER_TYPE {
                    let text = String::from_utf8_lossy(&std::mem::take(&mut s.partial)).into_owned();
                    s.inbox.push(text);
                    true
                } else {
                    false
                }
            };
            if whole {
                conn.wake.set();
            }
            conn.receive();
        }
        WINHTTP_CALLBACK_STATUS_WRITE_COMPLETE => {
            lock(conn).sending = None;
            conn.send_next();
        }
        WINHTTP_CALLBACK_STATUS_REQUEST_ERROR => conn.fail(),
        // The handle's reference to `conn` goes with it
        WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING => unsafe { drop(Arc::from_raw(context as *const Conn)) },
        _ => {}
    }
}

/// A WebSocket to a server: messages come into an inbox, and the owner's event is set when one
/// comes or when the connection ends. Closed when dropped.
pub struct Socket {
    conn: Arc<Conn>,
}

impl Socket {
    /// Starts connecting to `url` (`http(s)://` or `ws(s)://`); what's sent before it's open
    /// waits.
    pub fn open(url: &str, headers: &[(&str, &str)], wake: Arc<Wake>) -> Result<Socket, String> {
        let u = parse(url).ok_or_else(|| format!("not an address: {url}"))?;
        let buffer = Box::into_raw(vec![0u8; CHUNK].into_boxed_slice()) as *mut u8;
        let conn = Arc::new(Conn { wake, buffer, state: Mutex::new(State::default()) });
        let socket = Socket { conn };
        unsafe {
            let session = WinHttpOpen(w!("RustyBar"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), WINHTTP_FLAG_ASYNC);
            if session.is_null() {
                return Err(format!("session: {}", windows::core::Error::from_thread()));
            }
            lock(&socket.conn).session = session as usize;
            WinHttpSetStatusCallback(session, Some(callback), u32::MAX, 0);
            let _ = WinHttpSetTimeouts(session, 10_000, 10_000, 10_000, 15_000);
            let connect = WinHttpConnect(session, &HSTRING::from(&u.host), u.port, 0);
            if connect.is_null() {
                return Err(format!("connect: {}", windows::core::Error::from_thread()));
            }
            lock(&socket.conn).connect = connect as usize;
            let flags = if u.secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
            let request = WinHttpOpenRequest(connect, w!("GET"), &HSTRING::from(&u.path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags);
            if request.is_null() {
                return Err(format!("request: {}", windows::core::Error::from_thread()));
            }
            lock(&socket.conn).request = request as usize;
            // The request's reference to `conn`, let go when its handle closes
            let context = Arc::into_raw(socket.conn.clone()) as usize;
            let _ = WinHttpSetOption(Some(request), WINHTTP_OPTION_CONTEXT_VALUE, Some(&context.to_ne_bytes()));
            WinHttpSetOption(Some(request), WINHTTP_OPTION_UPGRADE_TO_WEB_SOCKET, None).map_err(|e| format!("upgrade: {e}"))?;
            let lines: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
            let lines: Vec<u16> = lines.encode_utf16().collect();
            WinHttpSendRequest(request, (!lines.is_empty()).then_some(&lines[..]), None, 0, 0, context).map_err(|e| format!("send: {e}"))?;
        }
        Ok(socket)
    }

    /// Sends a text message, after those before it.
    pub fn send(&self, text: &str) {
        lock(&self.conn).queue.push_back(text.as_bytes().to_vec());
        self.conn.send_next();
    }

    /// The messages that came since last time, and whether the connection is over.
    pub fn take(&self) -> (Vec<String>, bool) {
        let mut s = lock(&self.conn);
        (std::mem::take(&mut s.inbox), s.closed)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        // Out of the lock first: closing a handle may call back on this thread
        let handles = {
            let mut s = lock(&self.conn);
            s.closed = true;
            [std::mem::take(&mut s.socket), std::mem::take(&mut s.request), std::mem::take(&mut s.connect), std::mem::take(&mut s.session)]
        };
        for h in handles {
            if h != 0 {
                unsafe {
                    let _ = WinHttpCloseHandle(h as *mut c_void);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let u = parse("http://homeassistant.local:8123").unwrap();
        assert_eq!(u, Url { secure: false, host: "homeassistant.local".into(), port: 8123, path: "/".into() });
        let u = parse("wss://ha.example.com/api/websocket").unwrap();
        assert_eq!((u.secure, u.port, u.path.as_str()), (true, 443, "/api/websocket"));
        let u = parse("https://calendar.example.com/ical/abc%40x/private-1/basic.ics?x=1").unwrap();
        assert_eq!(u.path, "/ical/abc%40x/private-1/basic.ics?x=1");
        assert_eq!(parse("http://h?q=1").unwrap().path, "/?q=1");
        assert_eq!(parse("http://[::1]:8080/a").unwrap().host, "::1");
        assert_eq!(parse("http://[::1]:8080/a").unwrap().port, 8080);
        assert!(parse("ftp://x").is_none());
        assert!(parse("homeassistant.local").is_none());
    }

    /// A server that answers `deflate` when asked (Open-Meteo): it used to fail at the first read.
    #[test]
    #[ignore]
    fn a_server_that_likes_deflate() {
        let url = "https://api.open-meteo.com/v1/forecast?latitude=41.89&longitude=12.49&current=temperature_2m";
        let r = request("GET", url, &[], None, 15_000).unwrap();
        assert_eq!(r.status, 200);
        assert!(String::from_utf8_lossy(&r.body).contains("temperature_2m"));
    }
}
