//! Datagrams → whole frames (async task) → decoded RGBA (decoder thread).

use std::{
    sync::{
        Arc,
        atomic::Ordering,
        mpsc::{self as std_mpsc, TrySendError},
    },
    time::Instant,
};

use anyhow::{Context, Result};
use connect_proto::{
    now_micros,
    video::{EncodedFrame, FrameAssembler},
};
use iroh::endpoint::Connection;
use openh264::{decoder::Decoder, formats::YUVSource};
use tokio::sync::mpsc::UnboundedSender;
use tracing::debug;

use crate::{Shared, VideoFrame, lock};

/// Complete frames waiting for the decoder. Beyond this the decoder has fallen behind, and
/// it's better to skip to the next keyframe than to show ever-older video.
const DECODE_QUEUE: usize = 4;

pub(crate) fn start_decoder(
    shared: Arc<Shared>,
    keyframes: UnboundedSender<()>,
) -> Result<std_mpsc::SyncSender<EncodedFrame>> {
    let (tx, rx) = std_mpsc::sync_channel(DECODE_QUEUE);
    let mut decoder = Decoder::new().context("starting H.264 decoder")?;
    std::thread::Builder::new()
        .name("connect-decode".into())
        .spawn(move || {
            while let Ok(frame) = rx.recv() {
                decode(&mut decoder, &frame, &shared, &keyframes);
            }
        })
        .context("starting decoder thread")?;
    Ok(tx)
}

fn decode(decoder: &mut Decoder, frame: &EncodedFrame, shared: &Shared, keyframes: &UnboundedSender<()>) {
    let started = Instant::now();
    let yuv = match decoder.decode(&frame.data) {
        Ok(Some(yuv)) => yuv,
        Ok(None) => return,
        Err(e) => {
            debug!(seq = frame.meta.seq, "decode failed: {e}");
            let _ = keyframes.send(());
            return;
        }
    };
    let (w, h) = yuv.dimensions();
    let mut rgba = vec![0; w * h * 4];
    yuv.write_rgba8(&mut rgba);

    let c = &shared.counters;
    c.decode_us.store(started.elapsed().as_micros() as u64, Ordering::Relaxed);
    c.encode_us.store(frame.meta.encode_time_us as u64, Ordering::Relaxed);
    let offset = c.clock_offset_us.load(Ordering::Relaxed);
    if offset != i64::MIN {
        let latency = now_micros() as i64 - (frame.meta.capture_time_us as i64 + offset);
        c.latency_us.store(latency, Ordering::Relaxed);
    }
    if c.frames_decoded.fetch_add(1, Ordering::Relaxed) == 0 {
        debug!(seq = frame.meta.seq, bytes = frame.data.len(), "first frame decoded");
    }

    *lock(&shared.frame) = Some(VideoFrame { width: w as u32, height: h as u32, rgba, meta: frame.meta });
    (shared.wake)();
}

/// Reassembles datagrams into frames for the decoder, asking for a keyframe after any loss.
pub(crate) async fn receive(
    conn: Connection,
    frames: std_mpsc::SyncSender<EncodedFrame>,
    keyframes: UnboundedSender<()>,
    shared: Arc<Shared>,
) {
    let mut assembler = FrameAssembler::new();
    let mut damage_reported = 0;
    while let Ok(datagram) = conn.read_datagram().await {
        shared.counters.bytes.fetch_add(datagram.len() as u64, Ordering::Relaxed);
        match datagram.first().copied() {
            Some(connect_proto::audio::KIND_AUDIO) => {
                if shared.audio_allowed.load(Ordering::Acquire) {
                    let playback = lock(&shared.playback).clone();
                    if let Some(playback) = playback
                        && let Err(error) = playback.push_datagram(&datagram)
                    {
                        shared.audio_allowed.store(false, Ordering::Release);
                        *lock(&shared.audio_failure) = Some(format!("Desktop audio playback stopped: {error:#}"));
                    }
                }
                continue;
            }
            Some(connect_proto::cursor::KIND_CURSOR) => {
                if let Ok(position) = connect_proto::CursorPosition::decode(&datagram) {
                    let mut cursor = lock(&shared.cursor);
                    if position.is_newer_than(cursor.0.seq) || cursor.2.elapsed() > std::time::Duration::from_secs(1) {
                        let moved =
                            (position.x, position.y, position.visible) != (cursor.0.x, cursor.0.y, cursor.0.visible);
                        cursor.0 = position;
                        cursor.2 = Instant::now();
                        drop(cursor);
                        if moved {
                            (shared.wake)();
                        }
                    }
                }
                continue;
            }
            _ => {}
        }
        match assembler.push(datagram) {
            Ok(Some(frame)) => match frames.try_send(frame) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    // The dropped frame is a reference for what follows: wait for a keyframe.
                    assembler.request_keyframe();
                    let _ = keyframes.send(());
                }
                Err(TrySendError::Disconnected(_)) => return,
            },
            Ok(None) => {}
            Err(e) => debug!("bad video datagram: {e:#}"),
        }

        let stats = assembler.stats;
        shared.counters.frames_lost.store(stats.frames_lost, Ordering::Relaxed);
        // Only ask once something was actually lost, not while waiting for the very first keyframe.
        let damage = stats.frames_lost + stats.frames_skipped;
        if assembler.needs_keyframe() && damage > damage_reported {
            damage_reported = damage;
            let _ = keyframes.send(());
        }
    }
}
