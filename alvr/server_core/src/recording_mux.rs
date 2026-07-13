//! Live capture of bitstream video + PCM audio, finalized into one Matroska via ffmpeg.
//!
//! Realtime path only appends elementary streams to temp files (cheap, no pipe deadlocks).
//! On stop, a background job remuxes with `-c:v copy` / PCM audio.
//!
//! Windows WASAPI loopback often delivers **no buffers while output is silent**. Without
//! compensation the PCM track would start at the first audible sample and stay shorter than
//! video. The audio writer therefore inserts s16le silence so the written sample timeline
//! tracks wall-clock from recording start (leading silence, mid gaps, and trailing pad).

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

pub struct LiveMuxSession {
    video_tx: Option<flume::Sender<Vec<u8>>>,
    audio_tx: Option<flume::Sender<Vec<u8>>>,
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

        let (video_tx, video_rx) = flume::unbounded::<Vec<u8>>();
        let (audio_tx, audio_rx) = flume::unbounded::<Vec<u8>>();
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
            let _ = tx.send(data.to_vec());
        }
    }

    pub fn push_audio(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        if let Some(tx) = &self.audio_tx {
            let _ = tx.send(data.to_vec());
        }
    }

    pub fn audio_sender(&self) -> flume::Sender<Vec<u8>> {
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
    if let Ok(out) = Command::new(if cfg!(windows) { "where" } else { "which" })
        .arg("ffmpeg")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            if let Some(line) = String::from_utf8_lossy(&out.stdout).lines().next() {
                let p = PathBuf::from(line.trim());
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }

    if let Some(layout) = crate::FILESYSTEM_LAYOUT.get() {
        let mut dir = layout.executables_dir.clone();
        for _ in 0..8 {
            for rel in [
                "deps/windows/ffmpeg/bin/ffmpeg.exe",
                "ffmpeg.exe",
                "bin/win64/ffmpeg.exe",
                "bin/ffmpeg",
            ] {
                let p = dir.join(rel);
                if p.is_file() {
                    return Some(p);
                }
            }
            if !dir.pop() {
                break;
            }
        }
    }

    [
        PathBuf::from("deps/windows/ffmpeg/bin/ffmpeg.exe"),
        PathBuf::from("ffmpeg.exe"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// Stereo s16le frame size.
const PCM_FRAME_BYTES: u64 = 4;

fn wall_pcm_bytes(started: Instant, sample_rate: u32) -> u64 {
    let frames = (started.elapsed().as_secs_f64() * f64::from(sample_rate)).round() as u64;
    frames * PCM_FRAME_BYTES
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
    let target = target_bytes - (target_bytes % PCM_FRAME_BYTES);
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
    video_rx: flume::Receiver<Vec<u8>>,
    audio_rx: flume::Receiver<Vec<u8>>,
    _stop_flag: Arc<AtomicBool>,
) -> Result<(), String> {
    let started = Instant::now();

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

    let vb = Arc::clone(&video_bytes);
    let vp = Arc::clone(&video_packets);
    let v_thread = thread::spawn(move || -> Result<(), String> {
        while let Ok(chunk) = video_rx.recv() {
            video_file
                .write_all(&chunk)
                .map_err(|e| format!("write video temp: {e}"))?;
            vb.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            vp.fetch_add(1, Ordering::Relaxed);
        }
        video_file
            .flush()
            .map_err(|e| format!("flush video temp: {e}"))?;
        Ok(())
    });

    let ab = Arc::clone(&audio_bytes);
    let asil = Arc::clone(&audio_silence);
    let a_thread = thread::spawn(move || -> Result<(), String> {
        let mut written = 0u64;
        let mut silence_padded = 0u64;

        // Drain real samples; before each chunk, fill any wall-clock gap with silence
        // (covers silent start + mid-session loopback gaps).
        loop {
            match audio_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        continue;
                    }
                    pad_audio_to(
                        &mut audio_file,
                        &mut written,
                        &mut silence_padded,
                        wall_pcm_bytes(started, sample_rate),
                    )?;
                    audio_file
                        .write_all(&chunk)
                        .map_err(|e| format!("write audio temp: {e}"))?;
                    written += chunk.len() as u64;
                }
                Err(flume::RecvTimeoutError::Timeout) => {
                    // Keep the PCM timeline advancing during long silence so stop-time
                    // padding is small and the file stays aligned if inspected mid-record.
                    pad_audio_to(
                        &mut audio_file,
                        &mut written,
                        &mut silence_padded,
                        wall_pcm_bytes(started, sample_rate),
                    )?;
                }
                Err(flume::RecvTimeoutError::Disconnected) => break,
            }
        }

        // Trailing silence through stop (channel closed).
        pad_audio_to(
            &mut audio_file,
            &mut written,
            &mut silence_padded,
            wall_pcm_bytes(started, sample_rate),
        )?;

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
    let a_bytes = audio_bytes.load(Ordering::Relaxed);
    let a_silence = audio_silence.load(Ordering::Relaxed);
    let packets = video_packets.load(Ordering::Relaxed);

    let audio_secs = a_bytes as f64 / (f64::from(sample_rate) * PCM_FRAME_BYTES as f64);
    let silence_secs = a_silence as f64 / (f64::from(sample_rate) * PCM_FRAME_BYTES as f64);

    info!(
        "Recording capture done: {:.2}s wall, {} video packets / {} bytes, audio {:.2}s ({} bytes, {:.2}s silence padded)",
        wall.as_secs_f64(),
        packets,
        v_bytes,
        audio_secs,
        a_bytes,
        silence_secs
    );

    if v_bytes == 0 && a_bytes == 0 {
        let _ = fs::remove_file(&video_tmp);
        let _ = fs::remove_file(&audio_tmp);
        return Err("no video or audio data captured".into());
    }

    // Prefer matching video timeline to the (now wall-aligned) audio length when present.
    let timeline = if a_bytes > 0 {
        Duration::from_secs_f64(audio_secs.max(0.05))
    } else {
        wall
    };
    let fps = compute_fps(packets, timeline, fallback_fps);
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
    );

    let _ = fs::remove_file(&video_tmp);
    let _ = fs::remove_file(&audio_tmp);

    result
}

fn compute_fps(video_packets: u64, wall: Duration, fallback_fps: f32) -> f32 {
    let wall_s = wall.as_secs_f64();
    let fallback = if fallback_fps.is_finite() && fallback_fps >= 1.0 {
        fallback_fps
    } else {
        72.0
    };

    // Prefer packet count over wall clock so container duration ≈ session length.
    // Subtract a little for the SPS/PPS config packet when present.
    let frames = if video_packets > 1 {
        video_packets as f64
    } else {
        video_packets as f64
    };

    if frames >= 2.0 && wall_s >= 0.05 {
        let fps = frames / wall_s;
        // Sanity clamp — ALVR is typically 60–120 Hz, allow some margin.
        if (15.0..=240.0).contains(&fps) {
            return fps as f32;
        }
    }
    fallback
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

    // Keep both full streams; do not cut to the shorter one.
    cmd.arg(output_mkv.as_os_str())
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
        "Remuxing with ffmpeg ({demux}, {fps:.2} fps, audio={has_audio}): {}",
        output_mkv.display()
    );

    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn ffmpeg ({}): {e}", ffmpeg.display()))?;
    finish_ffmpeg(child)
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
