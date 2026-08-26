//! Which bitstream is written into the recording mux.
//!
//! Pre-FFR is a second encode of the composition SBS (same layer as screenshots).
//! Stream copy is the packed foveated bitstream sent to the headset (legacy / fallback).

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingVideoSource {
    /// Wait for pre-FFR encoder NALs; do not mux headset stream packets.
    PreFfr,
    /// Mux the same NALs sent to the headset (packed FFR when foveation is on).
    StreamCopy,
}

/// Headset-stream NALs go into the MKV until the dedicated pre-FFR encoder
/// actually emits NALs (init failed, not started yet, or Linux fallback).
pub fn mux_stream_nals(recording_active: bool, pre_ffr_nals_seen: bool) -> bool {
    recording_active && !pre_ffr_nals_seen
}

/// Once any pre-FFR NAL arrives, keep using that source for the rest of the session.
pub fn on_pre_ffr_nal(current: RecordingVideoSource) -> RecordingVideoSource {
    let _ = current;
    RecordingVideoSource::PreFfr
}

/// Stream VideoSend strips SPS/PPS into decoder_config. The mux file needs those
/// NALs in-band or ffmpeg reports "non-existing PPS" and writes a 0-byte MKV.
pub fn should_prefix_decoder_config(is_idr: bool, already_prefixed: bool) -> bool {
    is_idr || !already_prefixed
}

/// Keep this frame for a recording capped at `max_fps`. `max_fps < 1` means uncapped.
/// `since_last == None` keeps the first frame. Uses wall-clock gaps, not pose timestamps
/// (ALVR's encoder timestamp is a frame index, not nanoseconds).
pub fn should_keep_recording_frame(since_last: Option<Duration>, max_fps: f32) -> bool {
    if !(max_fps >= 1.0) {
        return true;
    }
    let Some(dt) = since_last else {
        return true;
    };
    let fps = max_fps.round().clamp(1.0, 240.0) as u64;
    dt >= Duration::from_nanos(1_000_000_000 / fps)
}

/// H.264 NVENC max edge. HEVC/AV1 can go to 8192.
pub fn recording_codec_max_dim(h264: bool) -> u32 {
    if h264 { 4096 } else { 8192 }
}

/// `0` means "no extra cap" (codec limit only). Values below 32 are treated the same.
pub fn recording_user_max_dim(setting: u32) -> Option<u32> {
    if setting >= 32 { Some(setting) } else { None }
}

fn align32(v: u32) -> u32 {
    let a = v & !31;
    if a < 32 { 32 } else { a }
}

/// Scale SBS source to fit `max_dim` on the longer edge, 32-pixel aligned.
pub fn fit_recording_encode_size(src_w: u32, src_h: u32, max_dim: u32) -> (u32, u32) {
    let src_w = src_w.max(1);
    let src_h = src_h.max(1);
    let max_dim = max_dim.max(32);
    let mut scale = 1.0f32;
    if src_w > max_dim {
        scale = max_dim as f32 / src_w as f32;
    }
    if ((src_h as f32 * scale) as u32) > max_dim {
        scale = max_dim as f32 / src_h as f32;
    }
    let w = align32((src_w as f32 * scale).round() as u32);
    let h = align32((src_h as f32 * scale).round() as u32);
    (w, h)
}

pub fn recording_encode_size(
    src_w: u32,
    src_h: u32,
    user_max_dim: u32,
    h264: bool,
) -> (u32, u32) {
    let codec_max = recording_codec_max_dim(h264);
    let max_dim = recording_user_max_dim(user_max_dim)
        .map(|u| u.min(codec_max))
        .unwrap_or(codec_max);
    fit_recording_encode_size(src_w, src_h, max_dim)
}

/// Staging copies on the encoder thread; NVENC Submit+Drain only on the worker.
pub const RECORDING_PIPELINE_SLOTS: u32 = 3;

/// Keep feeding NVENC while a slot is free. Drop only when the ring is full.
#[allow(dead_code)] // C++ encoder thread implements the same rule; tests lock the policy.
pub fn should_submit_recording_encode(in_flight: u32, max_in_flight: u32) -> bool {
    in_flight < max_in_flight.max(1)
}

/// SPS/PPS blobs must not move video t=0; only coded pictures do.
pub fn should_anchor_video_clock(picture_count: u64) -> bool {
    picture_count > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_mux_when_not_recording() {
        assert!(!mux_stream_nals(false, false));
        assert!(!mux_stream_nals(false, true));
    }

    #[test]
    fn copies_stream_until_pre_ffr_arrives() {
        assert!(mux_stream_nals(true, false));
        assert!(!mux_stream_nals(true, true));
    }

    #[test]
    fn pre_ffr_nal_switches_source() {
        assert_eq!(
            on_pre_ffr_nal(RecordingVideoSource::StreamCopy),
            RecordingVideoSource::PreFfr
        );
    }

    #[test]
    fn stream_copy_needs_sps_pps_before_first_slice() {
        assert!(should_prefix_decoder_config(false, false));
        assert!(should_prefix_decoder_config(true, false));
        assert!(should_prefix_decoder_config(true, true));
        assert!(!should_prefix_decoder_config(false, true));
    }

    #[test]
    fn zero_max_fps_keeps_every_frame() {
        assert!(should_keep_recording_frame(None, 0.0));
        assert!(should_keep_recording_frame(Some(Duration::from_millis(1)), 0.0));
    }

    #[test]
    fn cap_30fps_keeps_about_every_third_90hz_frame() {
        let dt = Duration::from_nanos(1_000_000_000 / 90);
        let mut last: Option<Duration> = None; // elapsed since previous keep, simulated
        let mut kept = 0u32;
        let mut acc = Duration::ZERO;
        let mut last_keep_at = Duration::ZERO;
        for i in 0..90u32 {
            let now = dt * i;
            let since = if kept == 0 {
                None
            } else {
                Some(now.saturating_sub(last_keep_at))
            };
            let _ = last;
            if should_keep_recording_frame(since, 30.0) {
                last_keep_at = now;
                last = Some(acc);
                kept += 1;
            }
            acc += dt;
        }
        assert!(
            (28..=32).contains(&kept),
            "expected ~30 kept frames, got {kept}"
        );
    }

    #[test]
    fn default_1920_cap_halves_3840x1792_sbs() {
        assert_eq!(
            recording_encode_size(3840, 1792, 1920, true),
            (1920, 896)
        );
    }

    #[test]
    fn zero_user_cap_keeps_source_under_h264_limit() {
        assert_eq!(
            recording_encode_size(3840, 1792, 0, true),
            (3840, 1792)
        );
    }

    #[test]
    fn user_cap_cannot_exceed_h264_4096() {
        let (w, h) = recording_encode_size(7680, 3584, 8192, true);
        assert!(w <= 4096 && h <= 4096, "got {w}x{h}");
        assert_eq!((w, h), fit_recording_encode_size(7680, 3584, 4096));
    }

    #[test]
    fn hevc_allows_8192_when_user_uncapped() {
        let (w, h) = recording_encode_size(7680, 2160, 0, false);
        assert_eq!((w, h), fit_recording_encode_size(7680, 2160, 8192));
        assert!(w <= 8192 && h <= 8192);
    }

    #[test]
    fn tiny_source_is_at_least_32_aligned() {
        let (w, h) = recording_encode_size(16, 16, 1920, true);
        assert!(w >= 32 && h >= 32);
        assert_eq!(w % 32, 0);
        assert_eq!(h % 32, 0);
    }

    #[test]
    fn pipeline_accepts_frames_until_slots_full() {
        assert_eq!(RECORDING_PIPELINE_SLOTS, 3);
        assert!(should_submit_recording_encode(0, RECORDING_PIPELINE_SLOTS));
        assert!(should_submit_recording_encode(2, RECORDING_PIPELINE_SLOTS));
        assert!(!should_submit_recording_encode(3, RECORDING_PIPELINE_SLOTS));
        assert!(!should_submit_recording_encode(4, RECORDING_PIPELINE_SLOTS));
    }

    #[test]
    fn config_nals_do_not_anchor_the_video_clock() {
        assert!(!should_anchor_video_clock(0));
        assert!(should_anchor_video_clock(1));
    }
}
