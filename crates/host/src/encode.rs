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
    Format {
        width: u32,
        height: u32,
    },
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
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(4)) as u16;
    EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(bitrate_bps))
        .max_frame_rate(FrameRate::from_hz(MAX_FPS))
        // A single slice forces one worker even with automatic threading. Bound CPU use for games.
        .max_slice_len(16 * 1024)
        .num_threads(threads)
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
    let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), encoder_config(bitrate_bps))
        .context("starting H.264 encoder")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use openh264::decoder::Decoder;

    fn moving_image(width: usize, height: usize, frame: usize) -> YUVBuffer {
        let mut pixels = vec![0; width * height * 4];
        for (i, pixel) in pixels.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i % width + frame * 7, i / width + frame * 3);
            pixel.copy_from_slice(&[(x ^ y) as u8, (y * 3) as u8, (x * 5) as u8, 255]);
        }
        YUVBuffer::from_bgra8_source(BgraSliceU8::new(&pixels, (width, height)))
    }

    fn video_slices(data: &[u8]) -> usize {
        openh264::nal_units(data)
            .filter(|nal| {
                let header = if nal.starts_with(&[0, 0, 0, 1]) { 4 } else { 3 };
                matches!(nal[header] & 0x1f, 1 | 5)
            })
            .count()
    }

    #[test]
    fn parallel_slices_decode_and_recover_after_resize() {
        // Two workers explicitly, so this also exercises the slice setup on single-core CI.
        let config = encoder_config(20_000_000).num_threads(2);
        let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), config).unwrap();
        let mut decoder = Decoder::new().unwrap();
        for (width, height) in [(640, 480), (640, 480), (800, 600)] {
            let image = moving_image(width, height, 0);
            let encoded = encoder.encode(&image).unwrap();
            let data = encoded.to_vec();
            assert!(video_slices(&data) >= 2, "a single slice serializes the encoder");
            let decoded = decoder.decode(&data).unwrap().expect("frame decodes without buffering");
            assert_eq!(decoded.dimensions(), (width, height));
        }
        encoder.force_intra_frame();
        let data = encoder.encode(&moving_image(800, 600, 1)).unwrap().to_vec();
        let mut fresh_decoder = Decoder::new().unwrap();
        assert_eq!(fresh_decoder.decode(&data).unwrap().unwrap().dimensions(), (800, 600));
    }

    #[test]
    fn tiny_frames_still_encode() {
        let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), encoder_config(20_000_000)).unwrap();
        let data = encoder.encode(&moving_image(16, 16, 0)).unwrap().to_vec();
        let mut decoder = Decoder::new().unwrap();
        assert_eq!(decoder.decode(&data).unwrap().unwrap().dimensions(), (16, 16));
    }

    /// Synthetic encode-only comparison; run explicitly without other tests competing for CPU.
    #[test]
    #[ignore]
    fn compare_encoder_frame_times() {
        let images: Vec<_> = (0..60).map(|i| moving_image(1920, 1080, i)).collect();
        let baseline = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(20_000_000))
            .max_frame_rate(FrameRate::from_hz(MAX_FPS))
            .skip_frames(true)
            .adaptive_quantization(false)
            .background_detection(false)
            .intra_frame_period(IntraFramePeriod::from_num_frames(INTRA_PERIOD_FRAMES));
        for (name, config) in [("original", baseline), ("parallel", encoder_config(20_000_000))] {
            // Process the same frames in both cases; exclude rate-control skips from this comparison.
            let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), config.skip_frames(false)).unwrap();
            let mut timings = Vec::new();
            let mut bytes = 0;
            let mut skipped = 0;
            for image in &images {
                let started = Instant::now();
                let encoded = encoder.encode(image).unwrap();
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                if encoded.frame_type() == FrameType::Skip {
                    skipped += 1;
                } else {
                    bytes += encoded.to_vec().len();
                    timings.push(elapsed);
                }
            }
            timings.sort_by(f64::total_cmp);
            let median = timings[timings.len() / 2];
            let p95 = timings[(timings.len() - 1) * 95 / 100];
            println!("{name}: median={median:.2}ms p95={p95:.2}ms skipped={skipped} bytes={bytes}");
        }
    }
}
