//! The capture → H.264 encode thread behind a session.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use connect_proto::{
    now_micros,
    video::{EncodedFrame, FrameMeta},
};
use openh264::{
    OpenH264API,
    encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode, UsageType},
    formats::{BgraSliceU8, YUVBuffer, YUVSource},
};
use tokio::sync::mpsc;

use crate::capture::{Frame, VideoSource};

/// Above any display's refresh rate: capture only yields a frame when the screen changes, so the
/// stream runs at whatever rate the host's monitor and content do (60, 120, 144, 240 Hz…).
const MAX_FPS: f32 = 240.0;
/// A keyframe at least this often, in case a keyframe request is lost along with the video.
const INTRA_PERIOD_FRAMES: u32 = 600;
/// How long one capture wait may block, so a stop request is noticed promptly.
const POLL: Duration = Duration::from_millis(100);

pub(crate) enum VideoEvent {
    /// The encoded size; always the first event, and sent again whenever the screen resizes.
    Format { width: u32, height: u32 },
    Frame(EncodedFrame),
}

/// Runs capture and encode on a dedicated thread, which stops when this is dropped.
pub(crate) struct VideoStream {
    events: mpsc::Receiver<Result<VideoEvent>>,
    keyframe: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl VideoStream {
    pub fn start(source: VideoSource, bitrate_bps: u32) -> Result<Self> {
        // Small, so a slow network pushes back on the encoder instead of queueing stale frames.
        let (tx, events) = mpsc::channel(2);
        let keyframe = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (kf, st) = (keyframe.clone(), stop.clone());
        std::thread::Builder::new()
            .name("connect-video".into())
            .spawn(move || {
                if let Err(e) = run(&source, bitrate_bps, &tx, &kf, &st) {
                    let _ = tx.blocking_send(Err(e));
                }
            })
            .context("starting video thread")?;
        Ok(Self { events, keyframe, stop })
    }

    /// The next event; `None` once the thread has stopped.
    pub async fn next(&mut self) -> Option<Result<VideoEvent>> {
        self.events.recv().await
    }

    /// Makes the next encoded frame a keyframe, even if the screen hasn't changed.
    pub fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn encoder_config(bitrate_bps: u32) -> EncoderConfig {
    EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(bitrate_bps))
        .max_frame_rate(FrameRate::from_hz(MAX_FPS))
        // Rate control can only hold the bitrate if it may drop a frame when over budget.
        .skip_frames(true)
        // Not supported for screen content; openh264 warns and turns them off otherwise.
        .adaptive_quantization(false)
        .background_detection(false)
        .intra_frame_period(IntraFramePeriod::from_num_frames(INTRA_PERIOD_FRAMES))
}

fn run(
    source: &VideoSource,
    bitrate_bps: u32,
    tx: &mpsc::Sender<Result<VideoEvent>>,
    keyframe: &AtomicBool,
    stop: &AtomicBool,
) -> Result<()> {
    let mut capturer = source.open()?;
    let mut encoder =
        Encoder::with_api_config(OpenH264API::from_source(), encoder_config(bitrate_bps)).context("starting H.264 encoder")?;
    // The last captured image, kept so a keyframe can be sent while the screen is static.
    let mut yuv: Option<YUVBuffer> = None;
    let mut packed = Vec::new();
    let mut bitstream = Vec::new();
    let mut seq = 0u32;

    while !stop.load(Ordering::Relaxed) {
        let frame = capturer.next_frame(POLL)?;
        let capture_time_us = now_micros();
        match frame {
            Some(frame) => {
                // 4:2:0 chroma needs even dimensions; drop an odd last row/column.
                let (w, h) = (frame.width & !1, frame.height & !1);
                ensure!(w > 0 && h > 0, "captured an empty image");
                if yuv.as_ref().map(|b| b.dimensions()) != Some((w as usize, h as usize)) {
                    yuv = Some(YUVBuffer::new(w as usize, h as usize));
                    if tx.blocking_send(Ok(VideoEvent::Format { width: w, height: h })).is_err() {
                        return Ok(());
                    }
                }
                let buf = yuv.as_mut().expect("set above");
                buf.read_bgra8(BgraSliceU8::new(packed_bgra(&frame, w, h, &mut packed), (w as usize, h as usize)));
            }
            None if yuv.is_some() && keyframe.load(Ordering::Relaxed) => {}
            None => continue,
        }
        let Some(image) = &yuv else { continue };

        if keyframe.swap(false, Ordering::Relaxed) {
            encoder.force_intra_frame();
        }
        let started = Instant::now();
        let encoded = encoder.encode(image).context("encoding frame")?;
        let frame_type = encoded.frame_type();
        bitstream.clear();
        encoded.write_vec(&mut bitstream);
        if bitstream.is_empty() || frame_type == FrameType::Skip {
            continue;
        }

        let meta = FrameMeta {
            seq,
            // Only IDR frames carry SPS/PPS and reset references, so only they let a decoder (re)start.
            keyframe: frame_type == FrameType::IDR,
            capture_time_us,
            encode_time_us: started.elapsed().as_micros() as u32,
        };
        seq = seq.wrapping_add(1);
        let frame = EncodedFrame { meta, data: Bytes::copy_from_slice(&bitstream) };
        if tx.blocking_send(Ok(VideoEvent::Frame(frame))).is_err() {
            return Ok(());
        }
    }
    Ok(())
}

/// `frame`'s pixels as a tightly packed `w`×`h` BGRA buffer, copying only if rows are padded
/// or cropped.
fn packed_bgra<'a>(frame: &'a Frame<'_>, w: u32, h: u32, scratch: &'a mut Vec<u8>) -> &'a [u8] {
    let row = w as usize * 4;
    if frame.stride == row {
        return &frame.bgra[..row * h as usize];
    }
    scratch.clear();
    for line in frame.bgra.chunks(frame.stride).take(h as usize) {
        scratch.extend_from_slice(&line[..row]);
    }
    scratch
}
