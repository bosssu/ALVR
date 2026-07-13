# Capture Hotkeys, SBS Screenshot & Recording Audio Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add PC global hotkeys for SBS PNG screenshot and toggle recording, play feedback sounds, and tee game-audio PCM into a paired WAV while recording video.

**Architecture:** Extend `extra.capture` settings; implement path helpers, hotkey thread, sound player, and WAV writer in `server_core`; tee from existing `alvr_audio` loopback; implement Windows (and Linux) encoder-side RGB SBS → PNG. Dual files under configurable `Captures/Records` and `Captures/Captures`.

**Tech Stack:** Rust (server_core, session, audio), rodio/cpal, Windows `RegisterHotKey` + WIC PNG, existing C++ FrameRender/CEncoder pipeline.

**Spec:** `docs/superpowers/specs/2026-07-13-capture-hotkeys-audio-design.md`

---

## File structure

| Path | Responsibility |
|------|----------------|
| `alvr/session/src/settings.rs` | CaptureConfig fields + defaults |
| `alvr/server_core/src/capture_paths.rs` | Resolve/create recording & screenshot dirs |
| `alvr/server_core/src/hotkeys.rs` | Parse hotkey strings; Windows hotkey thread |
| `alvr/server_core/src/audio_recording.rs` | WAV writer + Armed/Recording state |
| `alvr/server_core/src/feedback_sounds.rs` | Play embedded click/start/stop sounds |
| `alvr/server_core/src/lib.rs` | Wire recording paths, audio arm, first-frame open, hotkeys lifecycle |
| `alvr/server_core/src/web_server.rs` | Start/stop use shared helpers (audio finalize) |
| `alvr/server_core/src/connection.rs` | Startup recording + disconnect stop; settings hash |
| `alvr/audio/src/lib.rs` | Optional tee callback in `record_audio_blocking` |
| `alvr/server_openvr/src/lib.rs` | Pass resolved `screenshot_dir` into C++ settings |
| `alvr/server_openvr/cpp/platform/win32/CEncoder.*` | CaptureFrame flag + PNG save |
| `alvr/server_openvr/cpp/platform/win32/FrameRender.*` | Expose RGB screenshot texture |
| `alvr/server_openvr/cpp/platform/linux/CEncoder.cpp` | Timestamped PNG paths |
| `alvr/server_core/resources/` (or similar) | Optional tiny WAV assets; can generate programmatically if no assets |

---

### Task 1: Extend CaptureConfig settings

**Files:**
- Modify: `alvr/session/src/settings.rs` (`CaptureConfig` ~1562–1571, defaults ~2240–2250)
- Modify: `alvr/server_core/src/connection.rs` (~246) if settings hash lists `capture_frame_dir`
- Modify: `alvr/server_openvr/src/lib.rs` (~60–68, ~184) after Task 1 uses new field name (can leave compile fix for Task 10/11)

- [ ] **Step 1: Replace `CaptureConfig` fields**

Replace the struct body so it becomes:

```rust
#[derive(SettingsSchema, Serialize, Deserialize, Clone)]
pub struct CaptureConfig {
    #[schema(strings(display_name = "Start video recording at client connection"))]
    pub startup_video_recording: bool,

    pub rolling_video_files: Switch<RollingVideoFilesConfig>,

    #[schema(strings(display_name = "Enable global hotkeys"))]
    pub hotkeys_enabled: bool,

    #[schema(strings(
        display_name = "Screenshot hotkey",
        help = "Examples: F8, Ctrl+F8, PrintScreen. Case-insensitive; modifiers: Ctrl, Alt, Shift, Win."
    ))]
    pub screenshot_hotkey: String,

    #[schema(strings(
        display_name = "Recording hotkey",
        help = "Same key toggles start/stop. Examples: F9, Ctrl+F9."
    ))]
    pub recording_hotkey: String,

    #[schema(strings(display_name = "Feedback sounds"))]
    pub feedback_sounds_enabled: bool,

    #[schema(strings(
        display_name = "Recording folder",
        help = "Relative to ALVR install root, or absolute. Video + WAV written here."
    ))]
    pub recording_dir: String,

    #[schema(strings(
        display_name = "Screenshot folder",
        help = "Relative to ALVR install root, or absolute. SBS PNG written here."
    ))]
    #[schema(flag = "steamvr-restart")]
    pub screenshot_dir: String,
}
```

Remove `capture_frame_dir`.

- [ ] **Step 2: Update defaults in `SettingsDefault` / `CaptureConfigDefault`**

```rust
capture: CaptureConfigDefault {
    startup_video_recording: false,
    rolling_video_files: SwitchDefault {
        enabled: false,
        content: RollingVideoFilesConfigDefault { duration_s: 5 },
    },
    hotkeys_enabled: true,
    screenshot_hotkey: "F8".into(),
    recording_hotkey: "F9".into(),
    feedback_sounds_enabled: true,
    recording_dir: "Captures/Records".into(),
    screenshot_dir: "Captures/Captures".into(),
},
```

- [ ] **Step 3: Fix compile breakages that still reference `capture_frame_dir`**

In `connection.rs` settings hash (~246): use `screenshot_dir` and also hash `recording_dir` / hotkey fields if that hash is meant to detect capture setting changes.

In `server_openvr/src/lib.rs`: temporarily keep reading `settings.extra.capture.screenshot_dir` into `m_captureFrameDir` (C++ field name can stay).

```rust
let cstr = CString::new(settings.extra.capture.screenshot_dir.as_str()).unwrap_or_default();
```

- [ ] **Step 4: Build session crate**

Run: `cargo check -p alvr_session -p alvr_server_core -p alvr_server_openvr`

Expected: compiles (or only unrelated errors).

- [ ] **Step 5: Commit**

```bash
git add alvr/session/src/settings.rs alvr/server_core/src/connection.rs alvr/server_openvr/src/lib.rs
git commit -m "feat(session): extend CaptureConfig for hotkeys and save paths"
```

---

### Task 2: Capture path helpers

**Files:**
- Create: `alvr/server_core/src/capture_paths.rs`
- Modify: `alvr/server_core/src/lib.rs` (add `mod capture_paths;`)

- [ ] **Step 1: Add unit tests for path resolution (fail until implemented)**

```rust
// alvr/server_core/src/capture_paths.rs
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn absolute_path_unchanged() {
        let root = PathBuf::from("C:/ALVR");
        #[cfg(windows)]
        {
            let p = resolve_capture_path(&root, "D:/out/recs");
            assert_eq!(p, PathBuf::from("D:/out/recs"));
        }
        #[cfg(not(windows))]
        {
            let p = resolve_capture_path(&root, "/tmp/recs");
            assert_eq!(p, PathBuf::from("/tmp/recs"));
        }
    }

    #[test]
    fn relative_joins_root() {
        let root = PathBuf::from("/opt/alvr");
        let p = resolve_capture_path(&root, "Captures/Records");
        assert_eq!(p, PathBuf::from("/opt/alvr/Captures/Records"));
    }
}
```

- [ ] **Step 2: Run tests — expect fail**

Run: `cargo test -p alvr_server_core capture_paths -- --nocapture`

Expected: compile fail / test module missing symbols.

- [ ] **Step 3: Implement helpers**

```rust
use alvr_common::{error, warn};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Resolve a configured capture path against the ALVR install root.
pub fn resolve_capture_path(program_root: &Path, configured: &str) -> PathBuf {
    let path = PathBuf::from(configured);
    if path.is_absolute() {
        path
    } else {
        program_root.join(path)
    }
}

pub fn ensure_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)
}

pub fn program_root() -> PathBuf {
    crate::FILESYSTEM_LAYOUT
        .get()
        .map(|l| l.executables_dir.clone())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn recording_stem(now: chrono::DateTime<chrono::Local>) -> String {
    format!("recording.{}", now.format("%F.%H-%M-%S"))
}

pub fn screenshot_filename(now: chrono::DateTime<chrono::Local>) -> String {
    format!("screenshot.{}.png", now.format("%F.%H-%M-%S"))
}
```

Make `FILESYSTEM_LAYOUT` accessible as `pub(crate)` in `lib.rs` if needed, or keep helper only using passed-in root from callers.

- [ ] **Step 4: Run tests — expect pass**

Run: `cargo test -p alvr_server_core capture_paths`

- [ ] **Step 5: Commit**

```bash
git add alvr/server_core/src/capture_paths.rs alvr/server_core/src/lib.rs
git commit -m "feat(server_core): add capture path resolution helpers"
```

---

### Task 3: Hotkey string parser

**Files:**
- Create: `alvr/server_core/src/hotkeys.rs`
- Modify: `alvr/server_core/src/lib.rs` (`mod hotkeys;`)

- [ ] **Step 1: Write parser tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_f8() {
        let k = parse_hotkey("F8").unwrap();
        assert!(k.modifiers.is_empty());
        assert_eq!(k.vk_name, "F8");
    }

    #[test]
    fn parse_ctrl_f8() {
        let k = parse_hotkey("Ctrl+F8").unwrap();
        assert!(k.modifiers.iter().any(|m| matches!(m, Modifier::Ctrl)));
        assert_eq!(k.vk_name, "F8");
    }

    #[test]
    fn parse_invalid() {
        assert!(parse_hotkey("").is_err());
        assert!(parse_hotkey("Ctrl+").is_err());
    }
}
```

- [ ] **Step 2: Run tests — expect fail**

Run: `cargo test -p alvr_server_core hotkeys::tests -- --nocapture`

- [ ] **Step 3: Implement parser (platform-agnostic)**

```rust
use alvr_common::anyhow::{self, bail, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Win,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeySpec {
    pub modifiers: Vec<Modifier>,
    /// Canonical key name e.g. "F8", "A", "PRINTSCREEN"
    pub vk_name: String,
}

pub fn parse_hotkey(s: &str) -> Result<HotkeySpec> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty hotkey");
    }
    let parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        bail!("empty hotkey");
    }
    let mut modifiers = Vec::new();
    for part in &parts[..parts.len() - 1] {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers.push(Modifier::Ctrl),
            "alt" => modifiers.push(Modifier::Alt),
            "shift" => modifiers.push(Modifier::Shift),
            "win" | "super" | "meta" => modifiers.push(Modifier::Win),
            other => bail!("unknown modifier: {other}"),
        }
    }
    let key = parts[parts.len() - 1];
    let vk_name = normalize_key_name(key)?;
    Ok(HotkeySpec { modifiers, vk_name })
}

fn normalize_key_name(key: &str) -> Result<String> {
    let u = key.to_ascii_uppercase();
    // Accept F1-F12, A-Z, 0-9, PRINTSCREEN, PAUSE, etc.
    if (u.len() == 1 && u.chars().next().unwrap().is_ascii_alphanumeric())
        || (u.starts_with('F') && u[1..].parse::<u8>().ok().is_some_and(|n| (1..=12).contains(&n)))
        || matches!(u.as_str(), "PRINTSCREEN" | "PAUSE" | "SCROLLLOCK" | "INSERT" | "DELETE"
            | "HOME" | "END" | "PRIOR" | "NEXT" | "LEFT" | "RIGHT" | "UP" | "DOWN" | "SPACE")
    {
        // Canonical PrintScreen
        if u == "PRINTSCREEN" || u == "PRTSC" {
            return Ok("PRINTSCREEN".into());
        }
        return Ok(u);
    }
    bail!("unsupported key: {key}")
}
```

Leave Windows virtual-key mapping + thread for Task 8; this task only parser + tests.

- [ ] **Step 4: Run tests — expect pass**

- [ ] **Step 5: Commit**

```bash
git add alvr/server_core/src/hotkeys.rs alvr/server_core/src/lib.rs
git commit -m "feat(server_core): add hotkey string parser"
```

---

### Task 4: WAV `AudioRecordingWriter`

**Files:**
- Create: `alvr/server_core/src/audio_recording.rs`
- Modify: `alvr/server_core/src/lib.rs`

- [ ] **Step 1: Write tests for WAV header + state machine**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn arm_then_first_video_opens_wav() {
        let dir = tempfile::tempdir().unwrap();
        let stem = dir.path().join("recording.test");
        let mut w = AudioRecordingWriter::new();
        w.arm(stem.clone(), 48000, 2);
        assert!(matches!(w.state(), AudioRecState::Armed));
        w.on_video_bytes_written().unwrap();
        assert!(matches!(w.state(), AudioRecState::Recording));
        w.write_pcm(&[0i16, 0, 100, -100]).unwrap();
        w.finalize().unwrap();
        let mut f = std::fs::File::open(stem.with_extension("wav")).unwrap();
        let mut hdr = [0u8; 12];
        f.read_exact(&mut hdr).unwrap();
        assert_eq!(&hdr[0..4], b"RIFF");
        assert_eq!(&hdr[8..12], b"WAVE");
    }

    #[test]
    fn finalize_armed_without_open_is_ok() {
        let mut w = AudioRecordingWriter::new();
        w.arm(std::env::temp_dir().join("nope_recording"), 44100, 2);
        w.finalize().unwrap();
        assert!(matches!(w.state(), AudioRecState::Idle));
    }
}
```

If `tempfile` is not a workspace dep, use `std::env::temp_dir()` + unique name and `std::fs::remove_file` in test, or add `tempfile` as dev-dependency of `alvr_server_core`.

- [ ] **Step 2: Run tests — expect fail**

- [ ] **Step 3: Implement writer**

```rust
use alvr_common::{error, anyhow::{Context, Result}};
use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRecState {
    Idle,
    Armed,
    Recording,
}

pub struct AudioRecordingWriter {
    state: AudioRecState,
    stem: Option<PathBuf>,
    sample_rate: u32,
    channels: u16,
    file: Option<File>,
    data_bytes: u32,
}

impl AudioRecordingWriter {
    pub fn new() -> Self {
        Self {
            state: AudioRecState::Idle,
            stem: None,
            sample_rate: 48000,
            channels: 2,
            file: None,
            data_bytes: 0,
        }
    }

    pub fn state(&self) -> AudioRecState {
        self.state
    }

    /// Prepare to record; WAV file is created only on first video write.
    pub fn arm(&mut self, stem: PathBuf, sample_rate: u32, channels: u16) {
        let _ = self.finalize();
        self.stem = Some(stem);
        self.sample_rate = sample_rate;
        self.channels = channels;
        self.state = AudioRecState::Armed;
        self.data_bytes = 0;
    }

    pub fn on_video_bytes_written(&mut self) -> Result<()> {
        if self.state != AudioRecState::Armed {
            return Ok(());
        }
        let path = self
            .stem
            .as_ref()
            .map(|s| s.with_extension("wav"))
            .context("no stem")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = File::create(&path)?;
        write_wav_header_placeholder(&mut file, self.sample_rate, self.channels)?;
        self.file = Some(file);
        self.state = AudioRecState::Recording;
        Ok(())
    }

    pub fn write_pcm_i16(&mut self, samples: &[i16]) -> Result<()> {
        if self.state != AudioRecState::Recording {
            return Ok(());
        }
        if let Some(file) = self.file.as_mut() {
            for s in samples {
                file.write_all(&s.to_le_bytes())?;
            }
            self.data_bytes = self.data_bytes.saturating_add((samples.len() * 2) as u32);
        }
        Ok(())
    }

    pub fn write_pcm_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        if self.state != AudioRecState::Recording {
            return Ok(());
        }
        if let Some(file) = self.file.as_mut() {
            file.write_all(bytes)?;
            self.data_bytes = self.data_bytes.saturating_add(bytes.len() as u32);
        }
        Ok(())
    }

    pub fn finalize(&mut self) -> Result<()> {
        if let Some(mut file) = self.file.take() {
            patch_wav_sizes(&mut file, self.data_bytes)?;
        }
        self.stem = None;
        self.state = AudioRecState::Idle;
        self.data_bytes = 0;
        Ok(())
    }
}

fn write_wav_header_placeholder(file: &mut File, sample_rate: u32, channels: u16) -> Result<()> {
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align = channels * bits_per_sample / 8;
    // RIFF size and data size filled in finalize
    file.write_all(b"RIFF")?;
    file.write_all(&0u32.to_le_bytes())?; // placeholder
    file.write_all(b"WAVE")?;
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?; // PCM
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&bits_per_sample.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&0u32.to_le_bytes())?; // placeholder
    Ok(())
}

fn patch_wav_sizes(file: &mut File, data_bytes: u32) -> Result<()> {
    let riff_size = 36u32 + data_bytes;
    file.seek(SeekFrom::Start(4))?;
    file.write_all(&riff_size.to_le_bytes())?;
    file.seek(SeekFrom::Start(40))?;
    file.write_all(&data_bytes.to_le_bytes())?;
    Ok(())
}
```

- [ ] **Step 4: Run tests — expect pass**

- [ ] **Step 5: Commit**

```bash
git add alvr/server_core/src/audio_recording.rs alvr/server_core/src/lib.rs alvr/server_core/Cargo.toml
git commit -m "feat(server_core): add WAV audio recording writer"
```

---

### Task 5: Wire recording start/stop + first-frame audio open

**Files:**
- Modify: `alvr/server_core/src/lib.rs` (`ConnectionContext`, `create_recording_file`, `send_video_nal`, `set_video_config_nals`)
- Modify: `alvr/server_core/src/web_server.rs` (`stop_recording`)
- Modify: `alvr/server_core/src/connection.rs` (startup recording, disconnect cleanup)

- [ ] **Step 1: Extend `ConnectionContext`**

```rust
// in ConnectionContext
audio_recording: Mutex<audio_recording::AudioRecordingWriter>,
// optional: last known game audio sample rate for arm()
game_audio_sample_rate: Mutex<u32>,
```

Initialize with `AudioRecordingWriter::new()` and default sample rate `48000`.

- [ ] **Step 2: Change `create_recording_file` to use `recording_dir` and arm audio**

```rust
pub fn create_recording_file(connection_context: &ConnectionContext, settings: &Settings) {
    let codec = settings.video.preferred_codec;
    let ext = match codec {
        CodecType::H264 => "h264",
        CodecType::Hevc => "h265",
        CodecType::AV1 => "av1",
    };

    let root = capture_paths::program_root();
    let dir = capture_paths::resolve_capture_path(&root, &settings.extra.capture.recording_dir);
    if let Err(e) = capture_paths::ensure_dir(&dir) {
        error!("Failed to create recording dir: {e}");
        return;
    }

    let stem_name = capture_paths::recording_stem(chrono::Local::now());
    let stem = dir.join(&stem_name);
    let video_path = stem.with_extension(ext);

    match File::create(&video_path) {
        Ok(mut file) => {
            if let Some(config) = &*connection_context.decoder_config.lock() {
                file.write_all(&config.config_buffer).ok();
            }
            *connection_context.video_recording_file.lock() = Some(file);

            let sample_rate = *connection_context.game_audio_sample_rate.lock();
            connection_context.audio_recording.lock().arm(stem, sample_rate, 2);

            // If config already written above, treat as first video bytes
            if connection_context.decoder_config.lock().is_some() {
                if let Err(e) = connection_context.audio_recording.lock().on_video_bytes_written() {
                    error!("Failed to open recording WAV: {e}");
                }
            }

            connection_context
                .events_sender
                .send(ServerCoreEvent::RequestIDR)
                .ok();
        }
        Err(e) => error!("Failed to record video on disk: {e}"),
    }
}

pub fn stop_recording(connection_context: &ConnectionContext) {
    *connection_context.video_recording_file.lock() = None;
    if let Err(e) = connection_context.audio_recording.lock().finalize() {
        error!("Failed to finalize recording WAV: {e}");
    }
}
```

- [ ] **Step 3: On every successful video file write, call `on_video_bytes_written`**

In `set_video_config_nals` and `send_video_nal` where `file.write_all(...)` succeeds:

```rust
if let Some(file) = &mut *self.connection_context.video_recording_file.lock() {
    if file.write_all(&nal_buffer).is_ok() {
        let _ = self
            .connection_context
            .audio_recording
            .lock()
            .on_video_bytes_written();
    }
}
```

Note: `on_video_bytes_written` is cheap after first open (state != Armed).

- [ ] **Step 4: Replace all `*video_recording_file.lock() = None` stop sites with `stop_recording`**

Including `web_server.rs` `stop_recording` handler and `connection.rs` disconnect path.

- [ ] **Step 5: Set `game_audio_sample_rate` when known**

In `connection.rs` where `game_audio_sample_rate` is computed (~742–765), store into `ctx.game_audio_sample_rate`.

- [ ] **Step 6: `cargo check -p alvr_server_core`**

- [ ] **Step 7: Commit**

```bash
git add alvr/server_core/src/lib.rs alvr/server_core/src/web_server.rs alvr/server_core/src/connection.rs
git commit -m "feat(server_core): record paired WAV armed after first video write"
```

---

### Task 6: Tee game audio into the writer

**Files:**
- Modify: `alvr/audio/src/lib.rs` (`record_audio_blocking`)
- Modify: `alvr/server_core/src/connection.rs` (game audio thread)

- [ ] **Step 1: Add optional tee callback to `record_audio_blocking`**

Change signature to accept an optional callback invoked with interleaved s16le bytes **after** downmix (same buffer sent on the network):

```rust
pub fn record_audio_blocking(
    is_running: Arc<dyn Fn() -> bool + Send + Sync>,
    mut sender: StreamSender<()>,
    device: &Device,
    channels_count: u16,
    mute: bool,
    mut tee: Option<Box<dyn FnMut(&[u8]) + Send>>,
) -> Result<()> {
    // ... existing stream setup ...
    // inside data callback after downmix_audio:
    let data = downmix_audio(data, config.channels(), channels_count);
    if let Some(tee) = tee.as_mut() {
        tee(&data);
    }
    if is_running() {
        sender.send_header_with_payload(&(), &data).ok();
    } else {
        *state.lock() = AudioRecordState::ShouldStop;
    }
    // ...
}
```

Update **all call sites** (`connection.rs` server, and any client if shared — grep `record_audio_blocking`). Client call sites pass `None`.

- [ ] **Step 2: Server game audio thread tee**

```rust
let audio_rec = Arc::clone(&ctx); // need Arc to audio writer
// inside spawn, when calling record_audio_blocking:
let tee = {
    let ctx = Arc::clone(&ctx);
    Some(Box::new(move |bytes: &[u8]| {
        let _ = ctx.audio_recording.lock().write_pcm_bytes(bytes);
    }) as Box<dyn FnMut(&[u8]) + Send>)
};
alvr_audio::record_audio_blocking(..., tee)?;
```

Ensure `ConnectionContext` is already `Arc` in that thread (it is via `ctx`).

- [ ] **Step 3: `cargo check -p alvr_audio -p alvr_server_core -p alvr_client_core`**

Fix every `record_audio_blocking` arity mismatch.

- [ ] **Step 4: Commit**

```bash
git add alvr/audio/src/lib.rs alvr/server_core/src/connection.rs alvr/client_core/src/connection.rs
git commit -m "feat(audio): tee game audio PCM into recording WAV"
```

---

### Task 7: Feedback sounds

**Files:**
- Create: `alvr/server_core/src/feedback_sounds.rs`
- Modify: `alvr/server_core/src/lib.rs`
- Modify: `alvr/server_core/Cargo.toml` if extra deps needed (rodio already via alvr_audio — use `alvr_audio` or add `rodio` directly)

- [ ] **Step 1: Implement generator-based short tones (no asset pipeline required)**

```rust
use alvr_common::{error, info};
use std::sync::mpsc;
use std::thread;

pub enum FeedbackKind {
    Screenshot,
    RecStart,
    RecStop,
}

pub struct FeedbackSounds {
    tx: Option<mpsc::Sender<FeedbackKind>>,
}

impl FeedbackSounds {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel::<FeedbackKind>();
        thread::spawn(move || {
            // Open default output once; on failure log and drain
            let stream = match rodio::OutputStreamBuilder::open_default_stream() {
                Ok(s) => s,
                Err(e) => {
                    error!("Feedback sounds disabled: {e}");
                    while rx.recv().is_ok() {}
                    return;
                }
            };
            let sink = rodio::Sink::connect_new(stream.mixer());
            while let Ok(kind) = rx.recv() {
                let (freq, ms) = match kind {
                    FeedbackKind::Screenshot => (1200.0, 60),
                    FeedbackKind::RecStart => (880.0, 120),
                    FeedbackKind::RecStop => (440.0, 120),
                };
                // Use rodio::source::SineWave + TakeDuration
                use rodio::Source;
                let src = rodio::source::SineWave::new(freq)
                    .take_duration(std::time::Duration::from_millis(ms))
                    .amplify(0.2);
                sink.append(src);
            }
        });
        Self { tx: Some(tx) }
    }

    pub fn play(&self, kind: FeedbackKind, enabled: bool) {
        if !enabled {
            return;
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(kind);
        }
    }
}

impl Drop for FeedbackSounds {
    fn drop(&mut self) {
        self.tx.take(); // close channel, end thread
    }
}
```

Add `rodio` dependency to `alvr_server_core/Cargo.toml` (same version as audio crate: `0.21`).

- [ ] **Step 2: Store `FeedbackSounds` on `ServerCoreContext` or `ConnectionContext`**

Prefer `ConnectionContext` or a process-level `OnceLock` so hotkeys and web handlers can play sounds.

- [ ] **Step 3: Play sounds from capture-frame and start/stop recording helpers**

```rust
// start recording success:
feedback.play(FeedbackKind::RecStart, settings.extra.capture.feedback_sounds_enabled);
// stop:
feedback.play(FeedbackKind::RecStop, ...);
// capture frame API:
feedback.play(FeedbackKind::Screenshot, ...);
```

- [ ] **Step 4: `cargo check -p alvr_server_core`**

- [ ] **Step 5: Commit**

```bash
git add alvr/server_core/src/feedback_sounds.rs alvr/server_core/src/lib.rs alvr/server_core/Cargo.toml alvr/server_core/src/web_server.rs
git commit -m "feat(server_core): play feedback tones for capture and recording"
```

---

### Task 8: Windows global hotkey thread

**Files:**
- Modify: `alvr/server_core/src/hotkeys.rs`
- Modify: `alvr/server_core/Cargo.toml` (windows features)
- Modify: `alvr/server_core/src/lib.rs` (start/stop thread)

- [ ] **Step 1: Add Windows deps**

```toml
[target.'cfg(windows)'.dependencies]
windows = { version = "0.62", features = [
    "Win32_Foundation",
    "Win32_UI_WindowsAndMessaging",
    "Win32_UI_Input_KeyboardAndMouse",
] }
```

- [ ] **Step 2: Map `HotkeySpec` → modifiers + virtual key**

```rust
#[cfg(windows)]
fn to_win_hotkey(spec: &HotkeySpec) -> Option<(u32, u32)> {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let mut mods = 0u32;
    for m in &spec.modifiers {
        mods |= match m {
            Modifier::Ctrl => MOD_CONTROL.0 as u32,
            Modifier::Alt => MOD_ALT.0 as u32,
            Modifier::Shift => MOD_SHIFT.0 as u32,
            Modifier::Win => MOD_WIN.0 as u32,
        };
    }
    let vk = match spec.vk_name.as_str() {
        "F8" => VK_F8.0 as u32,
        "F9" => VK_F9.0 as u32,
        // ... F1-F12, A-Z via VK codes, PRINTSCREEN => VK_SNAPSHOT
        _ => return None,
    };
    Some((mods, vk))
}
```

Implement full F1–F12 and alphanumerics as needed for config flexibility.

- [ ] **Step 3: Hotkey thread API**

```rust
pub enum HotkeyAction {
    Screenshot,
    ToggleRecording,
}

pub struct HotkeyThread {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl HotkeyThread {
    pub fn start(
        get_settings: Arc<dyn Fn() -> alvr_session::Settings + Send + Sync>,
        on_action: Arc<dyn Fn(HotkeyAction) + Send + Sync>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let join = thread::spawn(move || {
            #[cfg(windows)]
            windows_hotkey_loop(stop2, get_settings, on_action);
            #[cfg(not(windows))]
            {
                // No global hotkeys yet: sleep until stop
                while !stop2.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(200));
                }
            }
        });
        Self { stop, join: Some(join) }
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Windows: PostThreadMessage WM_QUIT to unblock GetMessage — store thread id
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
```

Windows loop outline:

1. Create message-only window OR use `GetMessage` on this thread after `RegisterHotKey(NULL, id, mods, vk)` (NULL hwnd delivers to thread message queue when using specific pattern — prefer hidden message window for reliability).
2. Register id=1 screenshot, id=2 recording from parsed settings.
3. On `WM_HOTKEY`, debounce 300ms, call `on_action`.
4. Periodically (or on custom message) re-read settings and re-register if hotkey strings changed.
5. On stop: `UnregisterHotKey`, destroy window, exit.

- [ ] **Step 4: Wire into `ServerCoreContext::new` and shutdown**

```rust
// on_action closure:
HotkeyAction::Screenshot => {
    let settings = SESSION_MANAGER.read().settings().clone();
    if settings.extra.capture.hotkeys_enabled {
        feedback.play(Screenshot, settings.extra.capture.feedback_sounds_enabled);
        events_sender.send(ServerCoreEvent::CaptureFrame).ok();
    }
}
HotkeyAction::ToggleRecording => {
    let settings = SESSION_MANAGER.read().settings().clone();
    if !settings.extra.capture.hotkeys_enabled { return; }
    if connection_context.video_recording_file.lock().is_some() {
        stop_recording(&connection_context);
        feedback.play(RecStop, ...);
    } else {
        create_recording_file(&connection_context, &settings);
        feedback.play(RecStart, ...);
    }
}
```

Only register when `hotkeys_enabled`; if both hotkeys parse equal, log warning and register neither (or only screenshot — spec: refuse dual bind).

- [ ] **Step 5: `cargo check -p alvr_server_core`**

- [ ] **Step 6: Commit**

```bash
git add alvr/server_core/src/hotkeys.rs alvr/server_core/src/lib.rs alvr/server_core/Cargo.toml
git commit -m "feat(server_core): Windows global hotkeys for capture and recording"
```

---

### Task 9: Windows SBS PNG capture in CEncoder / FrameRender

**Files:**
- Modify: `alvr/server_openvr/cpp/platform/win32/FrameRender.h`
- Modify: `alvr/server_openvr/cpp/platform/win32/FrameRender.cpp`
- Modify: `alvr/server_openvr/cpp/platform/win32/CEncoder.h`
- Modify: `alvr/server_openvr/cpp/platform/win32/CEncoder.cpp`
- Possibly: `shared/d3drender` helpers for staging readback

Note: `CEncoder.h` already includes `<wincodec.h>` — use WIC for PNG.

- [ ] **Step 1: Keep RGB screenshot source texture in `FrameRender`**

During `Startup` pipeline construction, after color correction assignment and **before** FFR/YUV overwrites `m_pStagingTexture`, save:

```cpp
m_pScreenshotTexture = m_pStagingTexture; // RGB SBS
```

If FFR runs, screenshot texture remains pre-FFR RGB (per spec).

Add:

```cpp
ComPtr<ID3D11Texture2D> GetScreenshotTexture();
```

- [ ] **Step 2: Implement `CEncoder::CaptureFrame`**

```cpp
void CEncoder::CaptureFrame() { m_captureFrame = true; }
```

Add `std::atomic_bool m_captureFrame{false};`

- [ ] **Step 3: In `CEncoder::Run` after `RenderFrame` / when texture ready**

When `m_captureFrame.exchange(false)` is true:

1. Get `GetScreenshotTexture()` (RGB).
2. Create CPU staging texture if needed (same size, `D3D11_USAGE_STAGING`, `CPU_ACCESS_READ`).
3. `CopyResource` GPU→staging.
4. `Map` read RGBA/BGRA pixels.
5. Build timestamped path: `std::string(Settings_Instance()->m_captureFrameDir) + "/screenshot." + timestamp + ".png"`.
6. Ensure directory exists (`CreateDirectory` recursive or C++17 `std::filesystem::create_directories`).
7. Encode PNG with WIC (`IWICImagingFactory`, `GUID_ContainerFormatPng`) on a **detached std::thread** copying pixel buffer so encode does not block the encoder loop longer than Map+memcpy.
8. Log success/failure.

Handle DXGI formats: `R8G8B8A8_UNORM` / `_SRGB` common; if HDR float path is screenshot source, either convert or capture pre-HDR composition — prefer 8-bit RGB path used for non-HDR; if only float available, convert to 8-bit in CPU for PNG.

- [ ] **Step 4: Ensure `m_captureFrameDir` receives resolved absolute screenshot path**

In `server_openvr/src/lib.rs` `make_settings`:

```rust
let root = alvr_server_core::/* or filesystem */ // use layout if available
// Prefer resolve in Rust before copy to CString:
let resolved = {
    let root = /* layout executables_dir from environment init */;
    let p = PathBuf::from(&settings.extra.capture.screenshot_dir);
    let full = if p.is_absolute() { p } else { root.join(p) };
    full.to_string_lossy().to_string()
};
```

If layout is not accessible from `server_openvr`, resolve using same `FILESYSTEM_LAYOUT` export from server_core:

```rust
// server_core
pub fn resolved_screenshot_dir(settings: &Settings) -> PathBuf { ... }
```

Pass absolute path into `m_captureFrameDir`.

- [ ] **Step 5: Build openvr driver**

Run: `cargo xtask build-server --release` (or project’s usual Windows build).

Expected: driver links; CaptureFrame not empty.

- [ ] **Step 6: Commit**

```bash
git add alvr/server_openvr/cpp/platform/win32/CEncoder.cpp alvr/server_openvr/cpp/platform/win32/CEncoder.h alvr/server_openvr/cpp/platform/win32/FrameRender.cpp alvr/server_openvr/cpp/platform/win32/FrameRender.h alvr/server_openvr/src/lib.rs alvr/server_core/src/lib.rs
git commit -m "feat(server_openvr): save SBS screenshots as PNG on Windows"
```

---

### Task 10: Linux screenshot naming (PNG if feasible)

**Files:**
- Modify: `alvr/server_openvr/cpp/platform/linux/CEncoder.cpp` (~236–244)
- Modify: `alvr/server_openvr/cpp/platform/linux/Renderer.cpp` (`dumpImage`) if converting PPM→PNG

- [ ] **Step 1: Use timestamped paths under `m_captureFrameDir`**

```cpp
// Prefer PNG if dumpImage supports it; else keep PPM with screenshot.*.ppm name
auto ts = /* local timestamp string */;
std::string base = std::string(Settings_Instance()->m_captureFrameDir) + "/screenshot." + ts;
render.CaptureInputFrame(base + "_input.ppm");  // optional debug
render.CaptureOutputFrame(base + ".ppm");       // user-facing until PNG available
```

If PNG encoder already exists or stb is available, write `.png` only for output frame.

- [ ] **Step 2: Build on Linux if available; otherwise compile-check headers**

- [ ] **Step 3: Commit**

```bash
git add alvr/server_openvr/cpp/platform/linux/CEncoder.cpp alvr/server_openvr/cpp/platform/linux/Renderer.cpp
git commit -m "feat(server_openvr): timestamped screenshot paths on Linux"
```

---

### Task 11: Dashboard copy + rolling recording audio polish

**Files:**
- Modify: `alvr/dashboard/src/dashboard/components/debug.rs`
- Modify: `alvr/server_core/src/lib.rs` (rolling file recreation)

- [ ] **Step 1: Update Debug tab label**

```rust
ui.label(
    "Capture frame saves an SBS PNG into the configured screenshot folder. \
     Start/Stop recording writes video + WAV (game audio) into the recording folder. \
     Global hotkeys (default F8 / F9) work while the ALVR driver is running.",
);
```

- [ ] **Step 2: Rolling video files**

When `create_recording_file` is called while a previous recording is active (rolling), ensure `stop_recording` finalize runs first **or** `arm()`’s internal finalize closes the previous WAV before opening a new stem. Current `arm()` calls `finalize()` — verify rolling path does not leak open files.

- [ ] **Step 3: Commit**

```bash
git add alvr/dashboard/src/dashboard/components/debug.rs alvr/server_core/src/lib.rs
git commit -m "docs(dashboard): describe hotkeys and dual-file recording"
```

---

### Task 12: End-to-end verification

**Files:** none (manual)

- [ ] **Step 1: Release build streamer**

Run: project’s Windows build (`build-all.ps1` or `cargo xtask build-server --release` + dashboard).

- [ ] **Step 2: Manual checklist**

1. Install/run driver + Dashboard; connect headset or mock if available.
2. Press **F8** → PNG under `{install}/Captures/Captures/`; hear click.
3. Press **F9** → start sound; files under `Captures/Records/`; press **F9** again → stop sound; `.wav` present if game audio enabled.
4. Dashboard Capture / Start / Stop still work; WAV created on Start path too.
5. Change hotkeys in settings; confirm re-register or document restart if not hot-reloaded.
6. Disable hotkeys / sounds in config.
7. Custom absolute paths work; missing dirs auto-created.
8. Remux short clip: `ffmpeg -i recording.*.h264 -i recording.*.wav -c copy out.mkv` — start roughly aligned.

- [ ] **Step 3: Fix any critical bugs found; commit fixes separately**

- [ ] **Step 4: Final commit if verification notes needed** (optional)

---

## Spec coverage checklist

| Spec requirement | Task |
|------------------|------|
| Global hotkeys in driver | 8 |
| Configurable hotkeys + enable | 1, 8 |
| F8/F9 defaults | 1 |
| SBS PNG screenshot | 9, 10 |
| Screenshot sound on trigger | 7 |
| Toggle recording one key | 8 |
| Distinct start/stop sounds | 7 |
| Game audio only dual WAV | 4, 5, 6 |
| First video write opens WAV | 5 |
| Configurable dirs + Captures defaults | 1, 2, 5, 9 |
| Dashboard/HTTP still work | 5, 11 |
| Minimal perf (tee, no remux) | 6 |
| Windows CaptureFrame implemented | 9 |

## Self-review notes

- No TBD placeholders left for required v1 behavior.
- Linux global hotkeys intentionally stubbed (spec: Windows primary).
- `capture_frame_dir` → `screenshot_dir` migration: old session keys ignored; new defaults apply.
- `record_audio_blocking` signature change must update all call sites in Task 6.
- C++ field remains `m_captureFrameDir` but holds resolved screenshot directory.
