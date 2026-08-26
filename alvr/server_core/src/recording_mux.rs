//! Live capture of bitstream video + PCM audio, finalized into one Matroska via ffmpeg.
//!
//! Realtime path only appends elementary streams to temp files (cheap, no pipe deadlocks).
//! On stop, a background job remuxes with `-c:v copy` / PCM audio.
//!
//! Windows WASAPI loopback often delivers **no buffers while output is silent**. Without
//! compensation the PCM track would start at the first audible sample and stay shorter than
//! video. The audio writer inserts s16le silence for true gaps.
//!
//! Two clocks must not be mixed: ffmpeg `-genpts` puts the **first video packet** at t=0
//! (encoder / IDR often hundreds of ms after F9), while loopback samples exist from F9.
//! Padding audio from F9 then muxing against video-t0 makes the soundtrack late — the
//! same class of A/V mismatch as a truncated WAV, just the other direction. Audio is
//! therefore timed from the first video packet; pre-roll samples are dropped.
//! Incoming PCM is timestamped in the WASAPI callback. Gaps are filled only from
//! **capture time**, never from writer-recv time or a 50ms timeout (timeout silence
//! plus the real buffer for the same interval stretched the soundtrack; lag grew
//! toward the end of the take).
//!
//! Video is muxed at `pictures / span`, not the recording-fps setting. A CFR label
//! higher than the actual picture rate makes the video track shorter than audio,
//! which also looks like audio lag that grows.

use alvr_common::{error, info, warn};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// PCM (or video NALs) with the Instant they were captured, not when the mux thread woke.
#[derive(Debug, Clone)]
pub struct TimedBytes {
    pub at: Instant,
    pub data: Vec<u8>,
}

pub struct LiveMuxSession {
    video_tx: Option<flume::Sender<TimedBytes>>,
    audio_tx: Option<flume::Sender<TimedBytes>>,
    join: Option<JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    output_path: PathBuf,
}

impl LiveMuxSession {
    pub fn start(
        output_mkv: PathBuf,
        sample_rate: u32,
        codec_hint: &str,
        fallback_fps: f32,
    ) -> Result<Self, String> {
        if let Some(parent) = output_mkv.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }

        let ffmpeg = find_ffmpeg().ok_or_else(|| {
            "ffmpeg not found (install on PATH or use deps/windows/ffmpeg/bin/ffmpeg.exe)"
                .to_string()
        })?;

        let sample_rate = sample_rate.max(8000);
        let demux = match codec_hint {
            "hevc" | "h265" => "hevc",
            "av1" => "av1",
            _ => "h264",
        }
        .to_string();

        let (video_tx, video_rx) = flume::unbounded::<TimedBytes>();
        let (audio_tx, audio_rx) = flume::unbounded::<TimedBytes>();
        let output_path = output_mkv.clone();
        let demux_for_log = demux.clone();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_for_worker = Arc::clone(&stop_flag);

        let join = thread::spawn(move || {
            if let Err(e) = capture_and_mux_worker(
                ffmpeg,
                output_path.clone(),
                sample_rate,
                &demux,
                fallback_fps,
                video_rx,
                audio_rx,
                stop_for_worker,
            ) {
                error!("Recording mux failed: {e}");
            } else {
                info!("Recording MKV finished: {}", output_path.display());
            }
        });

        info!(
            "Recording started ({} Hz, {}, fallback {:.0} fps): {}",
            sample_rate,
            demux_for_log,
            fallback_fps,
            output_mkv.display()
        );

        Ok(Self {
            video_tx: Some(video_tx),
            audio_tx: Some(audio_tx),
            join: Some(join),
            stop_flag,
            output_path: output_mkv,
        })
    }

    pub fn push_video(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        if let Some(tx) = &self.video_tx {
            let _ = tx.send(TimedBytes {
                at: Instant::now(),
                data: data.to_vec(),
            });
        }
    }

    pub fn push_audio(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        if let Some(tx) = &self.audio_tx {
            let _ = tx.send(TimedBytes {
                at: Instant::now(),
                data: data.to_vec(),
            });
        }
    }

    pub fn audio_sender(&self) -> flume::Sender<TimedBytes> {
        self.audio_tx
            .as_ref()
            .expect("recording audio sender")
            .clone()
    }

    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    /// Close queues and wait for remux (may block). Prefer [`finish_async`] from hotkeys.
    pub fn finish(mut self) {
        self.begin_stop();
        if let Some(j) = self.join.take() {
            wait_join(j, Duration::from_secs(120));
        }
    }

    /// Non-blocking stop for the VR/hotkey path.
    pub fn finish_async(mut self) {
        self.begin_stop();
        if let Some(j) = self.join.take() {
            thread::spawn(move || {
                wait_join(j, Duration::from_secs(120));
            });
        }
    }

    fn begin_stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        // Closing senders ends the capture threads after they drain queued data.
        self.video_tx = None;
        self.audio_tx = None;
    }
}

impl Drop for LiveMuxSession {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        self.video_tx = None;
        self.audio_tx = None;
        if let Some(j) = self.join.take() {
            wait_join(j, Duration::from_secs(30));
        }
    }
}

fn wait_join(join: JoinHandle<()>, timeout: Duration) {
    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    thread::spawn(move || {
        let _ = join.join();
        flag.store(true, Ordering::SeqCst);
    });
    let start = Instant::now();
    while !done.load(Ordering::SeqCst) && start.elapsed() < timeout {
        thread::sleep(Duration::from_millis(50));
    }
    if !done.load(Ordering::SeqCst) {
        warn!("Recording finalize still running after {timeout:?}");
    }
}

fn find_ffmpeg() -> Option<PathBuf> {
    // PATH ffmpeg may be an old build (this tree has seen Lavf58 muxes). Prefer
    // the copy next to the streamer / in deps so setts restamp is available.
    let mut candidates = Vec::new();
    if let Some(layout) = crate::FILESYSTEM_LAYOUT.get() {
        let mut dir = layout.executables_dir.clone();
        for _ in 0..8 {
            for rel in [
                "deps/windows/ffmpeg/bin/ffmpeg.exe",
                "ffmpeg.exe",
                "bin/win64/ffmpeg.exe",
                "bin/ffmpeg",
            ] {
                candidates.push(dir.join(rel));
            }
            if !dir.pop() {
                break;
            }
        }
    }
    candidates.push(PathBuf::from("deps/windows/ffmpeg/bin/ffmpeg.exe"));
    candidates.push(PathBuf::from("ffmpeg.exe"));
    if let Ok(out) = Command::new(if cfg!(windows) { "where" } else { "which" })
        .arg("ffmpeg")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let p = PathBuf::from(line.trim());
                if p.is_file() {
                    candidates.push(p);
                }
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Stereo s16le frame size.
const PCM_FRAME_BYTES: u64 = 4;

fn align_pcm(nbytes: u64) -> u64 {
    nbytes - (nbytes % PCM_FRAME_BYTES)
}

fn pcm_bytes_for_duration(dt: Duration, sample_rate: u32) -> u64 {
    let frames = (dt.as_secs_f64() * f64::from(sample_rate)).round() as u64;
    frames * PCM_FRAME_BYTES
}

fn pcm_bytes_between(t0: Instant, t: Instant, sample_rate: u32) -> u64 {
    if t <= t0 {
        return 0;
    }
    pcm_bytes_for_duration(t.duration_since(t0), sample_rate)
}

/// Where silence should end before appending a PCM chunk captured over the last
/// `chunk_len` bytes. Using `wall` itself would place those samples *after* now.
pub(crate) fn pad_target_before_chunk(wall_bytes: u64, chunk_len: usize) -> u64 {
    let chunk = align_pcm(chunk_len as u64);
    align_pcm(wall_bytes.saturating_sub(chunk))
}

fn write_silence(file: &mut File, mut nbytes: u64) -> Result<(), String> {
    // Keep PCM frame-aligned.
    nbytes -= nbytes % PCM_FRAME_BYTES;
    if nbytes == 0 {
        return Ok(());
    }
    // Reuse a modest zero buffer instead of allocating multi‑second vectors.
    let chunk = vec![0u8; 16 * 1024];
    let mut left = nbytes;
    while left > 0 {
        let n = left.min(chunk.len() as u64) as usize;
        file.write_all(&chunk[..n])
            .map_err(|e| format!("write silence: {e}"))?;
        left -= n as u64;
    }
    Ok(())
}

/// Ensure written PCM reaches at least `target_bytes` by inserting silence.
fn pad_audio_to(
    file: &mut File,
    written: &mut u64,
    silence_padded: &mut u64,
    target_bytes: u64,
) -> Result<(), String> {
    let target = align_pcm(target_bytes);
    if target <= *written {
        return Ok(());
    }
    let gap = target - *written;
    write_silence(file, gap)?;
    *written += gap;
    *silence_padded += gap;
    Ok(())
}

fn capture_and_mux_worker(
    ffmpeg: PathBuf,
    output_mkv: PathBuf,
    sample_rate: u32,
    demux: &str,
    fallback_fps: f32,
    video_rx: flume::Receiver<TimedBytes>,
    audio_rx: flume::Receiver<TimedBytes>,
    _stop_flag: Arc<AtomicBool>,
) -> Result<(), String> {
    let started = Instant::now();
    let video_t0 = Arc::new(std::sync::Mutex::new(None::<Instant>));
    let video_t1 = Arc::new(std::sync::Mutex::new(None::<Instant>));

    let video_tmp = output_mkv.with_extension("video.tmp");
    let audio_tmp = output_mkv.with_extension("audio.tmp");
    let _ = fs::remove_file(&video_tmp);
    let _ = fs::remove_file(&audio_tmp);

    let mut video_file =
        File::create(&video_tmp).map_err(|e| format!("create video temp: {e}"))?;
    let mut audio_file =
        File::create(&audio_tmp).map_err(|e| format!("create audio temp: {e}"))?;

    let video_bytes = Arc::new(AtomicU64::new(0));
    let audio_bytes = Arc::new(AtomicU64::new(0));
    let audio_silence = Arc::new(AtomicU64::new(0));
    let video_packets = Arc::new(AtomicU64::new(0));
    let video_pictures = Arc::new(AtomicU64::new(0));

    let vb = Arc::clone(&video_bytes);
    let vp = Arc::clone(&video_packets);
    let vpic = Arc::clone(&video_pictures);
    let video_t0_v = Arc::clone(&video_t0);
    let video_t1_v = Arc::clone(&video_t1);
    let demux_owned = demux.to_string();
    let v_thread = thread::spawn(move || -> Result<(), String> {
        let mut since_flush = 0u64;
        while let Ok(chunk) = video_rx.recv() {
            if chunk.data.is_empty() {
                continue;
            }
            {
                let mut t0 = video_t0_v.lock().unwrap();
                if t0.is_none() {
                    *t0 = Some(chunk.at);
                }
                *video_t1_v.lock().unwrap() = Some(chunk.at);
            }
            let pics = count_coded_pictures(&chunk.data, &demux_owned);
            vpic.fetch_add(pics, Ordering::Relaxed);
            video_file
                .write_all(&chunk.data)
                .map_err(|e| format!("write video temp: {e}"))?;
            vb.fetch_add(chunk.data.len() as u64, Ordering::Relaxed);
            vp.fetch_add(1, Ordering::Relaxed);
            since_flush += 1;
            if since_flush >= 30 {
                video_file
                    .flush()
                    .map_err(|e| format!("flush video temp: {e}"))?;
                since_flush = 0;
            }
        }
        video_file
            .flush()
            .map_err(|e| format!("flush video temp: {e}"))?;
        Ok(())
    });

    let ab = Arc::clone(&audio_bytes);
    let asil = Arc::clone(&audio_silence);
    let video_t0_a = Arc::clone(&video_t0);
    let a_thread = thread::spawn(move || -> Result<(), String> {
        let mut written = 0u64;
        let mut silence_padded = 0u64;

        loop {
            match audio_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(chunk) => {
                    if chunk.data.is_empty() {
                        continue;
                    }
                    let Some(origin) = *video_t0_a.lock().unwrap() else {
                        // Pre-roll: pictures have not started; ffmpeg video t=0 is later.
                        continue;
                    };
                    if chunk.at <= origin {
                        continue;
                    }
                    pad_audio_to(
                        &mut audio_file,
                        &mut written,
                        &mut silence_padded,
                        pad_target_before_chunk(
                            pcm_bytes_between(origin, chunk.at, sample_rate),
                            chunk.data.len(),
                        ),
                    )?;
                    audio_file
                        .write_all(&chunk.data)
                        .map_err(|e| format!("write audio temp: {e}"))?;
                    written += chunk.data.len() as u64;
                }
                Err(flume::RecvTimeoutError::Timeout) => {
                    // Do not invent silence here — a later real buffer would stack on it
                    // and stretch the soundtrack (lag grows toward the end).
                    let _ = audio_file.flush();
                }
                Err(flume::RecvTimeoutError::Disconnected) => break,
            }
        }

        audio_file
            .flush()
            .map_err(|e| format!("flush audio temp: {e}"))?;
        ab.store(written, Ordering::Relaxed);
        asil.store(silence_padded, Ordering::Relaxed);
        Ok(())
    });

    let v_res = v_thread
        .join()
        .map_err(|_| "video writer panicked".to_string())?;
    let a_res = a_thread
        .join()
        .map_err(|_| "audio writer panicked".to_string())?;
    v_res?;
    a_res?;

    let wall = started.elapsed();
    let v_bytes = video_bytes.load(Ordering::Relaxed);
    let mut a_bytes = audio_bytes.load(Ordering::Relaxed);
    let mut a_silence = audio_silence.load(Ordering::Relaxed);
    let packets = video_packets.load(Ordering::Relaxed);
    let pictures = video_pictures.load(Ordering::Relaxed);
    let t0 = *video_t0.lock().unwrap();
    let t1 = *video_t1.lock().unwrap();
    let video_span = match (t0, t1) {
        (Some(a), Some(b)) if b > a => b.duration_since(a),
        _ => wall,
    };

    // Pad/trim the PCM file to the video span after both writers have finished,
    // so the last picture's timestamp is known.
    if v_bytes > 0 {
        let target = pcm_bytes_for_duration(video_span, sample_rate);
        if a_bytes < target {
            let mut audio_file = fs::OpenOptions::new()
                .append(true)
                .open(&audio_tmp)
                .map_err(|e| format!("reopen audio temp: {e}"))?;
            let mut written = a_bytes;
            let mut extra = 0u64;
            pad_audio_to(&mut audio_file, &mut written, &mut extra, target)?;
            audio_file
                .flush()
                .map_err(|e| format!("flush audio pad: {e}"))?;
            a_silence += extra;
            a_bytes = written;
        }
        // Do not truncate PCM to the video span. Extra samples are the real
        // session length when pictures were timestamped too fast (72 Hz / 30 cap
        // -> ~24 pictures/s labeled as 30).
    }

    let audio_secs = a_bytes as f64 / (f64::from(sample_rate) * PCM_FRAME_BYTES as f64);
    let silence_secs = a_silence as f64 / (f64::from(sample_rate) * PCM_FRAME_BYTES as f64);
    let span_s = video_span.as_secs_f64();
    // Prefer the longer of video-span vs captured PCM: a 72 Hz stream capped at 30
    // keeps ~24 pictures/s. Labeling those as 30 fps makes video ~20% short of audio.
    let duration_s = span_s.max(audio_secs);
    let fps = mux_fps_from_pictures(
        pictures,
        Duration::from_secs_f64(duration_s.max(0.05)),
        fallback_fps,
    );

    info!(
        "Recording analysis: wall={:.3}s video_span={:.3}s audio={:.3}s pictures={} packets={} mux_fps={:.4} cap_fps={:.2} sample_rate={} silence={:.3}s ffmpeg={}",
        wall.as_secs_f64(),
        span_s,
        audio_secs,
        pictures,
        packets,
        fps,
        fallback_fps,
        sample_rate,
        silence_secs,
        ffmpeg.display()
    );
    if pictures >= 2 && duration_s > 0.05 {
        let implied = pictures as f64 / duration_s;
        if (fallback_fps as f64 - implied).abs() > 1.5 {
            warn!(
                "Recording fps mismatch: cap/fallback {:.2} vs actual {:.2} ({pictures} pictures in {duration_s:.3}s). Muxing at actual rate so A/V durations match.",
                fallback_fps, implied
            );
        }
    }

    if v_bytes == 0 && a_bytes == 0 {
        let _ = fs::remove_file(&video_tmp);
        let _ = fs::remove_file(&audio_tmp);
        return Err("no video or audio data captured".into());
    }

    let result = run_ffmpeg_mux(
        &ffmpeg,
        &output_mkv,
        &video_tmp,
        &audio_tmp,
        demux,
        sample_rate,
        fps,
        v_bytes > 0,
        a_bytes > 0,
        duration_s,
    );

    let _ = fs::remove_file(&video_tmp);
    let _ = fs::remove_file(&audio_tmp);

    result
}

pub(crate) fn mux_fps_from_pictures(pictures: u64, span: Duration, fallback_fps: f32) -> f32 {
    let fallback = if fallback_fps.is_finite() && fallback_fps >= 1.0 {
        fallback_fps
    } else {
        72.0
    };
    let span_s = span.as_secs_f64();
    if pictures >= 2 && span_s >= 0.05 {
        let fps = pictures as f64 / span_s;
        if (5.0..=240.0).contains(&fps) {
            return fps as f32;
        }
    }
    fallback
}

fn for_each_nal_payload(data: &[u8], mut visit: impl FnMut(&[u8])) {
    let mut i = 0usize;
    while i + 3 < data.len() {
        let start = if data[i..].starts_with(&[0, 0, 0, 1]) {
            i + 4
        } else if data[i..].starts_with(&[0, 0, 1]) {
            i + 3
        } else {
            i += 1;
            continue;
        };
        let mut j = start;
        let next = loop {
            if j + 3 > data.len() {
                break data.len();
            }
            if data[j..].starts_with(&[0, 0, 0, 1]) || data[j..].starts_with(&[0, 0, 1]) {
                break j;
            }
            j += 1;
        };
        if start < next {
            visit(&data[start..next]);
        }
        i = next;
    }
}

pub(crate) fn count_coded_pictures(data: &[u8], demux: &str) -> u64 {
    match demux {
        "hevc" | "h265" => {
            let mut n = 0u64;
            for_each_nal_payload(data, |nal| {
                if nal.is_empty() {
                    return;
                }
                let nal_type = (nal[0] >> 1) & 0x3F;
                let is_slice = nal_type <= 9 || (16..=21).contains(&nal_type);
                if is_slice {
                    n += 1;
                }
            });
            n
        }
        "av1" => {
            if data.is_empty() {
                0
            } else {
                1
            }
        }
        _ => {
            let mut n = 0u64;
            for_each_nal_payload(data, |nal| {
                if nal.is_empty() {
                    return;
                }
                let nal_type = nal[0] & 0x1F;
                if nal_type == 1 || nal_type == 5 {
                    n += 1;
                }
            });
            n
        }
    }
}

fn run_ffmpeg_mux(
    ffmpeg: &Path,
    output_mkv: &Path,
    video_tmp: &Path,
    audio_tmp: &Path,
    demux: &str,
    sample_rate: u32,
    fps: f32,
    has_video: bool,
    has_audio: bool,
    duration_s: f64,
) -> Result<(), String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y");

    if has_video {
        cmd.arg("-fflags")
            .arg("+genpts")
            .arg("-f")
            .arg(demux)
            .arg("-framerate")
            .arg(format!("{fps:.3}"))
            .arg("-i")
            .arg(video_tmp.as_os_str());
    }

    if has_audio {
        cmd.arg("-f")
            .arg("s16le")
            .arg("-ar")
            .arg(sample_rate.to_string())
            .arg("-ac")
            .arg("2")
            .arg("-i")
            .arg(audio_tmp.as_os_str());
    }

    if has_video {
        cmd.arg("-c:v").arg("copy");
    }
    if has_audio {
        // Lossless PCM in MKV (same idea as previous live path).
        cmd.arg("-c:a").arg("pcm_s16le");
    }

    if has_video && fps.is_finite() && fps >= 1.0 {
        // NVENC VUI still says the cap (e.g. 30). Players would then run 24
        // pictures/s at 30 fps. Rewrite packet PTS to the actual picture rate.
        cmd.arg("-bsf:v")
            .arg(format!("setts=pts=N/{fps:.6}/TB:dts=N/{fps:.6}/TB"));
    }
    let mux_tmp = output_mkv.with_extension("muxing.mkv");
    let _ = fs::remove_file(&mux_tmp);
    cmd.arg(mux_tmp.as_os_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    info!(
        "Recording ffmpeg: {} -f {demux} -framerate {fps:.4} -c:v copy setts={fps:.4}Hz duration_hint={duration_s:.3}s -> {}",
        ffmpeg.display(),
        output_mkv.display()
    );

    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn ffmpeg ({}): {e}", ffmpeg.display()))?;
    let result = finish_ffmpeg(child);
    match result {
        Ok(()) => {
            fs::rename(&mux_tmp, output_mkv).map_err(|e| {
                let _ = fs::remove_file(&mux_tmp);
                format!("rename mux output: {e}")
            })?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&mux_tmp);
            Err(e)
        }
    }
}

fn finish_ffmpeg(mut child: Child) -> Result<(), String> {
    let stderr_handle = child.stderr.take().map(|mut stderr| {
        thread::spawn(move || {
            let mut err_text = String::new();
            let _ = stderr.read_to_string(&mut err_text);
            err_text
        })
    });

    let status = child.wait().map_err(|e| format!("ffmpeg wait: {e}"))?;
    let err_text = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    if status.success() {
        Ok(())
    } else {
        let msg = err_text.trim();
        if msg.is_empty() {
            Err(format!("ffmpeg exit {status}"))
        } else {
            Err(format!("ffmpeg exit {status}: {msg}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_before_chunk_does_not_shift_samples_past_now() {
        // 100 ms of stereo s16le at 48 kHz = 19200 bytes.
        let wall = 19200u64;
        let chunk = 19200usize;
        assert_eq!(pad_target_before_chunk(wall, chunk), 0);
    }

    #[test]
    fn pad_before_chunk_keeps_true_gaps() {
        let wall = 48000u64;
        let chunk = 19200usize;
        assert_eq!(pad_target_before_chunk(wall, chunk), 28800);
    }

    #[test]
    fn pad_before_chunk_aligns_pcm_frames() {
        assert_eq!(pad_target_before_chunk(7, 3), 4);
    }

    #[test]
    fn mux_fps_matches_picture_span_not_the_cap_setting() {
        let fps = mux_fps_from_pictures(270, Duration::from_secs(10), 30.0);
        assert!(
            (26.5..=27.5).contains(&fps),
            "expected ~27 fps from 270 pictures in 10s, got {fps}"
        );
    }

    #[test]
    fn mux_fps_matches_72hz_keep_every_third_over_audio_length() {
        // User take: 331 pictures, 13.781s audio, cap 30, headset 72 Hz.
        let span = Duration::from_secs_f64(13.781);
        let fps = mux_fps_from_pictures(331, span, 30.0);
        assert!(
            (23.5..=24.5).contains(&fps),
            "expected ~24.02 fps, got {fps}"
        );
        let video_if_labeled_30 = 331.0_f64 / 30.0;
        assert!((video_if_labeled_30 - 11.03).abs() < 0.05);
    }

    #[test]
    fn h264_sps_is_not_a_picture() {
        // NAL type 7 (SPS)
        let data = [0, 0, 0, 1, 0x67, 0x42, 0x00, 0x0A];
        assert_eq!(count_coded_pictures(&data, "h264"), 0);
    }

    #[test]
    fn h264_idr_slice_is_one_picture() {
        let data = [0, 0, 0, 1, 0x65, 0x88, 0x80];
        assert_eq!(count_coded_pictures(&data, "h264"), 1);
    }

    #[test]
    fn h264_config_then_slice_counts_one() {
        let mut data = vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xCE];
        data.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);
        assert_eq!(count_coded_pictures(&data, "h264"), 1);
    }
}
