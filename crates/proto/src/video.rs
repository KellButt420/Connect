//! Encoded video frames split across QUIC datagrams.
//!
//! Every datagram carries a fixed [`HEADER_LEN`]-byte header followed by a slice of one
//! encoded frame (H.264 Annex-B access unit). Datagrams are never retransmitted, so a frame
//! with any missing chunk is dropped; the [`FrameAssembler`] then holds back further frames
//! until a keyframe arrives (decoding P-frames against a missing reference shows garbage)
//! and reports via [`FrameAssembler::needs_keyframe`] that one should be requested.

use std::collections::BTreeMap;

use anyhow::{Result, bail, ensure};
use bytes::{BufMut, Bytes, BytesMut};

/// First byte of every video datagram.
const KIND_VIDEO: u8 = 0x56;
const FLAG_KEYFRAME: u8 = 1 << 0;

/// kind u8 | flags u8 | chunk_index u16 | chunk_count u16 | frame_seq u32 |
/// capture_time_us u64 | encode_time_us u32
pub const HEADER_LEN: usize = 22;

/// Frames still being reassembled at once; older partial frames are abandoned beyond this.
const MAX_PARTIAL_FRAMES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameMeta {
    /// Increments by one per encoded frame.
    pub seq: u32,
    pub keyframe: bool,
    /// Host wall clock when the frame was captured.
    pub capture_time_us: u64,
    /// Time the host spent encoding this frame.
    pub encode_time_us: u32,
}

#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub meta: FrameMeta,
    pub data: Bytes,
}

/// Splits one encoded frame into datagrams of at most `max_datagram` bytes.
pub fn packetize(meta: FrameMeta, data: &[u8], max_datagram: usize) -> Result<Vec<Bytes>> {
    ensure!(max_datagram > HEADER_LEN, "datagram size {max_datagram} too small");
    let payload = max_datagram - HEADER_LEN;
    let count = data.len().div_ceil(payload).max(1);
    ensure!(count <= u16::MAX as usize, "frame too large: {} bytes", data.len());

    let flags = if meta.keyframe { FLAG_KEYFRAME } else { 0 };
    let mut chunks = Vec::with_capacity(count);
    for index in 0..count {
        let body = &data[(index * payload).min(data.len())..((index + 1) * payload).min(data.len())];
        let mut buf = BytesMut::with_capacity(HEADER_LEN + body.len());
        buf.put_u8(KIND_VIDEO);
        buf.put_u8(flags);
        buf.put_u16_le(index as u16);
        buf.put_u16_le(count as u16);
        buf.put_u32_le(meta.seq);
        buf.put_u64_le(meta.capture_time_us);
        buf.put_u32_le(meta.encode_time_us);
        buf.put_slice(body);
        chunks.push(buf.freeze());
    }
    Ok(chunks)
}

struct Chunk {
    meta: FrameMeta,
    index: u16,
    count: u16,
    body: Bytes,
}

fn parse(datagram: Bytes) -> Result<Chunk> {
    ensure!(datagram.len() >= HEADER_LEN, "short video datagram");
    if datagram[0] != KIND_VIDEO {
        bail!("unknown datagram kind {:#x}", datagram[0]);
    }
    let u16_at = |i: usize| u16::from_le_bytes([datagram[i], datagram[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes(datagram[i..i + 4].try_into().unwrap());
    let u64_at = |i: usize| u64::from_le_bytes(datagram[i..i + 8].try_into().unwrap());
    let chunk = Chunk {
        meta: FrameMeta {
            keyframe: datagram[1] & FLAG_KEYFRAME != 0,
            seq: u32_at(6),
            capture_time_us: u64_at(10),
            encode_time_us: u32_at(18),
        },
        index: u16_at(2),
        count: u16_at(4),
        body: datagram.slice(HEADER_LEN..),
    };
    ensure!(chunk.count > 0 && chunk.index < chunk.count, "bad chunk index");
    Ok(chunk)
}

struct Partial {
    meta: FrameMeta,
    chunks: Vec<Option<Bytes>>,
    received: usize,
}

/// Counters for the stats overlay.
#[derive(Debug, Default, Clone, Copy)]
pub struct AssemblerStats {
    pub frames_completed: u64,
    /// Frames that never completed (a chunk was lost or arrived too late).
    pub frames_lost: u64,
    /// Complete frames thrown away because they depended on a lost frame.
    pub frames_skipped: u64,
}

pub struct FrameAssembler {
    partial: BTreeMap<u32, Partial>,
    /// Highest sequence number handed out (or skipped); anything at or below it is stale.
    last_seq: Option<u32>,
    awaiting_keyframe: bool,
    pub stats: AssemblerStats,
}

impl Default for FrameAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameAssembler {
    pub fn new() -> Self {
        Self {
            partial: BTreeMap::new(),
            last_seq: None,
            // The decoder needs SPS/PPS + IDR before anything else.
            awaiting_keyframe: true,
            stats: AssemblerStats::default(),
        }
    }

    /// True while frames are being withheld until the next keyframe.
    pub fn needs_keyframe(&self) -> bool {
        self.awaiting_keyframe
    }

    /// Withholds frames until the next keyframe, e.g. after the decoder dropped or failed on one.
    pub fn request_keyframe(&mut self) {
        self.awaiting_keyframe = true;
    }

    /// Feeds one datagram; returns a frame once it is complete and safe to decode.
    pub fn push(&mut self, datagram: Bytes) -> Result<Option<EncodedFrame>> {
        let chunk = parse(datagram)?;
        let seq = chunk.meta.seq;
        if self.last_seq.is_some_and(|last| seq <= last) {
            return Ok(None); // late chunk of a frame already completed or abandoned
        }

        let partial = self.partial.entry(seq).or_insert_with(|| Partial {
            meta: chunk.meta,
            chunks: vec![None; chunk.count as usize],
            received: 0,
        });
        ensure!(partial.chunks.len() == chunk.count as usize, "inconsistent chunk count");
        let slot = &mut partial.chunks[chunk.index as usize];
        if slot.is_none() {
            *slot = Some(chunk.body);
            partial.received += 1;
        }

        if partial.received < partial.chunks.len() {
            if self.partial.len() > MAX_PARTIAL_FRAMES {
                // Counted as lost once a later frame completes and exposes the gap.
                self.partial.pop_first();
            }
            return Ok(None);
        }

        let done = self.partial.remove(&seq).expect("frame present");
        // Anything older than a completed frame will not be used any more.
        while self.partial.first_key_value().is_some_and(|(&s, _)| s < seq) {
            self.partial.pop_first();
        }
        // Every frame between the previous one and this one is lost, whether some of its
        // chunks arrived or none did.
        if let Some(last) = self.last_seq {
            let missing = seq - last - 1;
            if missing > 0 {
                self.stats.frames_lost += missing as u64;
                self.awaiting_keyframe = true;
            }
        }
        self.last_seq = Some(seq);

        if self.awaiting_keyframe && !done.meta.keyframe {
            self.stats.frames_skipped += 1;
            return Ok(None);
        }
        self.awaiting_keyframe = false;
        self.stats.frames_completed += 1;

        let len = done.chunks.iter().flatten().map(Bytes::len).sum();
        let mut data = BytesMut::with_capacity(len);
        for body in done.chunks.into_iter().flatten() {
            data.put(body);
        }
        Ok(Some(EncodedFrame { meta: done.meta, data: data.freeze() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u32, keyframe: bool, len: usize) -> (FrameMeta, Vec<u8>) {
        let meta = FrameMeta { seq, keyframe, capture_time_us: 1_000 + seq as u64, encode_time_us: 7 };
        (meta, (0..len).map(|i| (i * 7 + seq as usize) as u8).collect())
    }

    #[test]
    fn reassembles_out_of_order_chunks() {
        let (meta, data) = frame(0, true, 5000);
        let mut chunks = packetize(meta, &data, 1200).unwrap();
        assert_eq!(chunks.len(), 5000usize.div_ceil(1200 - HEADER_LEN));
        assert!(chunks.iter().all(|c| c.len() <= 1200));
        chunks.reverse();

        let mut asm = FrameAssembler::new();
        let mut out = None;
        for c in chunks {
            out = asm.push(c).unwrap().or(out);
        }
        let out = out.expect("frame completes");
        assert_eq!(out.meta, meta);
        assert_eq!(&out.data[..], &data[..]);
        assert!(!asm.needs_keyframe());
    }

    #[test]
    fn empty_frame_is_one_chunk() {
        let (meta, _) = frame(0, true, 0);
        let chunks = packetize(meta, &[], 1200).unwrap();
        assert_eq!(chunks.len(), 1);
        let out = FrameAssembler::new().push(chunks[0].clone()).unwrap().unwrap();
        assert!(out.data.is_empty());
    }

    #[test]
    fn waits_for_first_keyframe() {
        let mut asm = FrameAssembler::new();
        let (meta, data) = frame(0, false, 100);
        for c in packetize(meta, &data, 1200).unwrap() {
            assert!(asm.push(c).unwrap().is_none());
        }
        assert!(asm.needs_keyframe());
        let (meta, data) = frame(1, true, 100);
        let got = packetize(meta, &data, 1200).unwrap().into_iter().find_map(|c| asm.push(c).unwrap());
        assert!(got.unwrap().meta.keyframe);
    }

    #[test]
    fn lost_chunk_drops_frame_and_holds_until_keyframe() {
        let mut asm = FrameAssembler::new();
        let push_all = |asm: &mut FrameAssembler, seq, key, skip_first: bool| {
            let (meta, data) = frame(seq, key, 3000);
            let chunks = packetize(meta, &data, 1200).unwrap();
            chunks.into_iter().skip(skip_first as usize).filter_map(|c| asm.push(c).unwrap()).last()
        };

        assert!(push_all(&mut asm, 0, true, false).is_some());
        assert!(push_all(&mut asm, 1, false, false).is_some());
        assert!(push_all(&mut asm, 2, false, true).is_none()); // chunk lost
        assert!(push_all(&mut asm, 3, false, false).is_none()); // depends on frame 2
        assert!(asm.needs_keyframe());
        assert_eq!(asm.stats.frames_lost, 1);
        assert_eq!(asm.stats.frames_skipped, 1);

        let key = push_all(&mut asm, 4, true, false).expect("keyframe recovers");
        assert_eq!(key.meta.seq, 4);
        assert!(!asm.needs_keyframe());
        assert!(push_all(&mut asm, 5, false, false).is_some());
    }

    #[test]
    fn wholly_missing_frame_is_a_gap() {
        let mut asm = FrameAssembler::new();
        for (seq, key) in [(0, true), (2, false)] {
            let (meta, data) = frame(seq, key, 10);
            for c in packetize(meta, &data, 1200).unwrap() {
                asm.push(c).unwrap();
            }
        }
        assert!(asm.needs_keyframe());
        assert_eq!(asm.stats.frames_lost, 1);
    }

    #[test]
    fn late_chunks_are_ignored() {
        let mut asm = FrameAssembler::new();
        let (meta, data) = frame(0, true, 10);
        let chunk = packetize(meta, &data, 1200).unwrap().remove(0);
        assert!(asm.push(chunk.clone()).unwrap().is_some());
        assert!(asm.push(chunk).unwrap().is_none());
    }

    #[test]
    fn rejects_garbage() {
        let mut asm = FrameAssembler::new();
        assert!(asm.push(Bytes::from_static(b"nope")).is_err());
        assert!(asm.push(Bytes::from(vec![0u8; HEADER_LEN])).is_err());
    }
}
