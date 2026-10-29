//! The sharing side of a session.
//!
//! [`start`] accepts connections on the app's endpoint. For each one the host user is asked to
//! allow or deny it (unless the caller's User-ID is trusted); an allowed peer then gets the
//! screen as H.264 over QUIC datagrams. One session runs at a time.

mod capture;
mod encode;
mod session;

use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use connect_proto::{APPROVAL_TIMEOUT, CLOSE_NORMAL, Permissions};
use iroh::{Endpoint, EndpointId, endpoint::Connection};
use tokio::sync::{mpsc, oneshot, watch};

pub use capture::VideoSource;

/// What the host starts with. Everything except `video` and `approval_timeout` can be
/// changed later through [`HostHandle`].
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Shown to the connecting user.
    pub host_name: String,
    /// When false, every connection is turned away without asking.
    pub accepting: bool,
    pub permissions: Permissions,
    /// User-IDs that are let in without asking.
    pub trusted: HashSet<EndpointId>,
    pub video: VideoSource,
    pub approval_timeout: Duration,
}

impl HostConfig {
    pub fn new(host_name: impl Into<String>) -> Self {
        Self {
            host_name: host_name.into(),
            accepting: true,
            permissions: Permissions::default(),
            trusted: HashSet::new(),
            video: VideoSource::default(),
            approval_timeout: APPROVAL_TIMEOUT,
        }
    }
}

/// The host user's answer to a [`ConnectionRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// Someone wants to connect. Answer with [`respond`](Self::respond) before the approval
/// timeout runs out; dropping the request unanswered denies it.
#[derive(Debug)]
pub struct ConnectionRequest {
    /// The requester's User-ID, proven by the connection handshake.
    pub peer: EndpointId,
    /// The name the requester gave their PC. Not verified: anyone can claim any name.
    pub client_name: String,
    respond: oneshot::Sender<Decision>,
}

impl ConnectionRequest {
    pub fn respond(self, decision: Decision) {
        let _ = self.respond.send(decision);
    }

    /// False once the request timed out or the requester gave up, so its prompt should close.
    pub fn is_pending(&self) -> bool {
        !self.respond.is_closed()
    }
}

#[derive(Debug)]
pub enum HostEvent {
    Request(ConnectionRequest),
    SessionStarted { peer: EndpointId, client_name: String },
    SessionEnded { peer: EndpointId, reason: String },
}

/// Controls a running host. Cheap to clone.
#[derive(Clone)]
pub struct HostHandle(Arc<Shared>);

struct Shared {
    host_name: RwLock<String>,
    accepting: AtomicBool,
    permissions: watch::Sender<Permissions>,
    trusted: RwLock<HashSet<EndpointId>>,
    /// The one connection being asked about or served; anyone else is told the host is busy.
    slot: Mutex<Option<(EndpointId, Connection)>>,
    video: VideoSource,
    approval_timeout: Duration,
}

/// Starts accepting connections on `endpoint`. Events must be drained, or connection requests stall.
pub fn start(endpoint: Endpoint, config: HostConfig) -> (HostHandle, mpsc::Receiver<HostEvent>) {
    let shared = Arc::new(Shared {
        host_name: RwLock::new(config.host_name),
        accepting: AtomicBool::new(config.accepting),
        permissions: watch::Sender::new(config.permissions),
        trusted: RwLock::new(config.trusted),
        slot: Mutex::new(None),
        video: config.video,
        approval_timeout: config.approval_timeout,
    });
    let (events_tx, events_rx) = mpsc::channel(16);
    tokio::spawn(accept_loop(endpoint, shared.clone(), events_tx));
    (HostHandle(shared), events_rx)
}

async fn accept_loop(endpoint: Endpoint, shared: Arc<Shared>, events: mpsc::Sender<HostEvent>) {
    while let Some(incoming) = endpoint.accept().await {
        tokio::spawn(session::run(incoming, shared.clone(), events.clone()));
    }
    tracing::debug!("endpoint closed; host stopped");
}

impl HostHandle {
    pub fn set_host_name(&self, name: impl Into<String>) {
        *self.0.host_name.write().unwrap_or_else(PoisonError::into_inner) = name.into();
    }

    pub fn set_accepting(&self, accepting: bool) {
        self.0.accepting.store(accepting, Ordering::Relaxed);
    }

    pub fn accepting(&self) -> bool {
        self.0.accepting.load(Ordering::Relaxed)
    }

    /// Changes which input devices the remote user may use, and tells the live session.
    pub fn set_permissions(&self, permissions: Permissions) {
        self.0.permissions.send_replace(permissions);
    }

    pub fn permissions(&self) -> Permissions {
        *self.0.permissions.borrow()
    }

    pub fn set_trusted(&self, peer: EndpointId, trusted: bool) {
        let mut set = self.0.trusted.write().unwrap_or_else(PoisonError::into_inner);
        if trusted {
            set.insert(peer);
        } else {
            set.remove(&peer);
        }
    }

    /// The peer currently connected or waiting for an answer.
    pub fn current_peer(&self) -> Option<EndpointId> {
        lock(&self.0.slot).as_ref().map(|(peer, _)| *peer)
    }

    /// Ends the current session or pending request, if any.
    pub fn disconnect(&self) {
        if let Some((_, conn)) = &*lock(&self.0.slot) {
            conn.close(CLOSE_NORMAL.into(), b"The host ended the session.");
        }
    }
}

impl Shared {
    fn host_name(&self) -> String {
        self.host_name.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn is_trusted(&self, peer: &EndpointId) -> bool {
        self.trusted.read().unwrap_or_else(PoisonError::into_inner).contains(peer)
    }

    /// Claims the session slot for `conn`, or `None` if another peer holds it.
    fn reserve(&self, conn: &Connection) -> Option<SlotGuard<'_>> {
        let mut slot = lock(&self.slot);
        if slot.is_some() {
            return None;
        }
        *slot = Some((conn.remote_id(), conn.clone()));
        Some(SlotGuard(self))
    }
}

struct SlotGuard<'a>(&'a Shared);

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        *lock(&self.0.slot) = None;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
