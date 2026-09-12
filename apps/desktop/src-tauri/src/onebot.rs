//! Localhost-only OneBot HTTP action boundary.
//!
//! This module deliberately does not start an event listener or enable any QQ
//! side effect by itself. It provides typed, loopback-only actions plus a
//! manually driven reverse-event listener for a future Observe integration.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::{json, Value};

use crate::qq::{QQAdapter, QQError, QQGroup, QQImage};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_IMAGE_BYTES: usize = 25 * 1024 * 1024;
const MAX_ACTION_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_HTTP_RESPONSE_BYTES: usize = MAX_IMAGE_BYTES + 64 * 1024;
const MAX_PENDING_MESSAGES: usize = 512;

/// A group message that has at least one OneBot image segment.
///
/// The adapter keeps this as an in-memory event until the pipeline consumes
/// it. Image bytes are deliberately not retained here; `download_image` asks
/// OneBot for the bytes only when the message is actually processed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneBotGroupMessage {
    pub message_id: String,
    pub group_id: String,
    pub images: Vec<QQImage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopbackEndpoint {
    host: String,
    port: u16,
    path: String,
}

impl LoopbackEndpoint {
    pub fn parse(url: &str) -> Result<Self, QQError> {
        let remainder = url
            .strip_prefix("http://")
            .ok_or_else(|| QQError::Unsupported("OneBot endpoint must use http://".to_owned()))?;
        if remainder.is_empty() || remainder.contains('@') {
            return Err(QQError::Unsupported(
                "endpoint must not contain credentials".to_owned(),
            ));
        }
        let (authority, path) = match remainder.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (remainder, "/".to_owned()),
        };
        let (host, port) = if authority.starts_with('[') {
            let closing = authority
                .find(']')
                .ok_or_else(|| QQError::Unsupported("invalid IPv6 endpoint".to_owned()))?;
            let host = &authority[1..closing];
            let port = authority
                .get(closing + 1..)
                .and_then(|suffix| suffix.strip_prefix(':'))
                .map(parse_port)
                .transpose()?
                .unwrap_or(80);
            (host.to_owned(), port)
        } else {
            let pieces: Vec<&str> = authority.split(':').collect();
            if pieces.len() > 2 {
                return Err(QQError::Unsupported(
                    "IPv6 endpoint must use brackets".to_owned(),
                ));
            }
            let host = pieces[0];
            let port = pieces
                .get(1)
                .map(|value| parse_port(value))
                .transpose()?
                .unwrap_or(80);
            (host.to_owned(), port)
        };
        if !is_loopback_host(&host) {
            return Err(QQError::Unsupported(
                "OneBot endpoint must resolve to localhost/loopback".to_owned(),
            ));
        }
        Ok(Self { host, port, path })
    }

    pub fn display_url(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("http://{host}:{}{}", self.port, self.path)
    }

    fn socket_addresses(&self) -> Result<Vec<SocketAddr>, QQError> {
        (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map(|addresses| {
                addresses
                    .filter(|address| address.ip().is_loopback())
                    .collect()
            })
            .map_err(|error| {
                QQError::Unsupported(format!("cannot resolve loopback endpoint: {error}"))
            })
    }
}

fn parse_port(value: &str) -> Result<u16, QQError> {
    let port = value
        .parse::<u16>()
        .map_err(|_| QQError::Unsupported("endpoint port must be 1..65535".to_owned()))?;
    if port == 0 {
        return Err(QQError::Unsupported(
            "endpoint port must be 1..65535".to_owned(),
        ));
    }
    Ok(port)
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|address| address.is_loopback())
            .unwrap_or(false)
}

pub struct OneBotHttpAdapter {
    endpoint: LoopbackEndpoint,
    token: Option<String>,
    connected: bool,
    subscribed_groups: HashSet<String>,
    pending_messages: VecDeque<OneBotGroupMessage>,
}

/// OneBot 11 implementation name used by the spec. The concrete type keeps
/// the HTTP transport explicit so tests can continue to exercise it without
/// starting a real QQ client.
pub type OneBot11Adapter = OneBotHttpAdapter;

impl std::fmt::Debug for OneBotHttpAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OneBotHttpAdapter")
            .field("endpoint", &self.endpoint)
            .field("connected", &self.connected)
            .field("subscribed_groups", &self.subscribed_groups)
            .field("pending_messages", &self.pending_messages.len())
            .finish()
    }
}

/// A manually driven OneBot 11 reverse HTTP event listener.
///
/// It binds only to addresses accepted by `LoopbackEndpoint`, never starts a
/// background thread, and only returns an event after the caller explicitly
/// calls `accept_event`. The host can then pass the value to
/// `OneBotHttpAdapter::ingest_event`.
pub struct OneBotReverseListener {
    listener: TcpListener,
    path: String,
    token: Option<String>,
}

impl std::fmt::Debug for OneBotReverseListener {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OneBotReverseListener")
            .field("local_addr", &self.listener.local_addr().ok())
            .field("path", &self.path)
            .field("token_configured", &self.token.is_some())
            .finish()
    }
}

impl OneBotReverseListener {
    pub fn bind(endpoint: LoopbackEndpoint, token: Option<String>) -> Result<Self, QQError> {
        validate_token(token.as_deref())?;
        let mut last_error = None;
        for address in endpoint.socket_addresses()? {
            match TcpListener::bind(address) {
                Ok(listener) => {
                    listener.set_nonblocking(false).map_err(|error| {
                        QQError::Unsupported(format!(
                            "cannot configure OneBot event listener: {error}"
                        ))
                    })?;
                    return Ok(Self {
                        listener,
                        path: endpoint.path,
                        token,
                    });
                }
                Err(error) => last_error = Some(error.to_string()),
            }
        }
        Err(QQError::Unsupported(format!(
            "cannot bind local OneBot event listener: {}",
            last_error.unwrap_or_else(|| "no loopback address resolved".to_owned())
        )))
    }

    pub fn local_addr(&self) -> Result<SocketAddr, QQError> {
        self.listener.local_addr().map_err(|error| {
            QQError::Unsupported(format!("cannot read event listener address: {error}"))
        })
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<(), QQError> {
        self.listener.set_nonblocking(nonblocking).map_err(|error| {
            QQError::Unsupported(format!("cannot configure OneBot event listener: {error}"))
        })
    }

    /// Accept one reverse HTTP event and acknowledge it to the OneBot client.
    /// The call blocks only for the current connection and has bounded headers,
    /// body size, and socket read/write timeouts.
    pub fn accept_event(&self) -> Result<Value, QQError> {
        let (stream, _) = self.listener.accept().map_err(|error| {
            QQError::Unsupported(format!("OneBot event listener accept failed: {error}"))
        })?;
        self.handle_event_stream(stream)
    }

    /// Non-blocking counterpart used by the desktop worker. `Ok(None)` means
    /// that no event has arrived yet; malformed or unauthorized requests still
    /// return an error and receive a bounded HTTP failure response.
    pub fn try_accept_event(&self) -> Result<Option<Value>, QQError> {
        match self.listener.accept() {
            Ok((stream, _)) => self.handle_event_stream(stream).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(QQError::Unsupported(format!(
                "OneBot event listener accept failed: {error}"
            ))),
        }
    }

    fn handle_event_stream(&self, mut stream: TcpStream) -> Result<Value, QQError> {
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .and_then(|_| stream.set_write_timeout(Some(IO_TIMEOUT)))
            .map_err(|error| {
                QQError::Unsupported(format!("cannot configure OneBot event socket: {error}"))
            })?;

        match read_reverse_event(&mut stream, &self.path, self.token.as_deref()) {
            Ok(event) => {
                write_event_response(&mut stream, 200, json!({"status": "ok", "retcode": 0}))?;
                Ok(event)
            }
            Err(error) => {
                let status = error.http_status();
                let message = error.public_message();
                let _ = write_event_response(
                    &mut stream,
                    status,
                    json!({"status": "failed", "retcode": status, "message": message}),
                );
                Err(error.into_qq_error())
            }
        }
    }
}

impl OneBotHttpAdapter {
    pub fn new(endpoint: LoopbackEndpoint, token: Option<String>) -> Result<Self, QQError> {
        validate_token(token.as_deref())?;
        Ok(Self {
            endpoint,
            token,
            connected: false,
            subscribed_groups: HashSet::new(),
            pending_messages: VecDeque::new(),
        })
    }

    pub fn endpoint(&self) -> &LoopbackEndpoint {
        &self.endpoint
    }

    pub fn subscribed_groups(&self) -> &HashSet<String> {
        &self.subscribed_groups
    }

    /// Parse and enqueue one reverse-post/event payload.
    ///
    /// The HTTP listener is intentionally outside this low-level action
    /// client. A host integration can pass each decoded JSON body here. Only
    /// subscribed groups with image segments are queued; text and unrelated
    /// events are ignored. This makes the event boundary testable and keeps
    /// the default desktop app side-effect free.
    pub fn ingest_event(&mut self, payload: &Value) -> Result<bool, QQError> {
        if !self.connected {
            return Err(QQError::NotConnected);
        }
        let Some(message) = parse_group_message_event(payload)? else {
            return Ok(false);
        };
        if !self.subscribed_groups.contains(&message.group_id) {
            return Ok(false);
        }
        if self.pending_messages.len() >= MAX_PENDING_MESSAGES {
            return Err(QQError::Unsupported(
                "OneBot pending message queue is full".to_owned(),
            ));
        }
        self.pending_messages.push_back(message);
        Ok(true)
    }

    pub fn next_group_message(&mut self) -> Option<OneBotGroupMessage> {
        self.pending_messages.pop_front()
    }

    pub fn pending_message_count(&self) -> usize {
        self.pending_messages.len()
    }

    fn call_action(&self, action: &str, params: Value) -> Result<Value, QQError> {
        if !self.connected {
            return Err(QQError::NotConnected);
        }
        let body = json!({"action": action, "params": params}).to_string();
        let addresses = self.endpoint.socket_addresses()?;
        let mut last_error = None;
        for address in addresses {
            match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
                Ok(mut stream) => {
                    stream
                        .set_read_timeout(Some(IO_TIMEOUT))
                        .and_then(|_| stream.set_write_timeout(Some(IO_TIMEOUT)))
                        .map_err(|error| {
                            QQError::Unsupported(format!("cannot configure OneBot socket: {error}"))
                        })?;
                    let host = if self.endpoint.host.contains(':') {
                        format!("[{}]", self.endpoint.host)
                    } else {
                        self.endpoint.host.clone()
                    };
                    let authorization = self
                        .token
                        .as_deref()
                        .map(|token| format!("Authorization: Bearer {token}\r\n"))
                        .unwrap_or_default();
                    let request = format!(
                        "POST {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}\r\n{}",
                        self.endpoint.path,
                        host,
                        self.endpoint.port,
                        body.len(),
                        authorization,
                        body
                    );
                    if let Err(error) = stream.write_all(request.as_bytes()) {
                        last_error = Some(error.to_string());
                        continue;
                    }
                    let response = match read_limited(&mut stream, MAX_ACTION_RESPONSE_BYTES) {
                        Ok(response) => response,
                        Err(error) => {
                            last_error = Some(error.to_string());
                            continue;
                        }
                    };
                    return parse_onebot_response(&response);
                }
                Err(error) => last_error = Some(error.to_string()),
            }
        }
        Err(QQError::RecallFailed(format!(
            "cannot connect to local OneBot endpoint: {}",
            last_error.unwrap_or_else(|| "no loopback address resolved".to_owned())
        )))
    }
}

impl QQAdapter for OneBotHttpAdapter {
    fn connect(&mut self) -> Result<(), QQError> {
        // Connecting only changes local adapter state. No network request or
        // QQ action is made until an explicit adapter method is called.
        self.connected = true;
        Ok(())
    }

    fn disconnect(&mut self) {
        self.connected = false;
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn subscribe_group_messages(&mut self, group_id: &str) -> Result<(), QQError> {
        if !self.connected {
            return Err(QQError::NotConnected);
        }
        self.subscribed_groups.insert(group_id.to_owned());
        Ok(())
    }

    fn download_image(&self, image: &QQImage) -> Result<Vec<u8>, QQError> {
        if !self.connected {
            return Err(QQError::NotConnected);
        }
        let data = self.call_action("get_image", json!({"file": image.source_key}))?;
        let object = data.as_object().ok_or_else(|| {
            QQError::Unsupported("OneBot image response is not an object".to_owned())
        })?;
        let mut candidates = Vec::new();
        for key in ["file", "url"] {
            if let Some(value) = object
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                candidates.push(value);
            }
        }
        if candidates.is_empty() {
            return Err(QQError::Unsupported(
                "OneBot image response is missing file/url".to_owned(),
            ));
        }

        let mut last_error = None;
        for candidate in candidates {
            match self.read_image_reference(candidate) {
                Ok(bytes) => return Ok(bytes),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            QQError::Unsupported("OneBot image reference could not be read".to_owned())
        }))
    }

    fn recall_message(&mut self, message_id: &str) -> Result<(), QQError> {
        self.call_action("delete_msg", json!({"message_id": message_id}))
            .map(|_| ())
            .map_err(|error| match error {
                QQError::RecallFailed(reason) => QQError::RecallFailed(reason),
                other => QQError::RecallFailed(other.to_string()),
            })
    }

    fn get_group_list(&self) -> Result<Vec<QQGroup>, QQError> {
        let data = self.call_action("get_group_list", json!({}))?;
        let groups = data.as_array().ok_or_else(|| {
            QQError::Unsupported("OneBot group list response is not an array".to_owned())
        })?;
        groups
            .iter()
            .map(|group| {
                Ok(QQGroup {
                    group_id: value_to_string(group.get("group_id"))?,
                    group_name: value_to_string(group.get("group_name"))?,
                })
            })
            .collect()
    }
}

impl OneBotHttpAdapter {
    fn read_image_reference(&self, reference: &str) -> Result<Vec<u8>, QQError> {
        if reference.starts_with("http://") {
            return self.fetch_loopback_image(reference);
        }
        if reference.starts_with("https://") || reference.starts_with("file://") {
            return Err(QQError::Unsupported(
                "OneBot image URL must be a local file or loopback http URL".to_owned(),
            ));
        }
        let metadata = fs::metadata(reference)
            .map_err(|error| QQError::ImageNotFound(format!("{} ({error})", reference)))?;
        if !metadata.is_file() {
            return Err(QQError::Unsupported(
                "OneBot image reference is not a regular file".to_owned(),
            ));
        }
        let size = usize::try_from(metadata.len()).map_err(|_| {
            QQError::Unsupported("OneBot image file size is not supported".to_owned())
        })?;
        if size > MAX_IMAGE_BYTES {
            return Err(QQError::Unsupported(format!(
                "OneBot image exceeds the {} MiB limit",
                MAX_IMAGE_BYTES / (1024 * 1024)
            )));
        }
        fs::read(reference)
            .map_err(|error| QQError::ImageNotFound(format!("{} ({error})", reference)))
    }

    fn fetch_loopback_image(&self, url: &str) -> Result<Vec<u8>, QQError> {
        let endpoint = LoopbackEndpoint::parse(url)?;
        let host = if endpoint.host.contains(':') {
            format!("[{}]", endpoint.host)
        } else {
            endpoint.host.clone()
        };
        let authorization = self
            .token
            .as_deref()
            .map(|token| format!("Authorization: Bearer {token}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n{}\r\n",
            endpoint.path, host, endpoint.port, authorization
        );

        let addresses = endpoint.socket_addresses()?;
        let mut last_error = None;
        for address in addresses {
            match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
                Ok(mut stream) => {
                    stream
                        .set_read_timeout(Some(IO_TIMEOUT))
                        .and_then(|_| stream.set_write_timeout(Some(IO_TIMEOUT)))
                        .map_err(|error| {
                            QQError::Unsupported(format!(
                                "cannot configure OneBot image socket: {error}"
                            ))
                        })?;
                    if let Err(error) = stream.write_all(request.as_bytes()) {
                        last_error = Some(error.to_string());
                        continue;
                    }
                    let response = read_limited(&mut stream, MAX_HTTP_RESPONSE_BYTES)?;
                    return parse_binary_http_response(&response, MAX_IMAGE_BYTES);
                }
                Err(error) => last_error = Some(error.to_string()),
            }
        }
        Err(QQError::ImageNotFound(format!(
            "cannot fetch local OneBot image URL: {}",
            last_error.unwrap_or_else(|| "no loopback address resolved".to_owned())
        )))
    }
}

fn validate_token(token: Option<&str>) -> Result<(), QQError> {
    if token.is_some_and(|value| value.contains('\r') || value.contains('\n')) {
        return Err(QQError::Unsupported(
            "OneBot token contains a newline".to_owned(),
        ));
    }
    Ok(())
}

/// Parse a OneBot 11 event into the smallest message shape required by the
/// QQ pipeline. A valid non-group event or a group message without images is
/// `Ok(None)`; malformed image-bearing events return an error instead of
/// being silently treated as a usable message.
pub fn parse_group_message_event(payload: &Value) -> Result<Option<OneBotGroupMessage>, QQError> {
    if payload.get("post_type").and_then(Value::as_str) != Some("message")
        || payload.get("message_type").and_then(Value::as_str) != Some("group")
    {
        return Ok(None);
    }

    let message_id = value_to_string(payload.get("message_id"))?;
    let group_id = value_to_string(payload.get("group_id"))?;
    let segments = match payload.get("message") {
        Some(Value::Array(segments)) => segments,
        Some(Value::String(_)) | None => return Ok(None),
        _ => {
            return Err(QQError::Unsupported(
                "OneBot group message field is not an array".to_owned(),
            ))
        }
    };

    let mut images = Vec::new();
    for segment in segments {
        if segment.get("type").and_then(Value::as_str) != Some("image") {
            continue;
        }
        let data = segment
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                QQError::Unsupported("OneBot image segment is missing data".to_owned())
            })?;
        let file = data
            .get("file")
            .or_else(|| data.get("url"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                QQError::Unsupported("OneBot image segment is missing file".to_owned())
            })?;
        let image_id = data
            .get("file_id")
            .or_else(|| data.get("file"))
            .or_else(|| data.get("url"))
            .map(|value| value_to_string(Some(value)))
            .transpose()?
            .unwrap_or_else(|| file.to_owned());
        images.push(QQImage {
            image_id,
            source_key: file.to_owned(),
        });
    }

    if images.is_empty() {
        return Ok(None);
    }
    Ok(Some(OneBotGroupMessage {
        message_id,
        group_id,
        images,
    }))
}

fn value_to_string(value: Option<&Value>) -> Result<String, QQError> {
    match value {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(Value::Number(value)) => Ok(value.to_string()),
        _ => Err(QQError::Unsupported(
            "OneBot response is missing a required field".to_owned(),
        )),
    }
}

#[derive(Debug)]
enum ReverseEventError {
    BadRequest(String),
    Unauthorized,
    Io(String),
}

impl ReverseEventError {
    fn http_status(&self) -> u16 {
        match self {
            Self::BadRequest(_) => 400,
            Self::Unauthorized => 401,
            Self::Io(_) => 500,
        }
    }

    fn public_message(&self) -> &str {
        match self {
            Self::BadRequest(message) => message,
            Self::Unauthorized => "unauthorized",
            Self::Io(_) => "event listener I/O failure",
        }
    }

    fn into_qq_error(self) -> QQError {
        match self {
            Self::BadRequest(message) => QQError::Unsupported(message),
            Self::Unauthorized => {
                QQError::Unsupported("OneBot event authorization failed".to_owned())
            }
            Self::Io(message) => QQError::Unsupported(message),
        }
    }
}

fn read_reverse_event(
    stream: &mut TcpStream,
    expected_path: &str,
    token: Option<&str>,
) -> Result<Value, ReverseEventError> {
    const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
    const MAX_HEADER_BYTES: usize = 64 * 1024;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 8192];
    let (header_end, content_length) = loop {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| ReverseEventError::Io(format!("OneBot event read failed: {error}")))?;
        if count == 0 {
            return Err(ReverseEventError::BadRequest(
                "OneBot event request ended before headers".to_owned(),
            ));
        }
        request.extend_from_slice(&buffer[..count]);
        let Some(separator) = find_header_end(&request) else {
            if request.len() > MAX_HEADER_BYTES {
                return Err(ReverseEventError::BadRequest(
                    "OneBot event headers exceed the size limit".to_owned(),
                ));
            }
            continue;
        };
        let header_text = String::from_utf8_lossy(&request[..separator]);
        let mut lines = header_text.split("\r\n");
        let request_line = lines.next().unwrap_or_default();
        let mut request_parts = request_line.split_whitespace();
        if request_parts.next() != Some("POST") {
            return Err(ReverseEventError::BadRequest(
                "OneBot event method must be POST".to_owned(),
            ));
        }
        if request_parts.next() != Some(expected_path) {
            return Err(ReverseEventError::BadRequest(
                "OneBot event path does not match the configured path".to_owned(),
            ));
        }
        let mut declared_length = None;
        let mut authorization = None;
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.eq_ignore_ascii_case("content-length") {
                declared_length = Some(value.trim().parse::<usize>().map_err(|_| {
                    ReverseEventError::BadRequest(
                        "OneBot event content length is invalid".to_owned(),
                    )
                })?);
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_owned());
            }
        }
        let content_length = declared_length.ok_or_else(|| {
            ReverseEventError::BadRequest("OneBot event must provide Content-Length".to_owned())
        })?;
        if content_length > MAX_REQUEST_BYTES {
            return Err(ReverseEventError::BadRequest(
                "OneBot event body exceeds the size limit".to_owned(),
            ));
        }
        if token
            .map(|expected| authorization.as_deref() != Some(&format!("Bearer {expected}")))
            .unwrap_or(false)
        {
            return Err(ReverseEventError::Unauthorized);
        }
        break (separator, content_length);
    };

    let total_length = header_end
        .checked_add(4)
        .and_then(|value| value.checked_add(content_length))
        .ok_or_else(|| ReverseEventError::BadRequest("OneBot event size overflow".to_owned()))?;
    while request.len() < total_length {
        let count = stream.read(&mut buffer).map_err(|error| {
            ReverseEventError::Io(format!("OneBot event body read failed: {error}"))
        })?;
        if count == 0 {
            return Err(ReverseEventError::BadRequest(
                "OneBot event body ended early".to_owned(),
            ));
        }
        request.extend_from_slice(&buffer[..count]);
        if request.len() > total_length {
            // Extra pipelined bytes are not consumed or interpreted.
            break;
        }
    }
    let body_start = header_end + 4;
    let body_end = body_start + content_length;
    serde_json::from_slice(&request[body_start..body_end]).map_err(|error| {
        ReverseEventError::BadRequest(format!("OneBot event JSON is invalid: {error}"))
    })
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn write_event_response(
    stream: &mut TcpStream,
    status: u16,
    payload: Value,
) -> Result<(), QQError> {
    let body = serde_json::to_vec(&payload).map_err(|error| {
        QQError::Unsupported(format!("cannot encode OneBot event response: {error}"))
    })?;
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        _ => "Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .and_then(|_| stream.write_all(&body))
        .map_err(|error| QQError::Unsupported(format!("cannot acknowledge OneBot event: {error}")))
}

fn read_limited<R: Read>(reader: &mut R, max_bytes: usize) -> Result<Vec<u8>, QQError> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| QQError::Unsupported(format!("OneBot HTTP read failed: {error}")))?;
        if count == 0 {
            break;
        }
        if bytes.len().saturating_add(count) > max_bytes {
            return Err(QQError::Unsupported(format!(
                "OneBot HTTP response exceeds the {} byte limit",
                max_bytes
            )));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}

fn parse_binary_http_response(response: &[u8], max_body_bytes: usize) -> Result<Vec<u8>, QQError> {
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| QQError::Unsupported("invalid OneBot image HTTP response".to_owned()))?;
    let header_bytes = &response[..separator];
    let body = &response[separator + 4..];
    let headers = String::from_utf8_lossy(header_bytes);
    let status_line = headers.lines().next().unwrap_or_default();
    if !status_line.contains(" 200 ") {
        return Err(QQError::ImageNotFound(format!(
            "OneBot image request returned {status_line}"
        )));
    }
    for line in headers.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            return Err(QQError::Unsupported(
                "chunked OneBot image responses are unsupported".to_owned(),
            ));
        }
        if name.eq_ignore_ascii_case("content-length") {
            let declared = value.trim().parse::<usize>().map_err(|_| {
                QQError::Unsupported("invalid OneBot image content length".to_owned())
            })?;
            if declared > max_body_bytes {
                return Err(QQError::Unsupported(format!(
                    "OneBot image exceeds the {} MiB limit",
                    max_body_bytes / (1024 * 1024)
                )));
            }
            if declared != body.len() {
                return Err(QQError::Unsupported(
                    "OneBot image content length does not match body".to_owned(),
                ));
            }
        }
    }
    if body.len() > max_body_bytes {
        return Err(QQError::Unsupported(format!(
            "OneBot image exceeds the {} MiB limit",
            max_body_bytes / (1024 * 1024)
        )));
    }
    Ok(body.to_vec())
}

fn parse_onebot_response(response: &[u8]) -> Result<Value, QQError> {
    let response = String::from_utf8_lossy(response);
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| QQError::Unsupported("invalid OneBot HTTP response".to_owned()))?;
    let status_line = headers.lines().next().unwrap_or_default();
    if !status_line.contains(" 200 ") {
        return Err(QQError::RecallFailed(format!(
            "OneBot returned {status_line}"
        )));
    }
    let payload: Value = serde_json::from_str(body)
        .map_err(|error| QQError::Unsupported(format!("invalid OneBot JSON response: {error}")))?;
    let object = payload.as_object().ok_or_else(|| {
        QQError::Unsupported("OneBot action response is not an object".to_owned())
    })?;
    let retcode = object
        .get("retcode")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            QQError::Unsupported("OneBot action response is missing retcode".to_owned())
        })?;
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            QQError::Unsupported("OneBot action response is missing status".to_owned())
        })?;
    if retcode != 0 || !matches!(status, "ok" | "async") {
        let message = object
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("OneBot action failed");
        return Err(QQError::RecallFailed(message.to_owned()));
    }
    Ok(object.get("data").cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::{
        is_loopback_host, parse_group_message_event, parse_onebot_response, LoopbackEndpoint,
        OneBotHttpAdapter, OneBotReverseListener,
    };
    use crate::qq::{QQAdapter, QQError};
    use serde_json::json;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn endpoint_accepts_only_loopback_http() {
        let endpoint = LoopbackEndpoint::parse("http://127.0.0.1:5700/api").unwrap();
        assert_eq!(endpoint.display_url(), "http://127.0.0.1:5700/api");
        assert!(LoopbackEndpoint::parse("http://localhost").is_ok());
        assert!(LoopbackEndpoint::parse("http://[::1]:5700").is_ok());
        assert!(LoopbackEndpoint::parse("https://127.0.0.1:5700").is_err());
        assert!(LoopbackEndpoint::parse("http://8.8.8.8:5700").is_err());
        assert!(LoopbackEndpoint::parse("http://user:pass@127.0.0.1:5700").is_err());
        assert!(LoopbackEndpoint::parse("http://127.0.0.1:0").is_err());
        assert!(is_loopback_host("127.0.0.1"));
        assert!(!is_loopback_host("example.com"));
    }

    #[test]
    fn adapter_is_disconnected_and_side_effect_free_by_default() {
        let endpoint = LoopbackEndpoint::parse("http://127.0.0.1:5700").unwrap();
        let mut adapter = OneBotHttpAdapter::new(endpoint, None).unwrap();
        assert!(!adapter.is_connected());
        assert_eq!(adapter.get_group_list(), Err(QQError::NotConnected));
        adapter.connect().unwrap();
        assert!(adapter.is_connected());
        assert_eq!(adapter.subscribe_group_messages("group-1"), Ok(()));
        assert!(adapter.subscribed_groups().contains("group-1"));
        assert!(OneBotHttpAdapter::new(
            LoopbackEndpoint::parse("http://127.0.0.1:5700").unwrap(),
            Some("bad\ntoken".to_owned())
        )
        .is_err());
        let adapter = OneBotHttpAdapter::new(
            LoopbackEndpoint::parse("http://127.0.0.1:5700").unwrap(),
            Some("secret-token".to_owned()),
        )
        .unwrap();
        assert!(!format!("{adapter:?}").contains("secret-token"));
    }

    #[test]
    fn response_parser_rejects_failed_actions_and_reads_data() {
        let success = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"status\":\"ok\",\"retcode\":0,\"data\":[{\"group_id\":1,\"group_name\":\"test\"}]}";
        assert_eq!(
            parse_onebot_response(success).unwrap(),
            json!([{"group_id":1,"group_name":"test"}])
        );
        let failure = b"HTTP/1.1 200 OK\r\n\r\n{\"status\":\"failed\",\"retcode\":100,\"message\":\"denied\"}";
        assert!(
            matches!(parse_onebot_response(failure), Err(QQError::RecallFailed(reason)) if reason == "denied")
        );
        assert!(matches!(
            parse_onebot_response(b"HTTP/1.1 200 OK\r\n\r\n{}"),
            Err(QQError::Unsupported(reason)) if reason.contains("missing retcode")
        ));
        assert!(matches!(
            parse_onebot_response(b"HTTP/1.1 200 OK\r\n\r\n[]"),
            Err(QQError::Unsupported(reason)) if reason.contains("not an object")
        ));
    }

    #[test]
    fn parses_only_group_messages_with_image_segments() {
        let event = json!({
            "post_type": "message",
            "message_type": "group",
            "message_id": 42,
            "group_id": "group-1",
            "message": [
                {"type": "text", "data": {"text": "hello"}},
                {"type": "image", "data": {"file": "cache/file-a.gif", "file_id": "img-a"}},
                {"type": "image", "data": {"url": "http://127.0.0.1:5700/file-b.png"}}
            ]
        });
        let parsed = parse_group_message_event(&event).unwrap().unwrap();
        assert_eq!(parsed.message_id, "42");
        assert_eq!(parsed.group_id, "group-1");
        assert_eq!(parsed.images.len(), 2);
        assert_eq!(parsed.images[0].image_id, "img-a");
        assert_eq!(parsed.images[0].source_key, "cache/file-a.gif");
        assert_eq!(
            parsed.images[1].source_key,
            "http://127.0.0.1:5700/file-b.png"
        );

        let text_only = json!({
            "post_type": "message",
            "message_type": "group",
            "message_id": 43,
            "group_id": 1,
            "message": [{"type": "text", "data": {"text": "hello"}}]
        });
        assert_eq!(parse_group_message_event(&text_only).unwrap(), None);
        assert_eq!(
            parse_group_message_event(&json!({"post_type": "notice"})).unwrap(),
            None
        );
    }

    #[test]
    fn queues_only_subscribed_group_events_after_connection() {
        let endpoint = LoopbackEndpoint::parse("http://127.0.0.1:5700").unwrap();
        let mut adapter = OneBotHttpAdapter::new(endpoint, None).unwrap();
        let event = json!({
            "post_type": "message",
            "message_type": "group",
            "message_id": 42,
            "group_id": "group-1",
            "message": [{"type": "image", "data": {"file": "file-a"}}]
        });
        assert_eq!(adapter.ingest_event(&event), Err(QQError::NotConnected));
        adapter.connect().unwrap();
        assert!(!adapter.ingest_event(&event).unwrap());
        adapter.subscribe_group_messages("group-1").unwrap();
        assert!(adapter.ingest_event(&event).unwrap());
        assert_eq!(adapter.pending_message_count(), 1);
        assert_eq!(adapter.next_group_message().unwrap().message_id, "42");
        assert_eq!(adapter.pending_message_count(), 0);
    }

    #[test]
    fn event_queue_has_a_bounded_fail_closed_capacity() {
        let endpoint = LoopbackEndpoint::parse("http://127.0.0.1:5700").unwrap();
        let mut adapter = OneBotHttpAdapter::new(endpoint, None).unwrap();
        adapter.connect().unwrap();
        adapter.subscribe_group_messages("group-1").unwrap();
        let event = json!({
            "post_type": "message",
            "message_type": "group",
            "message_id": 42,
            "group_id": "group-1",
            "message": [{"type": "image", "data": {"file": "file-a"}}]
        });
        for _ in 0..512 {
            assert!(adapter.ingest_event(&event).unwrap());
        }
        assert_eq!(adapter.pending_message_count(), 512);
        assert!(matches!(
            adapter.ingest_event(&event),
            Err(QQError::Unsupported(reason)) if reason.contains("queue is full")
        ));
        assert_eq!(adapter.pending_message_count(), 512);
    }

    #[test]
    fn malformed_image_segment_is_rejected() {
        let event = json!({
            "post_type": "message",
            "message_type": "group",
            "message_id": 42,
            "group_id": 1,
            "message": [{"type": "image", "data": {}}]
        });
        assert!(matches!(
            parse_group_message_event(&event),
            Err(QQError::Unsupported(reason)) if reason.contains("missing file")
        ));
    }

    #[test]
    fn reverse_listener_accepts_a_local_event_and_acknowledges_it() {
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let endpoint =
            LoopbackEndpoint::parse(&format!("http://127.0.0.1:{port}/onebot/events")).unwrap();
        let listener =
            OneBotReverseListener::bind(endpoint, Some("test-token".to_owned())).unwrap();
        let address = listener.local_addr().unwrap();
        let body = json!({
            "post_type": "message",
            "message_type": "group",
            "message_id": 77,
            "group_id": 88,
            "message": [{"type": "image", "data": {"file": "image-77"}}]
        })
        .to_string();
        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            let request = format!(
                "POST /onebot/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer test-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            String::from_utf8(response).unwrap()
        });

        let event = listener.accept_event().unwrap();
        assert_eq!(event["message_id"], 77);
        let response = client.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("\"retcode\":0"));
    }

    #[test]
    fn reverse_listener_rejects_bad_tokens_without_exposing_them() {
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let endpoint = LoopbackEndpoint::parse(&format!("http://127.0.0.1:{port}/events")).unwrap();
        let listener =
            OneBotReverseListener::bind(endpoint, Some("secret-token".to_owned())).unwrap();
        let address = listener.local_addr().unwrap();
        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            let body = b"{}";
            let request = format!(
                "POST /events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer wrong-token\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                String::from_utf8_lossy(body)
            );
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            String::from_utf8(response).unwrap()
        });

        let error = listener.accept_event().unwrap_err();
        assert!(error.to_string().contains("authorization"));
        assert!(!error.to_string().contains("secret-token"));
        let response = client.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 401 Unauthorized"));
        assert!(!response.contains("secret-token"));
    }

    #[test]
    fn http_adapter_executes_bounded_loopback_actions() {
        let image_path = std::env::temp_dir().join(format!(
            "nlnf-onebot-action-test-{}-{}.bin",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&image_path, [1_u8, 2, 3, 4]).unwrap();
        let image_path_for_server = image_path.to_string_lossy().into_owned();
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let server_thread = thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = server.accept().unwrap();
                let body = read_test_request_body(&mut stream);
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let data = match request["action"].as_str().unwrap() {
                    "get_group_list" => json!([{"group_id": 123, "group_name": "test-group"}]),
                    "get_image" => json!({"file": image_path_for_server}),
                    "delete_msg" => serde_json::Value::Null,
                    action => panic!("unexpected action: {action}"),
                };
                let response_body = json!({"status": "ok", "retcode": 0, "data": data});
                write_test_response(&mut stream, response_body);
            }
        });

        let endpoint = LoopbackEndpoint::parse(&format!("http://127.0.0.1:{port}/api")).unwrap();
        let mut adapter = OneBotHttpAdapter::new(endpoint, Some("test-token".to_owned())).unwrap();
        adapter.connect().unwrap();
        let groups = adapter.get_group_list().unwrap();
        assert_eq!(groups[0].group_id, "123");
        let bytes = adapter
            .download_image(&crate::qq::QQImage {
                image_id: "image-1".to_owned(),
                source_key: "qq-file-id".to_owned(),
            })
            .unwrap();
        assert_eq!(bytes, vec![1, 2, 3, 4]);
        adapter.recall_message("message-1").unwrap();
        server_thread.join().unwrap();
        fs::remove_file(image_path).unwrap();
    }

    fn read_test_request_body(stream: &mut TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0, "test HTTP request ended before its body");
            request.extend_from_slice(&buffer[..count]);
            let Some(separator) = super::find_header_end(&request) else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..separator]);
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let body_start = separator + 4;
            if request.len() >= body_start + length {
                return request[body_start..body_start + length].to_vec();
            }
        }
    }

    fn write_test_response(stream: &mut TcpStream, payload: serde_json::Value) {
        let body = serde_json::to_vec(&payload).unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
    }
}
