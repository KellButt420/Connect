//! One incoming connection: handshake, approval, then streaming until either side hangs up.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use connect_net as net;
use connect_proto::{
    CLOSE_NORMAL, CLOSE_PROTOCOL, CLOSE_REJECTED, PROTOCOL_VERSION, Permissions,
    control::{ClientMsg, HostMsg},
    video::{self, EncodedFrame},
};
use iroh::endpoint::{Connection, ConnectionError, Incoming, RecvStream, SendDatagramError, SendStream};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::timeout,
};
use tracing::{debug, info, warn};

use crate::{
    ConnectionRequest, Decision, HostEvent, Shared, VideoSource,
    encode::{VideoEvent, VideoStream},
};

/// How long a new connection may take to say hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for a turned-away client to read the reason and hang up itself.
const REJECT_LINGER: Duration = Duration::from_secs(3);

const DECLINED: &str = "The host declined the connection.";
const NO_ANSWER: &str = "The host didn't answer in time.";
const BUSY: &str = "The host is in another session right now.";
const NOT_ACCEPTING: &str = "The host isn't accepting connections right now.";
const NO_CAPTURE: &str = "The host couldn't capture its screen.";
const CANNOT_HOST: &str = "That PC can't be shared: hosting needs Windows. It can still connect to others.";

pub(crate) async fn run(incoming: Incoming, shared: Arc<Shared>, events: mpsc::Sender<HostEvent>) {
    let conn = match incoming.accept() {
        Ok(accepting) => match accepting.await {
            Ok(conn) => conn,
            Err(e) => return debug!("incoming handshake failed: {e:#}"),
        },
        Err(e) => return debug!("incoming connection failed: {e:#}"),
    };
    if let Err(e) = serve(&conn, &shared, &events).await {
        warn!(peer = %conn.remote_id().fmt_short(), "connection failed: {e:#}");
        conn.close(CLOSE_PROTOCOL.into(), b"protocol error");
    }
}

async fn serve(conn: &Connection, shared: &Shared, events: &mpsc::Sender<HostEvent>) -> Result<()> {
    let peer = conn.remote_id();
    let (mut send, mut recv) = timeout(HELLO_TIMEOUT, conn.accept_bi()).await.context("no hello")??;
    let hello = timeout(HELLO_TIMEOUT, net::read_msg(&mut recv)).await.context("no hello")??;
    let Some(ClientMsg::Hello { version, client_name }) = hello else { bail!("expected hello") };
    let client_name = clean_name(&client_name);
    info!(peer = %peer.fmt_short(), %client_name, "connection request");

    if version != PROTOCOL_VERSION {
        let reason = format!(
            "The host runs a different version of Connect (protocol {PROTOCOL_VERSION}, yours is {version}). \
             Update both PCs to the same version."
        );
        return reject(conn, &mut send, &reason).await;
    }
    if shared.video == VideoSource::Screen && !VideoSource::SCREEN_SUPPORTED {
        return reject(conn, &mut send, CANNOT_HOST).await;
    }
    if !shared.accepting.load(std::sync::atomic::Ordering::Relaxed) {
        return reject(conn, &mut send, NOT_ACCEPTING).await;
    }
    let Some(_slot) = shared.reserve(conn) else {
        return reject(conn, &mut send, BUSY).await;
    };

    if !shared.is_trusted(&peer) {
        match ask(conn, shared, events, &client_name).await {
            Answer::Decided(Decision::Allow) => {}
            Answer::Decided(Decision::Deny) => return reject(conn, &mut send, DECLINED).await,
            Answer::TimedOut => return reject(conn, &mut send, NO_ANSWER).await,
            Answer::Withdrawn => return Ok(()),
        }
    }

    let mut video = VideoStream::start(shared.video.clone())?;
    let (width, height) = match video.next().await {
        Some(Ok(VideoEvent::Format { width, height })) => (width, height),
        Some(Err(e)) => {
            warn!("screen capture failed to start: {e:#}");
            return reject(conn, &mut send, NO_CAPTURE).await;
        }
        _ => return reject(conn, &mut send, NO_CAPTURE).await,
    };

    let permissions = shared.permissions.subscribe();
    let welcome =
        HostMsg::Welcome { host_name: shared.host_name(), width, height, permissions: *permissions.borrow() };
    net::write_msg(&mut send, &welcome).await?;
    info!(peer = %peer.fmt_short(), %client_name, width, height, "session started");
    let _ = events.send(HostEvent::SessionStarted { peer, client_name: client_name.clone() }).await;

    let result = stream(conn, send, recv, video, permissions).await;
    let reason = end_reason(conn, &client_name, &result);
    info!(peer = %peer.fmt_short(), "session ended: {reason}");
    if conn.close_reason().is_none() {
        let code = if result.is_ok() { CLOSE_NORMAL } else { CLOSE_PROTOCOL };
        conn.close(code.into(), b"session ended");
    }
    let _ = events.send(HostEvent::SessionEnded { peer, reason }).await;
    Ok(())
}

enum Answer {
    Decided(Decision),
    TimedOut,
    /// The requester hung up (or the host user cancelled) before an answer.
    Withdrawn,
}

async fn ask(conn: &Connection, shared: &Shared, events: &mpsc::Sender<HostEvent>, client_name: &str) -> Answer {
    let (respond, answer) = oneshot::channel();
    let request = ConnectionRequest { peer: conn.remote_id(), client_name: client_name.to_owned(), respond };
    if events.send(HostEvent::Request(request)).await.is_err() {
        return Answer::Decided(Decision::Deny); // nobody is listening to ask
    }
    tokio::select! {
        answer = timeout(shared.approval_timeout, answer) => match answer {
            Ok(Ok(decision)) => Answer::Decided(decision),
            Ok(Err(_)) => Answer::Decided(Decision::Deny), // request dropped unanswered
            Err(_) => Answer::TimedOut,
        },
        _ = conn.closed() => Answer::Withdrawn,
    }
}

/// Tells the client why, then closes. Closing straight away could discard the reason in flight.
async fn reject(conn: &Connection, send: &mut SendStream, reason: &str) -> Result<()> {
    info!(peer = %conn.remote_id().fmt_short(), "turning away: {reason}");
    net::write_msg(send, &HostMsg::Rejected { reason: reason.to_owned() }).await?;
    send.finish()?;
    let _ = timeout(REJECT_LINGER, conn.closed()).await;
    conn.close(CLOSE_REJECTED.into(), reason.as_bytes());
    Ok(())
}

async fn stream(
    conn: &Connection,
    mut send: SendStream,
    recv: RecvStream,
    mut video: VideoStream,
    mut permissions: watch::Receiver<Permissions>,
) -> Result<()> {
    // Reading isn't cancel-safe (a half-read message would be lost), so it gets its own task.
    let (msg_tx, mut msgs) = mpsc::channel(16);
    tokio::spawn(read_loop(recv, msg_tx));

    loop {
        tokio::select! {
            msg = msgs.recv() => match msg {
                Some(Ok(ClientMsg::Ping { client_time_us })) => {
                    let pong = HostMsg::Pong { client_time_us, host_time_us: connect_proto::now_micros() };
                    net::write_msg(&mut send, &pong).await?;
                }
                Some(Ok(ClientMsg::RequestKeyframe)) => {
                    debug!("client asked for a keyframe");
                    video.request_keyframe();
                }
                Some(Ok(ClientMsg::Hello { .. })) => bail!("unexpected second hello"),
                Some(Err(e)) => return Err(e),
                None => return Ok(()), // the client finished its control stream: a clean hang-up
            },
            Ok(()) = permissions.changed() => {
                let p = *permissions.borrow_and_update();
                net::write_msg(&mut send, &HostMsg::PermissionsChanged(p)).await?;
            }
            event = video.next() => match event {
                Some(Ok(VideoEvent::Frame(frame))) => send_frame(conn, &frame)?,
                Some(Ok(VideoEvent::Format { width, height })) => {
                    net::write_msg(&mut send, &HostMsg::VideoFormat { width, height }).await?;
                }
                Some(Err(e)) => return Err(e.context("screen capture failed")),
                None => bail!("video thread stopped"),
            },
            _ = conn.closed() => return Ok(()),
        }
    }
}

async fn read_loop(mut recv: RecvStream, tx: mpsc::Sender<Result<ClientMsg>>) {
    loop {
        match net::read_msg::<ClientMsg>(&mut recv).await {
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

fn send_frame(conn: &Connection, frame: &EncodedFrame) -> Result<()> {
    let max = conn.max_datagram_size().context("the client doesn't accept video datagrams")?;
    let chunks = video::packetize(frame.meta, &frame.data, max)?;
    if frame.meta.keyframe {
        debug!(seq = frame.meta.seq, bytes = frame.data.len(), datagrams = chunks.len(), max, "sending keyframe");
    }
    for chunk in chunks {
        match conn.send_datagram(chunk) {
            Ok(()) => {}
            // The path MTU shrank mid-frame; the client will notice the gap and ask for a keyframe.
            Err(SendDatagramError::TooLarge) => {
                debug!(max, "datagram too large for the path; frame dropped");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn end_reason(conn: &Connection, client_name: &str, result: &Result<()>) -> String {
    match conn.close_reason() {
        Some(ConnectionError::LocallyClosed) => "You ended the session.".into(),
        Some(ConnectionError::ApplicationClosed(_)) => format!("{client_name} disconnected."),
        Some(ConnectionError::TimedOut | ConnectionError::Reset) => format!("Lost the connection to {client_name}."),
        Some(other) => format!("Connection error: {other}"),
        None => match result {
            Ok(()) => format!("{client_name} disconnected."),
            Err(e) => format!("Session error: {e:#}"),
        },
    }
}

/// A peer-supplied name made safe to show: no control characters, bounded length.
fn clean_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(48).collect();
    let name = name.trim();
    if name.is_empty() { "Unknown device".into() } else { name.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::clean_name;

    #[test]
    fn names_are_sanitised() {
        assert_eq!(clean_name("  Gaming PC \n"), "Gaming PC");
        assert_eq!(clean_name("\u{7}\u{1b}[2J"), "[2J");
        assert_eq!(clean_name(""), "Unknown device");
        assert_eq!(clean_name(&"x".repeat(100)).len(), 48);
    }
}
