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
}
