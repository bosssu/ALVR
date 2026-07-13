//! Live lossless mux of bitstream video + PCM audio into Matroska via ffmpeg.
//!
//! NALs/PCM are queued from realtime callbacks; helper threads write into OS pipes
//! that ffmpeg reads. `-use_wallclock_as_timestamps 1` keeps A/V on wall-clock time.

use alvr_common::{error, info};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread::{self, JoinHandle},
};

pub struct LiveMuxSession {
    video_tx: Option<flume::Sender<Vec<u8>>>,
    audio_tx: Option<flume::Sender<Vec<u8>>>,
    join: Option<JoinHandle<()>>,
    output_path: PathBuf,
}

impl LiveMuxSession {
    pub fn start(output_mkv: PathBuf, sample_rate: u32, codec_hint: &str) -> Result<Self, String> {
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

        let join = thread::spawn(move || {
            if let Err(e) =
                mux_worker(ffmpeg, output_path.clone(), sample_rate, &demux, video_rx, audio_rx)
            {
                error!("Live mux failed: {e}");
            } else {
                info!("Live MKV recording finished: {}", output_path.display());
            }
        });

        info!(
            "Live MKV recording started ({} Hz, {}): {}",
            sample_rate,
            demux_for_log,
            output_mkv.display()
        );

        Ok(Self {
            video_tx: Some(video_tx),
            audio_tx: Some(audio_tx),
            join: Some(join),
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
            .expect("live mux audio sender")
            .clone()
    }

    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    pub fn finish(mut self) {
        // Close channels so pipe writers see EOF, then wait for ffmpeg.
        self.video_tx = None;
        self.audio_tx = None;
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for LiveMuxSession {
    fn drop(&mut self) {
        self.video_tx = None;
        self.audio_tx = None;
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
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

fn mux_worker(
    ffmpeg: PathBuf,
    output_mkv: PathBuf,
    sample_rate: u32,
    demux: &str,
    video_rx: flume::Receiver<Vec<u8>>,
    audio_rx: flume::Receiver<Vec<u8>>,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        mux_worker_windows(ffmpeg, output_mkv, sample_rate, demux, video_rx, audio_rx)
    }
    #[cfg(not(windows))]
    {
        mux_worker_unix(ffmpeg, output_mkv, sample_rate, demux, video_rx, audio_rx)
    }
}

fn spawn_ffmpeg(
    ffmpeg: &Path,
    output_mkv: &Path,
    sample_rate: u32,
    demux: &str,
    video_input: &str,
    audio_input: &str,
) -> Result<Child, String> {
    Command::new(ffmpeg)
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-thread_queue_size")
        .arg("1024")
        .arg("-use_wallclock_as_timestamps")
        .arg("1")
        .arg("-f")
        .arg(demux)
        .arg("-i")
        .arg(video_input)
        .arg("-thread_queue_size")
        .arg("1024")
        .arg("-use_wallclock_as_timestamps")
        .arg("1")
        .arg("-f")
        .arg("s16le")
        .arg("-ar")
        .arg(sample_rate.to_string())
        .arg("-ac")
        .arg("2")
        .arg("-i")
        .arg(audio_input)
        .arg("-c:v")
        .arg("copy")
        .arg("-c:a")
        .arg("pcm_s16le")
        .arg(output_mkv.as_os_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn ffmpeg ({}): {e}", ffmpeg.display()))
}

fn finish_ffmpeg(mut child: Child) -> Result<(), String> {
    let mut err_text = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut err_text);
    }
    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => {
            let msg = err_text.trim();
            if msg.is_empty() {
                Err(format!("ffmpeg exit {status}"))
            } else {
                Err(format!("ffmpeg exit {status}: {msg}"))
            }
        }
        Err(e) => Err(format!("ffmpeg wait: {e}")),
    }
}

#[cfg(windows)]
fn mux_worker_windows(
    ffmpeg: PathBuf,
    output_mkv: PathBuf,
    sample_rate: u32,
    demux: &str,
    video_rx: flume::Receiver<Vec<u8>>,
    audio_rx: flume::Receiver<Vec<u8>>,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::WriteFile;
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    // winbase.h PIPE_ACCESS_OUTBOUND
    const PIPE_ACCESS_OUTBOUND: FILE_FLAGS_AND_ATTRIBUTES = FILE_FLAGS_AND_ATTRIBUTES(0x00000002);

    let id = std::process::id();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let vid_name = format!(r"\\.\pipe\alvr_rec_v_{id}_{stamp}");
    let aud_name = format!(r"\\.\pipe\alvr_rec_a_{id}_{stamp}");

    unsafe fn make_pipe(name: &str) -> Result<HANDLE, String> {
        let wide: Vec<u16> = std::ffi::OsStr::new(name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let h = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                PIPE_ACCESS_OUTBOUND,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                1 << 20,
                1 << 20,
                0,
                None,
            )
        };
        if h.is_invalid() {
            Err(format!(
                "CreateNamedPipeW({name}): {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(h)
        }
    }

    let vid_handle = unsafe { make_pipe(&vid_name)? };
    let aud_handle = unsafe { make_pipe(&aud_name)? };

    let child = spawn_ffmpeg(
        &ffmpeg,
        &output_mkv,
        sample_rate,
        demux,
        &vid_name,
        &aud_name,
    )?;

    let connect = |h: HANDLE, label: &str| -> Result<(), String> {
        let r = unsafe { ConnectNamedPipe(h, None) };
        if r.is_err() {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(535) {
                return Err(format!("ConnectNamedPipe({label}): {err}"));
            }
        }
        Ok(())
    };
    // HANDLE is not Send in windows-rs; opaque wrapper for pipe threads.
    struct SendHandle(HANDLE);
    unsafe impl Send for SendHandle {}

    // Connect sequentially. ffmpeg typically opens inputs in CLI order (video then audio).
    // If this blocks forever, check that ffmpeg is running and paths match.
    connect(vid_handle, "video")?;
    connect(aud_handle, "audio")?;

    let write_loop = |sh: SendHandle, rx: flume::Receiver<Vec<u8>>| {
        let handle = sh.0;
        while let Ok(chunk) = rx.recv() {
            let mut off = 0usize;
            while off < chunk.len() {
                let mut written = 0u32;
                let ok = unsafe { WriteFile(handle, Some(&chunk[off..]), Some(&mut written), None) };
                if ok.is_err() || written == 0 {
                    unsafe {
                        let _ = CloseHandle(handle);
                    }
                    return;
                }
                off += written as usize;
            }
        }
        unsafe {
            let _ = CloseHandle(handle);
        }
    };

    let vh = SendHandle(vid_handle);
    let ah = SendHandle(aud_handle);
    let v_thread = thread::spawn(move || write_loop(vh, video_rx));
    let a_thread = thread::spawn(move || write_loop(ah, audio_rx));
    let _ = v_thread.join();
    let _ = a_thread.join();

    finish_ffmpeg(child)
}

#[cfg(not(windows))]
fn mux_worker_unix(
    ffmpeg: PathBuf,
    output_mkv: PathBuf,
    sample_rate: u32,
    demux: &str,
    video_rx: flume::Receiver<Vec<u8>>,
    audio_rx: flume::Receiver<Vec<u8>>,
) -> Result<(), String> {
    let id = std::process::id();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = std::env::temp_dir();
    let vid_path = dir.join(format!("alvr_rec_v_{id}_{stamp}"));
    let aud_path = dir.join(format!("alvr_rec_a_{id}_{stamp}"));
    let _ = fs::remove_file(&vid_path);
    let _ = fs::remove_file(&aud_path);

    for p in [&vid_path, &aud_path] {
        let status = Command::new("mkfifo")
            .arg(p)
            .status()
            .map_err(|e| format!("mkfifo: {e}"))?;
        if !status.success() {
            return Err(format!("mkfifo failed for {}", p.display()));
        }
    }

    let child = spawn_ffmpeg(
        &ffmpeg,
        &output_mkv,
        sample_rate,
        demux,
        &vid_path.to_string_lossy(),
        &aud_path.to_string_lossy(),
    )?;

    let vp = vid_path.clone();
    let ap = aud_path.clone();
    let v_open = thread::spawn(move || fs::OpenOptions::new().write(true).open(vp));
    let a_open = thread::spawn(move || fs::OpenOptions::new().write(true).open(ap));

    let mut vid_w = v_open
        .join()
        .map_err(|_| "vid join".to_string())?
        .map_err(|e| format!("open vid fifo: {e}"))?;
    let mut aud_w = a_open
        .join()
        .map_err(|_| "aud join".to_string())?
        .map_err(|e| format!("open aud fifo: {e}"))?;

    let v_thread = thread::spawn(move || {
        while let Ok(chunk) = video_rx.recv() {
            if vid_w.write_all(&chunk).is_err() {
                break;
            }
        }
        let _ = vid_w.flush();
    });
    let a_thread = thread::spawn(move || {
        while let Ok(chunk) = audio_rx.recv() {
            if aud_w.write_all(&chunk).is_err() {
                break;
            }
        }
        let _ = aud_w.flush();
    });

    let _ = v_thread.join();
    let _ = a_thread.join();
    let res = finish_ffmpeg(child);
    let _ = fs::remove_file(&vid_path);
    let _ = fs::remove_file(&aud_path);
    res
}
