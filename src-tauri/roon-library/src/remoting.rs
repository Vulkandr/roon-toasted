// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Protocol details from arthursoares/roon-api-reverse-engineering (MIT), see NOTICE.

//! The remoting layer on top of the socket: method calls with request /
//! response matching, the one-time declarations the Core needs before a call
//! (DEFMETHOD for our method ids, DEFTYPE for structs we send), keep-alive
//! replies, and the stream of pushes for the object graph.
//!
//! Method ids and type ids are ours to choose; the Core learns what they mean
//! from the declarations. Object ids are the Core's.
//!
//! One task reads the socket and dispatches; one task writes whatever the
//! others queue, so declarations always reach the Core before the call that
//! uses them.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

use crate::frame::{cmd, encode_request, encode_response, Frame, FrameParser};
use crate::graph::PropertyType;
use crate::wire::{Reader, Writer};

#[derive(Debug, Clone)]
pub struct CallResult {
    /// The Core's status string; "Success" (or empty) means it worked.
    pub status: String,
    /// The return value, if any: the bytes after the status.
    pub payload: Vec<u8>,
}

impl CallResult {
    pub fn success(&self) -> bool {
        self.status.is_empty() || self.status == "Success"
    }
}

#[derive(Debug)]
pub enum RemotingError {
    /// The connection is gone.
    Closed,
    /// The Core did not answer in time.
    Timeout,
    /// The Core answered a call with a failure status.
    Failed { method: String, status: String },
    /// The answer could not be decoded.
    BadResponse(String),
}

impl fmt::Display for RemotingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RemotingError::Closed => f.write_str("not connected to the Core"),
            RemotingError::Timeout => f.write_str("the Core did not answer in time"),
            RemotingError::Failed { method, status } => write!(f, "{method} failed: {status}"),
            RemotingError::BadResponse(m) => write!(f, "could not read the Core's answer: {m}"),
        }
    }
}

impl std::error::Error for RemotingError {}

/// A member of a by-value struct we send: its full wire name and serialized value.
pub struct StructField {
    pub name: &'static str,
    pub prop_type: PropertyType,
    pub value: Vec<u8>,
}

/// How often an idle session is pinged (the Core gives up after about 30 s).
pub const KEEPALIVE: Duration = Duration::from_secs(10);

struct Pending {
    chunks: Vec<u8>,
    done: oneshot::Sender<Vec<u8>>,
}

/// What a session had been doing when it ended, for the disconnect reason.
#[derive(Debug, Clone)]
pub struct SessionStats {
    pub age: Duration,
    pub frames_in: u64,
    pub since_last_in: Duration,
    pub frames_out: u64,
    pub since_last_out: Duration,
    pub write_error: Option<String>,
}

impl fmt::Display for SessionStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "session {}s old, {} frames in (last {:.1}s ago), {} out (last {:.1}s ago)",
            self.age.as_secs(),
            self.frames_in,
            self.since_last_in.as_secs_f64(),
            self.frames_out,
            self.since_last_out.as_secs_f64()
        )?;
        if let Some(e) = &self.write_error {
            write!(f, ", write failed: {e}")?;
        }
        Ok(())
    }
}

/// Counters the reader and writer tasks keep for `SessionStats`.
struct Counters {
    started: Instant,
    frames_in: AtomicU64,
    last_in_ms: AtomicU64,
    frames_out: AtomicU64,
    last_out_ms: AtomicU64,
    write_error: Mutex<Option<String>>,
}

impl Counters {
    fn new() -> Self {
        Counters {
            started: Instant::now(),
            frames_in: AtomicU64::new(0),
            last_in_ms: AtomicU64::new(0),
            frames_out: AtomicU64::new(0),
            last_out_ms: AtomicU64::new(0),
            write_error: Mutex::new(None),
        }
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn stats(&self) -> SessionStats {
        let now = self.now_ms();
        SessionStats {
            age: self.started.elapsed(),
            frames_in: self.frames_in.load(Ordering::Relaxed),
            since_last_in: Duration::from_millis(now.saturating_sub(self.last_in_ms.load(Ordering::Relaxed))),
            frames_out: self.frames_out.load(Ordering::Relaxed),
            since_last_out: Duration::from_millis(now.saturating_sub(self.last_out_ms.load(Ordering::Relaxed))),
            write_error: self.write_error.lock().unwrap().clone(),
        }
    }
}

pub struct Remoting {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    pending: Mutex<HashMap<u32, Pending>>,
    // Starts past the request id the ConnectRequest used, so its late response is never mistaken for ours.
    next_rid: AtomicU32,
    method_ids: Mutex<HashMap<String, u32>>,
    type_ids: Mutex<HashMap<String, u32>>,
    closed: AtomicBool,
    request_timeout: Duration,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    counters: Arc<Counters>,
}

impl Remoting {
    /// Takes over a socket that finished the handshake. `initial` is what
    /// arrived with the ConnectResponse. `on_push` sees every push from the
    /// Core; `on_close` runs once when the connection ends.
    pub fn start(
        stream: TcpStream,
        initial: Vec<u8>,
        request_timeout: Duration,
        on_push: impl Fn(&Frame) + Send + Sync + 'static,
        on_close: impl FnOnce(Option<std::io::Error>, SessionStats) + Send + 'static,
    ) -> Arc<Remoting> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let remoting = Arc::new(Remoting {
            tx,
            pending: Mutex::new(HashMap::new()),
            next_rid: AtomicU32::new(16),
            method_ids: Mutex::new(HashMap::new()),
            type_ids: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            request_timeout,
            tasks: Mutex::new(Vec::new()),
            counters: Arc::new(Counters::new()),
        });

        let (mut read_half, mut write_half) = stream.into_split();

        // Writer: everything goes out in the order it was queued.
        let counters = Arc::clone(&remoting.counters);
        let writer = tokio::spawn(async move {
            while let Some(bytes) = rx.recv().await {
                if let Err(e) = write_half.write_all(&bytes).await {
                    *counters.write_error.lock().unwrap() = Some(e.to_string());
                    break;
                }
                counters.frames_out.fetch_add(1, Ordering::Relaxed);
                counters.last_out_ms.store(counters.now_ms(), Ordering::Relaxed);
            }
            let _ = write_half.shutdown().await;
        });

        // Reader: frames in, responses matched, pings answered, pushes handed on.
        let reader_remoting = Arc::clone(&remoting);
        let reader = tokio::spawn(async move {
            let mut parser = FrameParser::new();
            let mut buf = vec![0u8; 64 * 1024];
            let mut reason: Option<std::io::Error> = None;
            let mut first = Some(initial);
            loop {
                let frames = match first.take() {
                    Some(initial) => parser.push(&initial),
                    None => match read_half.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => parser.push(&buf[..n]),
                        Err(e) => {
                            reason = Some(e);
                            break;
                        }
                    },
                };
                for frame in frames {
                    reader_remoting.counters.frames_in.fetch_add(1, Ordering::Relaxed);
                    reader_remoting
                        .counters
                        .last_in_ms
                        .store(reader_remoting.counters.now_ms(), Ordering::Relaxed);
                    reader_remoting.handle_frame(frame, &on_push);
                }
                if reader_remoting.closed.load(Ordering::SeqCst) {
                    break;
                }
            }
            let was_open = !reader_remoting.closed.swap(true, Ordering::SeqCst);
            reader_remoting.fail_pending();
            reader_remoting.abort_tasks();
            if was_open {
                on_close(reason, reader_remoting.counters.stats());
            }
        });
        // Keepalive: a PING every KEEPALIVE keeps an idle session open. If
        // the Core stops answering, the reader notices the dropped socket.
        let ping_remoting = Arc::clone(&remoting);
        let keepalive = tokio::spawn(async move {
            let mut ticks = tokio::time::interval(KEEPALIVE);
            ticks.tick().await; // the first tick is immediate
            loop {
                ticks.tick().await;
                if ping_remoting.is_closed() {
                    break;
                }
                let _ = ping_remoting.ping().await;
            }
        });
        remoting.tasks.lock().unwrap().extend([writer, reader, keepalive]);

        remoting
    }

    fn abort_tasks(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }

    /// Traffic counters for this session.
    pub fn stats(&self) -> SessionStats {
        self.counters.stats()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Ends the session deliberately (`on_close` does not run for this).
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.fail_pending();
        self.abort_tasks();
    }

    fn fail_pending(&self) {
        let pending: Vec<Pending> = self.pending.lock().unwrap().drain().map(|(_, p)| p).collect();
        drop(pending); // dropping the oneshot senders wakes the callers with an error
    }

    fn send(&self, bytes: Vec<u8>) -> Result<(), RemotingError> {
        if self.is_closed() {
            return Err(RemotingError::Closed);
        }
        self.tx.send(bytes).map_err(|_| RemotingError::Closed)
    }

    fn handle_frame(&self, frame: Frame, on_push: &(impl Fn(&Frame) + Send + Sync)) {
        if frame.is_response {
            let Some(rid) = frame.rid else { return };
            let mut pending = self.pending.lock().unwrap();
            let Some(mut p) = pending.remove(&rid) else { return }; // late or unknown
            p.chunks.extend_from_slice(&frame.body);
            if frame.is_final {
                let _ = p.done.send(p.chunks);
            } else {
                pending.insert(rid, p);
            }
            return;
        }
        if frame.cmd == cmd::PING {
            if let Some(rid) = frame.rid {
                let _ = self.send(encode_response(rid, &[], true));
            }
            return;
        }
        on_push(&frame);
        // Pushes that carry a request id want an acknowledgement.
        if let Some(rid) = frame.rid {
            let _ = self.send(encode_response(rid, &[], true));
        }
    }

    /// Our id for a method signature, declaring it to the Core on first use.
    fn method_id(&self, signature: &str) -> Result<u32, RemotingError> {
        let mut ids = self.method_ids.lock().unwrap();
        if let Some(id) = ids.get(signature) {
            return Ok(*id);
        }
        let id = ids.len() as u32 + 1;
        ids.insert(signature.to_string(), id);
        let mut w = Writer::new();
        w.flex_int(id).string(Some(signature));
        self.send(encode_request(cmd::DEFMETHOD, w.as_bytes(), None))?;
        Ok(id)
    }

    /// Our id for a by-value struct type, declaring it (with these members, in
    /// this order) on first use. Use the same member list for a type within a session.
    pub fn define_type(&self, type_name: &str, members: &[(&str, PropertyType)]) -> Result<u32, RemotingError> {
        let mut ids = self.type_ids.lock().unwrap();
        if let Some(id) = ids.get(type_name) {
            return Ok(*id);
        }
        let id = ids.len() as u32 + 1;
        ids.insert(type_name.to_string(), id);
        let mut w = Writer::new();
        w.flex_int(id).string(Some(type_name)).flex_int(members.len() as u32);
        for (name, prop_type) in members {
            w.string(Some(name)).integer(*prop_type as i32);
        }
        self.send(encode_request(cmd::DEFTYPE, w.as_bytes(), None))?;
        Ok(id)
    }

    /// Serializes a by-value struct as an inline value object:
    /// `flexLong(1) flexInt(type id) flexInt(len) (flexInt(index) value)* 0`.
    pub fn inline_struct(&self, type_name: &str, fields: &[StructField]) -> Result<Vec<u8>, RemotingError> {
        let members: Vec<(&str, PropertyType)> = fields.iter().map(|f| (f.name, f.prop_type)).collect();
        let type_id = self.define_type(type_name, &members)?;
        let mut body = Writer::new();
        for (i, f) in fields.iter().enumerate() {
            body.flex_int(i as u32 + 1).bytes(&f.value);
        }
        body.flex_int(0);
        let mut w = Writer::new();
        w.long(1).flex_int(type_id).flex_int(body.len() as u32).bytes(body.as_bytes());
        Ok(w.into_bytes())
    }

    /// Calls a method that has a result callback and waits for the Core's answer.
    pub async fn call(&self, object_id: u64, signature: &str, args: &[u8]) -> Result<CallResult, RemotingError> {
        let method_id = self.method_id(signature)?;
        let mut w = Writer::new();
        w.long(object_id).flex_int(method_id).bytes(args);
        let body = self.request(cmd::CALL, w.as_bytes()).await?;
        Ok(parse_call_result(&body))
    }

    /// Sends a request that expects a response and waits for its body.
    async fn request(&self, command: u8, body: &[u8]) -> Result<Vec<u8>, RemotingError> {
        let rid = self.next_rid.fetch_add(1, Ordering::SeqCst) & 0x7fff_ffff;
        let (done_tx, done_rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(rid, Pending { chunks: Vec::new(), done: done_tx });

        if let Err(e) = self.send(encode_request(command, body, Some(rid))) {
            self.pending.lock().unwrap().remove(&rid);
            return Err(e);
        }

        match tokio::time::timeout(self.request_timeout, done_rx).await {
            Ok(Ok(body)) => Ok(body),
            Ok(Err(_)) => Err(RemotingError::Closed),
            Err(_) => {
                self.pending.lock().unwrap().remove(&rid);
                Err(RemotingError::Timeout)
            }
        }
    }

    /// A keepalive: the Core answers with an empty response. It drops a
    /// session that has been silent for about 30 seconds (which happens as
    /// soon as nothing is playing), so `start` pings every `KEEPALIVE`.
    pub async fn ping(&self) -> Result<(), RemotingError> {
        self.request(cmd::PING, &[]).await.map(|_| ())
    }

    /// Calls a method without a result callback (the Core never answers).
    pub fn call_no_reply(&self, object_id: u64, signature: &str, args: &[u8]) -> Result<(), RemotingError> {
        let method_id = self.method_id(signature)?;
        let mut w = Writer::new();
        w.long(object_id).flex_int(method_id).bytes(args);
        self.send(encode_request(cmd::CALL, w.as_bytes(), None))
    }

    /// Resolves a service GUID to its object id.
    pub async fn get_service(&self, guid: &[u8; 16]) -> Result<u64, RemotingError> {
        let rid = self.next_rid.fetch_add(1, Ordering::SeqCst) & 0x7fff_ffff;
        let (done_tx, done_rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(rid, Pending { chunks: Vec::new(), done: done_tx });
        if let Err(e) = self.send(encode_request(cmd::GETSVC, guid, Some(rid))) {
            self.pending.lock().unwrap().remove(&rid);
            return Err(e);
        }
        let body = match tokio::time::timeout(self.request_timeout, done_rx).await {
            Ok(Ok(body)) => body,
            Ok(Err(_)) => return Err(RemotingError::Closed),
            Err(_) => {
                self.pending.lock().unwrap().remove(&rid);
                return Err(RemotingError::Timeout);
            }
        };
        let res = parse_call_result(&body);
        if !res.success() {
            return Err(RemotingError::Failed { method: "GETSVC".into(), status: res.status });
        }
        Reader::new(&res.payload)
            .flex_long()
            .map_err(|_| RemotingError::BadResponse("no object id in the GETSVC answer".into()))
    }
}

/// A response body is a status string followed by the return value.
pub fn parse_call_result(body: &[u8]) -> CallResult {
    let mut r = Reader::new(body);
    let status = r.string().ok().flatten().unwrap_or_default();
    CallResult {
        status,
        payload: r.rest().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_results() {
        let mut w = Writer::new();
        w.string(Some("Success")).byte(0xaa);
        let ok = parse_call_result(w.as_bytes());
        assert!(ok.success());
        assert_eq!(ok.payload, vec![0xaa]);
        let mut w = Writer::new();
        w.string(Some("MissingMethod"));
        let bad = parse_call_result(w.as_bytes());
        assert!(!bad.success());
        assert_eq!(bad.status, "MissingMethod");
    }
}
