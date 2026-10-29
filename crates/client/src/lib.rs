//! The connecting side of a session.
//!
//! [`connect`] dials a host by User-ID, waits while the host user is asked to allow it, then
//! receives and decodes the host's screen. Progress arrives as [`ClientEvent`]s; the newest
//! decoded frame and live [`Stats`] are read from the [`ClientHandle`].

mod video;

use std::{
    fmt,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use connect_net::{self as net, PathKind};
use connect_proto::{
    CLOSE_NORMAL, CLOSE_REJECTED, PROTOCOL_VERSION, Permissions,
    control::{ClientMsg, HostMsg},
    now_micros,
    video::FrameMeta,
};
use iroh::{
    Endpoint, EndpointAddr,
    endpoint::{Connection, ConnectionError, RecvStream, SendStream},
};
use tokio::sync::{Notify, mpsc};
use tracing::debug;

const PING_INTERVAL: Duration = Duration::from_secs(1);
/// Lower bound between keyframe requests; each one costs the host a large frame.
const KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Looking up the User-ID and reaching the host.
    Connecting,
    /// Reached the host; its user is being asked to allow the connection.
    WaitingForApproval,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    Stage(Stage),
    Accepted { host_name: String, width: u32, height: u32, permissions: Permissions },
    PermissionsChanged(Permissions),
    VideoFormat { width: u32, height: u32 },
    /// Always the last event.
    Ended(EndReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// The host turned the connection down; the text says why.
    Rejected(String),
    /// Couldn't reach the host, or the connection broke during setup.
    Failed(String),
    /// The host ended the session.
    HostEnded(String),
    /// [`ClientHandle::disconnect`] was called.
    Disconnected,
    /// The network path to the host died.
    ConnectionLost,
}

impl fmt::Display for EndReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EndReason::Rejected(reason) | EndReason::HostEnded(reason) => f.write_str(reason),
            EndReason::Failed(error) => write!(f, "Couldn't connect: {error}"),
            EndReason::Disconnected => f.write_str("Disconnected."),
            EndReason::ConnectionLost => f.write_str("Lost the connection to the host."),
        }
    }
}

/// One decoded picture, tightly packed RGBA.
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub meta: FrameMeta,
}

/// Figures for the stats overlay, refreshed about once a second.
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub path: PathKind,
    pub rtt: Option<Duration>,
    pub fps: f32,
    pub mbps: f32,
    pub frames_lost: u64,
    pub encode_ms: f32,
    pub decode_ms: f32,
    /// Host capture → decoded here, via the estimated clock offset between the two PCs.
    pub latency_ms: Option<f32>,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            path: PathKind::Unknown,
            rtt: None,
            fps: 0.0,
            mbps: 0.0,
            frames_lost: 0,
            encode_ms: 0.0,
            decode_ms: 0.0,
            latency_ms: None,
        }
    }
}

/// Controls a connection attempt or session. Cheap to clone.
#[derive(Clone)]
pub struct ClientHandle(Arc<Shared>);

struct Shared {
    wake: Box<dyn Fn() + Send + Sync>,
    disconnect: Notify,
    disconnected: AtomicBool,
    conn: Mutex<Option<Connection>>,
    frame: Mutex<Option<VideoFrame>>,
    stats: Mutex<Stats>,
    counters: Counters,
}

/// Written by the receive and decode loops, summarised into [`Stats`] by the control loop.
#[derive(Default)]
struct Counters {
    bytes: AtomicU64,
    frames_decoded: AtomicU64,
    frames_lost: AtomicU64,
    encode_us: AtomicU64,
    decode_us: AtomicU64,
    /// Client clock minus host clock, µs; `i64::MIN` until measured.
    clock_offset_us: AtomicI64,
    latency_us: AtomicI64,
}

/// Starts connecting to `host`. `wake` is called (from any thread) whenever there's a new event
/// or frame, e.g. to request a repaint.
pub fn connect(
    endpoint: Endpoint,
    host: impl Into<EndpointAddr>,
    client_name: impl Into<String>,
    wake: impl Fn() + Send + Sync + 'static,
) -> (ClientHandle, mpsc::Receiver<ClientEvent>) {
    let shared = Arc::new(Shared {
        wake: Box::new(wake),
        disconnect: Notify::new(),
        disconnected: AtomicBool::new(false),
        conn: Mutex::new(None),
        frame: Mutex::new(None),
        stats: Mutex::new(Stats::default()),
        counters: Counters {
            clock_offset_us: AtomicI64::new(i64::MIN),
            latency_us: AtomicI64::new(i64::MIN),
            ..Default::default()
        },
    });
    let (events_tx, events_rx) = mpsc::channel(32);
    let events = Events { tx: events_tx, shared: shared.clone() };
    let (host, client_name) = (host.into(), client_name.into());
    let task_shared = shared.clone();
    tokio::spawn(async move {
        let reason = run(&endpoint, host, &client_name, &task_shared, &events).await;
        debug!("session ended: {reason}");
        events.send(ClientEvent::Ended(reason)).await;
    });
    (ClientHandle(shared), events_rx)
}

impl ClientHandle {
    /// The newest decoded frame, if one arrived since the last call.
    pub fn take_frame(&self) -> Option<VideoFrame> {
        lock(&self.0.frame).take()
    }

    pub fn stats(&self) -> Stats {
        *lock(&self.0.stats)
    }

    /// Hangs up, or stops connecting. [`ClientEvent::Ended`] follows.
    pub fn disconnect(&self) {
        self.0.disconnected.store(true, Ordering::Relaxed);
        self.0.disconnect.notify_one();
        if let Some(conn) = &*lock(&self.0.conn) {
            conn.close(CLOSE_NORMAL.into(), b"disconnected");
        }
    }
}

struct Events {
    tx: mpsc::Sender<ClientEvent>,
    shared: Arc<Shared>,
}

impl Events {
    async fn send(&self, event: ClientEvent) {
        let _ = self.tx.send(event).await;
        (self.shared.wake)();
    }
}

async fn run(endpoint: &Endpoint, host: EndpointAddr, client_name: &str, shared: &Arc<Shared>, events: &Events) -> EndReason {
    events.send(ClientEvent::Stage(Stage::Connecting)).await;
    let conn = tokio::select! {
        conn = net::connect(endpoint, host) => match conn {
            Ok(conn) => conn,
            Err(e) => return EndReason::Failed(format!("{e:#}")),
        },
        _ = shared.disconnect.notified() => return EndReason::Disconnected,
    };
    *lock(&shared.conn) = Some(conn.clone());
    if shared.disconnected.load(Ordering::Relaxed) {
        conn.close(CLOSE_NORMAL.into(), b"disconnected");
        return EndReason::Disconnected;
    }

    let result = session(&conn, client_name, shared, events).await;
    let reason = match (conn.close_reason(), result) {
        (_, Ok(Some(reason))) => reason,
        (Some(closed), _) => describe_close(&closed),
        (None, Err(e)) => EndReason::Failed(format!("{e:#}")),
        (None, Ok(None)) => EndReason::HostEnded("The host ended the session.".into()),
    };
    conn.close(CLOSE_NORMAL.into(), b"bye");
    *lock(&shared.conn) = None;
    reason
}

/// Runs the handshake and, once allowed, the session. `Ok(Some(reason))` when the host said
/// why it ended; otherwise the reason comes from how the connection closed.
async fn session(conn: &Connection, client_name: &str, shared: &Arc<Shared>, events: &Events) -> Result<Option<EndReason>> {
    let (mut send, mut recv) = conn.open_bi().await?;
    let hello = ClientMsg::Hello { version: PROTOCOL_VERSION, client_name: client_name.to_owned() };
    net::write_msg(&mut send, &hello).await?;
    events.send(ClientEvent::Stage(Stage::WaitingForApproval)).await;

    let reply = tokio::select! {
        reply = net::read_msg::<HostMsg>(&mut recv) => reply,
        _ = shared.disconnect.notified() => return Ok(Some(EndReason::Disconnected)),
    };
    match reply {
        Ok(Some(HostMsg::Welcome { host_name, width, height, permissions })) => {
            events.send(ClientEvent::Accepted { host_name, width, height, permissions }).await;
        }
        Ok(Some(HostMsg::Rejected { reason })) => return Ok(Some(EndReason::Rejected(reason))),
        Ok(Some(other)) => bail!("unexpected reply from host: {other:?}"),
        Ok(None) => bail!("the host closed the connection"),
        // If the host closed before its reason arrived, `run` takes the reason from the close.
        Err(e) => return Err(e),
    }

    stream(conn, send, recv, shared, events).await
}

async fn stream(
    conn: &Connection,
    mut send: SendStream,
    recv: RecvStream,
    shared: &Arc<Shared>,
    events: &Events,
) -> Result<Option<EndReason>> {
    let (keyframe_tx, mut keyframe_rx) = mpsc::unbounded_channel();
    let frames = video::start_decoder(shared.clone(), keyframe_tx.clone())?;
    tokio::spawn(video::receive(conn.clone(), frames, keyframe_tx, shared.clone()));

    // Reading isn't cancel-safe (a half-read message would be lost), so it gets its own task.
    let (msg_tx, mut msgs) = mpsc::channel(16);
    tokio::spawn(read_loop(recv, msg_tx));

    let mut ping = tokio::time::interval(PING_INTERVAL);
    let mut last_keyframe_request: Option<Instant> = None;
    let mut window = StatsWindow::new(shared);
    let mut best_rtt_us = u64::MAX;

    loop {
        tokio::select! {
            msg = msgs.recv() => match msg {
                Some(Ok(HostMsg::Pong { client_time_us, host_time_us })) => {
                    let now = now_micros();
                    let rtt = now.saturating_sub(client_time_us);
                    // The fastest round trip gives the tightest bound on the clock offset.
                    if rtt <= best_rtt_us {
                        best_rtt_us = rtt;
                        let offset = (client_time_us + rtt / 2) as i64 - host_time_us as i64;
                        shared.counters.clock_offset_us.store(offset, Ordering::Relaxed);
                    }
                }
                Some(Ok(HostMsg::PermissionsChanged(p))) => events.send(ClientEvent::PermissionsChanged(p)).await,
                Some(Ok(HostMsg::VideoFormat { width, height })) => {
                    events.send(ClientEvent::VideoFormat { width, height }).await;
                }
                Some(Ok(HostMsg::Rejected { reason })) => return Ok(Some(EndReason::HostEnded(reason))),
                Some(Ok(other @ HostMsg::Welcome { .. })) => bail!("unexpected message from host: {other:?}"),
                Some(Err(e)) => return Err(e),
                None => return Ok(None),
            },
            Some(()) = keyframe_rx.recv() => {
                if last_keyframe_request.is_none_or(|t| t.elapsed() >= KEYFRAME_REQUEST_INTERVAL) {
                    last_keyframe_request = Some(Instant::now());
                    debug!("requesting a keyframe");
                    net::write_msg(&mut send, &ClientMsg::RequestKeyframe).await?;
                }
            }
            _ = ping.tick() => {
                net::write_msg(&mut send, &ClientMsg::Ping { client_time_us: now_micros() }).await?;
                window.update(conn, shared);
            }
            _ = conn.closed() => return Ok(None),
            _ = shared.disconnect.notified() => return Ok(Some(EndReason::Disconnected)),
        }
    }
}

async fn read_loop(mut recv: RecvStream, tx: mpsc::Sender<Result<HostMsg>>) {
    loop {
        match net::read_msg::<HostMsg>(&mut recv).await {
            Ok(Some(msg)) => {
                if tx.send(Ok(msg)).await.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(e) => {
                let _ = tx.send(Err(e)).await;
                return;
            }
        }
    }
}

fn describe_close(error: &ConnectionError) -> EndReason {
    match error {
        ConnectionError::LocallyClosed => EndReason::Disconnected,
        ConnectionError::ApplicationClosed(close) => {
            let reason = String::from_utf8_lossy(&close.reason).into_owned();
            if close.error_code == CLOSE_REJECTED.into() {
                EndReason::Rejected(reason)
            } else if close.error_code == CLOSE_NORMAL.into() && !reason.is_empty() {
                EndReason::HostEnded(reason)
            } else {
                EndReason::HostEnded("The host ended the session.".into())
            }
        }
        ConnectionError::TimedOut | ConnectionError::Reset => EndReason::ConnectionLost,
        other => EndReason::Failed(other.to_string()),
    }
}

/// Turns the running counters into per-second rates.
struct StatsWindow {
    at: Instant,
    bytes: u64,
    frames: u64,
}

impl StatsWindow {
    fn new(shared: &Shared) -> Self {
        let c = &shared.counters;
        Self { at: Instant::now(), bytes: c.bytes.load(Ordering::Relaxed), frames: c.frames_decoded.load(Ordering::Relaxed) }
    }

    fn update(&mut self, conn: &Connection, shared: &Shared) {
        let c = &shared.counters;
        let secs = self.at.elapsed().as_secs_f32();
        if secs < 0.5 {
            return; // the first tick fires at once; rates need a real window
        }
        let (bytes, frames) = (c.bytes.load(Ordering::Relaxed), c.frames_decoded.load(Ordering::Relaxed));
        let (path, rtt) = net::selected_path(conn);
        let latency_us = c.latency_us.load(Ordering::Relaxed);
        *lock(&shared.stats) = Stats {
            path,
            rtt,
            fps: (frames - self.frames) as f32 / secs,
            mbps: (bytes - self.bytes) as f32 * 8.0 / secs / 1e6,
            frames_lost: c.frames_lost.load(Ordering::Relaxed),
            encode_ms: c.encode_us.load(Ordering::Relaxed) as f32 / 1000.0,
            decode_ms: c.decode_us.load(Ordering::Relaxed) as f32 / 1000.0,
            latency_ms: (latency_us != i64::MIN).then(|| latency_us.max(0) as f32 / 1000.0),
        };
        *self = Self { at: Instant::now(), bytes, frames };
        (shared.wake)();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
