//! Captured BGRA → NV12 on the graphics card, with Direct3D 11's video processor: every
//! Windows GPU driver has one. The result stays in a texture on the capture device, for a
//! hardware encoder on the same device to read; nothing crosses to system memory.
//!
//! Colours match the processor path in `yuv`: BT.601, limited range, which the viewer expects.
//!
//! Each conversion's run time on the graphics card is measured with timestamp queries, read back
//! a few frames later without waiting, for the performance trace.

use std::mem::ManuallyDrop;

use anyhow::{Context, Result, ensure};
use windows::{
    Win32::Graphics::{
        Direct3D11::{
            D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ,
            D3D11_MAPPED_SUBRESOURCE, D3D11_QUERY, D3D11_QUERY_DATA_TIMESTAMP_DISJOINT, D3D11_QUERY_DESC,
            D3D11_QUERY_TIMESTAMP, D3D11_QUERY_TIMESTAMP_DISJOINT, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
            D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            D3D11_VIDEO_PROCESSOR_COLOR_SPACE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
            D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT,
            D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
            D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
            D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Device, ID3D11DeviceContext, ID3D11Query, ID3D11Texture2D,
            ID3D11VideoContext, ID3D11VideoContext1, ID3D11VideoDevice, ID3D11VideoProcessor,
            ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
        },
        Dxgi::Common::{
            DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
            DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
        },
    },
    core::Interface,
};

use crate::{
    trace::TimerVerdict,
    yuv::{Layout, Picture},
};

pub(crate) struct Nv12Converter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    nv12: ID3D11Texture2D,
    output: ID3D11VideoProcessorOutputView,
    /// The view of the capture texture last converted, reused while capture keeps the texture.
    input: Option<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    /// For reading the picture back, made when first needed.
    staging: Option<ID3D11Texture2D>,
    /// `None` where the device can't time its work.
    timer: Option<GpuTimer>,
    width: u32,
    height: u32,
}

impl Nv12Converter {
    /// A converter from `width`×`height` BGRA captures on `device`.
    pub fn new(device: &ID3D11Device, width: u32, height: u32) -> Result<Self> {
        ensure!(
            width > 0 && height > 0 && width.is_multiple_of(2) && height.is_multiple_of(2),
            "NV12 needs even dimensions"
        );
        unsafe {
            let video: ID3D11VideoDevice = device.cast().context("the Direct3D device has no video support")?;
            let context = device.GetImmediateContext()?;
            let video_context: ID3D11VideoContext = context.cast().context("no video context")?;
            let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
            let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: rate,
                InputWidth: width,
                InputHeight: height,
                OutputFrameRate: rate,
                OutputWidth: width,
                OutputHeight: height,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let enumerator = video.CreateVideoProcessorEnumerator(&content).context("no video processor")?;
            let input_support = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM)?;
            ensure!(
                input_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 != 0,
                "the video processor can't read BGRA"
            );
            let output_support = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12)?;
            ensure!(
                output_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 != 0,
                "the video processor can't write NV12"
            );
            let processor = video.CreateVideoProcessor(&enumerator, 0).context("creating the video processor")?;

            // Exactly the conversion asked for: no driver "enhancements", whole frames.
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video_context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            if let Ok(context1) = video_context.cast::<ID3D11VideoContext1>() {
                context1.VideoProcessorSetStreamColorSpace1(&processor, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                context1.VideoProcessorSetOutputColorSpace1(&processor, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601);
            } else {
                // Usage 0, RGB full range, BT.601 matrix, YCbCr nominal range 16–235 (bits 4–5 = 1).
                video_context.VideoProcessorSetStreamColorSpace(
                    &processor,
                    0,
                    &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 },
                );
                video_context.VideoProcessorSetOutputColorSpace(
                    &processor,
                    &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 1 << 4 },
                );
            }

            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut nv12 = None;
            device.CreateTexture2D(&desc, None, Some(&mut nv12)).context("creating the NV12 texture")?;
            let nv12 = nv12.context("no NV12 texture")?;
            let view = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut output = None;
            video
                .CreateVideoProcessorOutputView(&nv12, &enumerator, &view, Some(&mut output))
                .context("creating the NV12 output view")?;
            Ok(Self {
                device: device.clone(),
                context,
                video,
                video_context,
                enumerator,
                processor,
                nv12,
                output: output.context("no output view")?,
                input: None,
                staging: None,
                timer: GpuTimer::new(device),
                width,
                height,
            })
        }
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The converted picture, for the encoder.
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.nv12
    }

    /// Converts `bgra`, a capture at least this converter's size (an odd last row or column
    /// is left out), into [`Self::texture`]. With a `tag`, the conversion is timed on the
    /// graphics card while tracing is on; [`Self::poll_timings`] gives the result later.
    pub fn convert(&mut self, bgra: &ID3D11Texture2D, tag: Option<u64>) -> Result<()> {
        unsafe {
            if self.input.as_ref().is_none_or(|(texture, _)| texture != bgra) {
                let view = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
                    },
                };
                let mut input = None;
                self.video
                    .CreateVideoProcessorInputView(bgra, &self.enumerator, &view, Some(&mut input))
                    .context("creating the capture input view")?;
                self.input = Some((bgra.clone(), input.context("no input view")?));
            }
            let (_, input) = self.input.as_ref().expect("set above");
            // Only the even-sized part of an odd-sized capture.
            let rect = windows::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: self.width as i32,
                bottom: self.height as i32,
            };
            self.video_context.VideoProcessorSetStreamSourceRect(&self.processor, 0, true, Some(&rect));
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(input.clone())),
                ..Default::default()
            };
            let mut streams = [stream];
            let timing = match (&mut self.timer, tag) {
                (Some(timer), Some(tag)) if connect_perf::enabled() => timer.begin(&self.context).map(|at| (at, tag)),
                _ => None,
            };
            let result = self.video_context.VideoProcessorBlt(&self.processor, &self.output, 0, &streams);
            if let (Some(timer), Some((at, tag))) = (&mut self.timer, timing) {
                timer.end(&self.context, at, tag);
            }
            ManuallyDrop::drop(&mut streams[0].pInputSurface);
            result.context("converting the capture to NV12")
        }
    }

    /// Conversions the graphics card has finished timing since the last call, oldest first: their
    /// tags and run times in µs. Never waits for the card.
    pub fn poll_timings(&mut self, out: impl FnMut(u64, u32)) {
        if let Some(timer) = &mut self.timer
            && !timer.poll(&self.context, out)
        {
            tracing::info!("this driver's GPU timestamps don't cover the colour conversion; not timing it");
            self.timer = None;
        }
    }

    /// The converted picture, copied into memory.
    pub fn read_back(&mut self) -> Result<Picture> {
        unsafe {
            if self.staging.is_none() {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                self.nv12.GetDesc(&mut desc);
                desc.Usage = D3D11_USAGE_STAGING;
                desc.BindFlags = 0;
                desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                let mut staging = None;
                self.device.CreateTexture2D(&desc, None, Some(&mut staging))?;
                self.staging = Some(staging.context("no staging texture")?);
            }
            let staging = self.staging.as_ref().expect("set above");
            self.context.CopyResource(staging, &self.nv12);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let (w, h, pitch) = (self.width as usize, self.height as usize, mapped.RowPitch as usize);
            let mut picture = Picture::new(Layout::Nv12, w, h);
            // The chroma plane follows the luma plane's `h` rows, at the same pitch.
            let source = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * (h + h / 2));
            for (row, line) in picture.data_mut().chunks_exact_mut(w).enumerate() {
                line.copy_from_slice(&source[row * pitch..][..w]);
            }
            self.context.Unmap(staging, 0);
            Ok(picture)
        }
    }
}

/// Conversions timed at once. Results come back a frame or two later; one whose slot is still
/// waiting when its turn comes round again goes untimed rather than wait.
const TIMER_SLOTS: usize = 4;

/// Times work on the graphics card: a timestamp before and after, inside a disjoint query that
/// says whether the counter's frequency held steady in between.
struct GpuTimer {
    slots: Vec<TimerSlot>,
    check: crate::trace::TimerCheck,
    /// The slot the next timing uses; the oldest in flight.
    next: usize,
}

struct TimerSlot {
    disjoint: ID3D11Query,
    start: ID3D11Query,
    end: ID3D11Query,
    /// Set while the slot's queries are on the card.
    tag: Option<u64>,
}

impl GpuTimer {
    fn new(device: &ID3D11Device) -> Option<Self> {
        let query = |kind: D3D11_QUERY| {
            let mut query = None;
            // SAFETY: a valid description, and somewhere for the query to go.
            unsafe { device.CreateQuery(&D3D11_QUERY_DESC { Query: kind, MiscFlags: 0 }, Some(&mut query)) }.ok()?;
            query
        };
        let slot = || {
            Some(TimerSlot {
                disjoint: query(D3D11_QUERY_TIMESTAMP_DISJOINT)?,
                start: query(D3D11_QUERY_TIMESTAMP)?,
                end: query(D3D11_QUERY_TIMESTAMP)?,
                tag: None,
            })
        };
        Some(Self {
            slots: (0..TIMER_SLOTS).map(|_| slot()).collect::<Option<_>>()?,
            next: 0,
            check: Default::default(),
        })
    }

    /// Marks the start; `None` (untimed) if the slot is still waiting for an earlier result.
    unsafe fn begin(&mut self, context: &ID3D11DeviceContext) -> Option<usize> {
        let at = self.next;
        let slot = &self.slots[at];
        if slot.tag.is_some() {
            return None;
        }
        unsafe {
            context.Begin(&slot.disjoint);
            context.End(&slot.start);
        }
        Some(at)
    }

    unsafe fn end(&mut self, context: &ID3D11DeviceContext, at: usize, tag: u64) {
        let slot = &mut self.slots[at];
        unsafe {
            context.End(&slot.end);
            context.End(&slot.disjoint);
        }
        slot.tag = Some(tag);
        self.next = (at + 1) % self.slots.len();
    }

    /// Hands over the timings that are ready. False once the timings have proved meaningless
    /// (see [`crate::trace::TimerCheck`]), so the timer should be dropped.
    fn poll(&mut self, context: &ID3D11DeviceContext, mut out: impl FnMut(u64, u32)) -> bool {
        let count = self.slots.len();
        for i in 0..count {
            let slot = &mut self.slots[(self.next + i) % count];
            let Some(tag) = slot.tag else { continue };
            // SAFETY: each query is read into the type its kind returns.
            let Some(disjoint) = (unsafe { ready::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>(context, &slot.disjoint) })
            else {
                break; // later slots finish later
            };
            let (start, end) = unsafe { (ready::<u64>(context, &slot.start), ready::<u64>(context, &slot.end)) };
            let (Some(start), Some(end)) = (start, end) else { break };
            slot.tag = None;
            if disjoint.Disjoint.as_bool() || disjoint.Frequency == 0 || end < start {
                continue;
            }
            match self.check.check(end - start, disjoint.Frequency) {
                TimerVerdict::Keep => {
                    let us = u128::from(end - start) * 1_000_000 / u128::from(disjoint.Frequency);
                    out(tag, us.min(u128::from(u32::MAX)) as u32);
                }
                TimerVerdict::Skip => {}
                TimerVerdict::GiveUp => return false,
            }
        }
        true
    }
}

/// A query's result if the card has it, without waiting or flushing. (The wrapper's `GetData`
/// folds "not ready yet", `S_FALSE`, into success, so this calls the method directly.)
unsafe fn ready<T: Default>(context: &ID3D11DeviceContext, query: &ID3D11Query) -> Option<T> {
    let mut value = T::default();
    let result = unsafe {
        (Interface::vtable(context).GetData)(
            Interface::as_raw(context),
            query.as_raw(),
            (&raw mut value).cast(),
            size_of::<T>() as u32,
            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
        )
    };
    (result == windows::Win32::Foundation::S_OK).then_some(value)
}
