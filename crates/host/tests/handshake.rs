//! Host ↔ client over two localhost endpoints: the approval flow, then video and input.

use std::time::Duration;

use connect_client::{ClientEvent, ClientHandle, EndReason, Stage};
use connect_host::{Decision, HostConfig, HostEvent, HostHandle, InputSink, VideoSource};
use connect_net::{self as net, iroh::Endpoint};
use connect_proto::{
    ALPN, DeviceClass, Permissions,
    control::{ClientMsg, HostMsg},
    input::{GamepadState, InputEvent, MouseButton},
    video::FrameAssembler,
};
use iroh::{
    SecretKey,
    endpoint::{Connection, RecvStream, SendStream},
};
use tokio::{sync::mpsc, time::timeout};

const WAIT: Duration = Duration::from_secs(20);

fn config() -> HostConfig {
    HostConfig {
        video: VideoSource::TestPattern { width: 320, height: 240, fps: 30 },
        ..HostConfig::new("Test host")
    }
}

async fn endpoint() -> Endpoint {
    net::bind_local_endpoint(SecretKey::generate()).await.unwrap()
}

async fn start_host(config: HostConfig) -> (Endpoint, HostHandle, mpsc::Receiver<HostEvent>) {
    let ep = endpoint().await;
    let (handle, events) = connect_host::start(ep.clone(), config);
    (ep, handle, events)
}

async fn start_client(host: &Endpoint) -> (Endpoint, ClientHandle, mpsc::Receiver<ClientEvent>) {
    let ep = endpoint().await;
    let (handle, events) = connect_client::connect(ep.clone(), net::local_addr(host), "Test client", || {});
    (ep, handle, events)
}

async fn next<T>(rx: &mut mpsc::Receiver<T>) -> T {
    timeout(WAIT, rx.recv()).await.expect("timed out waiting for an event").expect("channel closed")
}

async fn request(host: &mut mpsc::Receiver<HostEvent>) -> connect_host::ConnectionRequest {
    match next(host).await {
        HostEvent::Request(request) => request,
        other => panic!("expected a connection request, got {other:?}"),
    }
}

/// Skips progress events and returns how the attempt ended.
async fn ended(client: &mut mpsc::Receiver<ClientEvent>) -> EndReason {
    loop {
        if let ClientEvent::Ended(reason) = next(client).await {
            return reason;
        }
    }
}

async fn accepted(client: &mut mpsc::Receiver<ClientEvent>) -> (String, u32, u32) {
    loop {
        match next(client).await {
            ClientEvent::Accepted { host_name, width, height, .. } => return (host_name, width, height),
            ClientEvent::Ended(reason) => panic!("ended before being accepted: {reason}"),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn deny_rejects_with_reason() {
    let (host_ep, _host, mut host_events) = start_host(config()).await;
    let (client_ep, _client, mut client_events) = start_client(&host_ep).await;

    assert_eq!(next(&mut client_events).await, ClientEvent::Stage(Stage::Connecting));
    let req = request(&mut host_events).await;
    assert_eq!(req.peer, client_ep.id(), "the prompt shows the verified User-ID");
    assert_eq!(req.client_name, "Test client");
    assert!(req.is_pending());
    assert_eq!(next(&mut client_events).await, ClientEvent::Stage(Stage::WaitingForApproval));
    req.respond(Decision::Deny);

    assert_eq!(ended(&mut client_events).await, EndReason::Rejected("The host declined the connection.".into()));
}

#[tokio::test(flavor = "multi_thread")]
async fn unanswered_request_times_out() {
    let (host_ep, _host, mut host_events) =
        start_host(HostConfig { approval_timeout: Duration::from_millis(300), ..config() }).await;
    let (_client_ep, _client, mut client_events) = start_client(&host_ep).await;

    let req = request(&mut host_events).await;
    assert_eq!(ended(&mut client_events).await, EndReason::Rejected("The host didn't answer in time.".into()));
    assert!(!req.is_pending(), "the prompt can tell it expired");
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_streams_video() {
    let (host_ep, _host, mut host_events) = start_host(config()).await;
    let (_client_ep, client, mut client_events) = start_client(&host_ep).await;

    request(&mut host_events).await.respond(Decision::Allow(Permissions::ALL));
    assert_eq!(accepted(&mut client_events).await, ("Test host".into(), 320, 240));
    assert!(matches!(next(&mut host_events).await, HostEvent::SessionStarted { .. }));

    let mut frames = 0;
    timeout(WAIT, async {
        while frames < 10 {
            if let Some(frame) = client.take_frame() {
                assert_eq!((frame.width, frame.height), (320, 240));
                assert_eq!(frame.rgba.len(), 320 * 240 * 4);
                frames += 1;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("video frames arrive");

    client.disconnect();
    assert_eq!(ended(&mut client_events).await, EndReason::Disconnected);
    match next(&mut host_events).await {
        HostEvent::SessionEnded { reason, .. } => assert_eq!(reason, "Test client disconnected."),
        other => panic!("expected the session to end, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_peer_needs_no_prompt() {
    let (host_ep, host, mut host_events) = start_host(config()).await;
    let client_ep = endpoint().await;
    host.set_trusted(client_ep.id(), true);
    let (_client, mut client_events) =
        connect_client::connect(client_ep.clone(), net::local_addr(&host_ep), "Trusted", || {});

    accepted(&mut client_events).await;
    assert!(matches!(next(&mut host_events).await, HostEvent::SessionStarted { .. }));
}

#[tokio::test(flavor = "multi_thread")]
async fn second_client_is_told_host_is_busy() {
    let (host_ep, host, mut host_events) = start_host(config()).await;
    let (first_ep, _first, mut first_events) = start_client(&host_ep).await;
    request(&mut host_events).await.respond(Decision::Allow(Permissions::ALL));
    accepted(&mut first_events).await;
    assert_eq!(host.current_peer(), Some(first_ep.id()));

    let (_second_ep, _second, mut second_events) = start_client(&host_ep).await;
    assert_eq!(ended(&mut second_events).await, EndReason::Rejected("The host is in another session right now.".into()));

    host.disconnect();
    assert_eq!(ended(&mut first_events).await, EndReason::HostEnded("The host ended the session.".into()));
}

#[tokio::test(flavor = "multi_thread")]
async fn not_accepting_turns_everyone_away() {
    let (host_ep, host, _host_events) = start_host(config()).await;
    host.set_accepting(false);
    let (_client_ep, _client, mut client_events) = start_client(&host_ep).await;
    assert_eq!(
        ended(&mut client_events).await,
        EndReason::Rejected("The host isn't accepting connections right now.".into())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn chosen_permissions_reach_the_client() {
    let (host_ep, host, mut host_events) = start_host(config()).await;
    let (_client_ep, _client, mut client_events) = start_client(&host_ep).await;
    let mouse_only = Permissions::VIEW_ONLY.with(DeviceClass::Mouse, true);
    request(&mut host_events).await.respond(Decision::Allow(mouse_only));

    let granted = loop {
        match next(&mut client_events).await {
            ClientEvent::Accepted { permissions, .. } => break permissions,
            ClientEvent::Ended(reason) => panic!("ended before being accepted: {reason}"),
            _ => {}
        }
    };
    assert_eq!(granted, mouse_only);
    match next(&mut host_events).await {
        HostEvent::SessionStarted { permissions, .. } => assert_eq!(permissions, mouse_only),
        other => panic!("expected the session to start, got {other:?}"),
    }
    assert_eq!(host.permissions(), mouse_only);

    host.set_permissions(Permissions::ALL);
    loop {
        match next(&mut client_events).await {
            ClientEvent::PermissionsChanged(p) => break assert_eq!(p, Permissions::ALL),
            ClientEvent::Ended(reason) => panic!("ended early: {reason}"),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn input_obeys_permissions_and_is_released() {
    let (sink, mut injected) = mpsc::unbounded_channel();
    let (host_ep, host, mut host_events) =
        start_host(HostConfig { input: InputSink::Channel(sink), ..config() }).await;
    let (_client_ep, client, mut client_events) = start_client(&host_ep).await;
    let no_gamepad = Permissions::ALL.with(DeviceClass::Gamepad, false);
    request(&mut host_events).await.respond(Decision::Allow(no_gamepad));
    accepted(&mut client_events).await;

    let w_down = InputEvent::Key { scancode: 0x11, down: true };
    let click = InputEvent::MouseButton { button: MouseButton::Left, down: true };
    let pad = GamepadState { buttons: GamepadState::A, ..GamepadState::NEUTRAL };
    client.send_input(w_down);
    client.send_input(InputEvent::Gamepad { pad: 0, state: pad }); // gamepad not allowed
    client.send_input(InputEvent::Key { scancode: 0, down: true }); // malformed
    client.send_input(click);
    let mut injected_next = async || timeout(WAIT, injected.recv()).await.expect("input arrives").unwrap();
    assert_eq!(injected_next().await, w_down);
    assert_eq!(injected_next().await, click);

    // Turning the keyboard off lets go of W straight away, and stops further keys.
    host.set_permissions(no_gamepad.with(DeviceClass::Keyboard, false));
    assert_eq!(injected_next().await, InputEvent::Key { scancode: 0x11, down: false });
    client.send_input(InputEvent::Key { scancode: 0x1E, down: true });

    // Ending the session lets go of the mouse button.
    client.disconnect();
    assert_eq!(injected_next().await, InputEvent::MouseButton { button: MouseButton::Left, down: false });
    let extra = timeout(Duration::from_millis(300), injected.recv()).await;
    assert!(!matches!(extra, Ok(Some(_))), "unexpected input: {extra:?}");
}

#[cfg(not(windows))]
#[tokio::test(flavor = "multi_thread")]
async fn only_windows_can_host() {
    let (host_ep, _host, _host_events) = start_host(HostConfig::new("Linux box")).await;
    let (_client_ep, _client, mut client_events) = start_client(&host_ep).await;
    match ended(&mut client_events).await {
        EndReason::Rejected(reason) => assert!(reason.contains("hosting needs Windows"), "{reason}"),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn client_giving_up_withdraws_the_prompt() {
    let (host_ep, _host, mut host_events) = start_host(config()).await;
    let (_client_ep, client, mut client_events) = start_client(&host_ep).await;
    let req = request(&mut host_events).await;
    client.disconnect();
    assert_eq!(ended(&mut client_events).await, EndReason::Disconnected);
    timeout(WAIT, async {
        while req.is_pending() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the request stops being pending");
}

/// Talks the protocol directly, for cases the client library never produces.
struct Raw {
    _ep: Endpoint, // the connection dies with its endpoint
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
}

async fn raw_hello(host_ep: &Endpoint, version: u32) -> Raw {
    let ep = endpoint().await;
    let conn = timeout(WAIT, ep.connect(net::local_addr(host_ep), ALPN)).await.unwrap().unwrap();
    let (mut send, recv) = conn.open_bi().await.unwrap();
    net::write_msg(&mut send, &ClientMsg::Hello { version, client_name: "Raw".into() }).await.unwrap();
    Raw { _ep: ep, conn, send, recv }
}

async fn read_reply(recv: &mut RecvStream) -> Option<HostMsg> {
    timeout(WAIT, net::read_msg::<HostMsg>(recv)).await.expect("timed out waiting for the host").unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn version_mismatch_is_rejected() {
    let (host_ep, _host, _host_events) = start_host(config()).await;
    let mut raw = raw_hello(&host_ep, 999).await;
    match read_reply(&mut raw.recv).await {
        Some(HostMsg::Rejected { reason }) => assert!(reason.contains("different version"), "{reason}"),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn keyframe_request_is_honoured() {
    let (host_ep, _host, mut host_events) = start_host(config()).await;
    let Raw { _ep, conn, mut send, mut recv } = raw_hello(&host_ep, connect_proto::PROTOCOL_VERSION).await;
    request(&mut host_events).await.respond(Decision::Allow(Permissions::ALL));
    assert!(matches!(read_reply(&mut recv).await, Some(HostMsg::Welcome { .. })));

    let mut asm = FrameAssembler::new();
    let mut next_frame = async || loop {
        let datagram = timeout(WAIT, conn.read_datagram()).await.unwrap().unwrap();
        if let Some(frame) = asm.push(datagram).unwrap() {
            return frame;
        }
    };
    let first = next_frame().await;
    assert!(first.meta.keyframe, "the stream starts with a keyframe");
    for _ in 0..3 {
        assert!(!next_frame().await.meta.keyframe, "then delta frames");
    }

    net::write_msg(&mut send, &ClientMsg::RequestKeyframe).await.unwrap();
    let got_keyframe = timeout(WAIT, async {
        loop {
            if next_frame().await.meta.keyframe {
                break;
            }
        }
    })
    .await;
    assert!(got_keyframe.is_ok(), "a keyframe follows the request");
}
