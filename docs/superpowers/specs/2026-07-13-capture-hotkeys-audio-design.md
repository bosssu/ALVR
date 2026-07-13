# Design: PC Capture Hotkeys, SBS Screenshot, Recording Audio

**Date:** 2026-07-13  
**Status:** Approved for planning  
**Scope:** ALVR streamer (PC / SteamVR driver process)

## Summary

Add PC-side global hotkeys for SBS screenshot and toggle video recording, play short feedback sounds, and record game audio alongside the existing elementary video dump with minimal performance impact.

## Goals

1. **Screenshot (SBS):** Global hotkey captures the encoder-side side-by-side stereo frame as PNG; play a sound on trigger.
2. **Recording toggle:** One global hotkey starts/stops recording; distinct sounds for start vs stop.
3. **Audio with recording:** While recording video, also record the game audio stream (loopback already used for streaming); dual files; align audio start with first written video frame.
4. **Configurable:** Hotkeys, enable flags, feedback sounds, and save directories live in session settings (`extra.capture`).

## Non-Goals

- Real-time mux into MP4/MKV (can be a later enhancement).
- Recording microphone audio.
- Headset-side hotkeys.
- Perfect A/V sync under heavy video packet drops (inherent limit of elementary stream + sidecar WAV).
- Replacing OBS / desktop capture for general gameplay recording.

## Current Behavior (baseline)

| Feature | Today |
|---------|--------|
| Video recording | `server_core` writes raw NAL bitstream to `recording.{timestamp}.{h264\|h265\|av1}` under `log_dir` |
| Audio in recording | None |
| Capture frame | Dashboard / HTTP → `ServerCoreEvent::CaptureFrame` → C++ `CaptureFrame()`; **Linux** dumps PPM; **Windows** is a no-op |
| Hotkeys | None |
| Game audio | Loopback via `alvr_audio::record_audio_blocking` in `connection`, streamed to client |

## Architecture (Approach A — minimal intrusion)

```
server_core (inside SteamVR driver process)
├── HotkeyThread (global RegisterHotKey / platform equivalent)
│     F8  → CaptureFrame event + screenshot sound
│     F9  → toggle recording + start/stop sounds
├── Recording
│     video: existing video_recording_file (elementary)
│     audio: AudioRecordingWriter WAV (opens after first video write)
├── game_audio loopback (existing)
│     └── tee PCM → AudioRecordingWriter when armed/active
└── SoundPlayer (rodio, async, non-blocking)

server_openvr C++
└── CEncoder / FrameRender
      CaptureFrame → RGB SBS (post color-correction, pre FFR/YUV) → PNG

Dashboard Debug tab / HTTP APIs unchanged (same start/stop/capture endpoints).
```

**Principles**

- Hotkeys live in the **driver process**, not only Dashboard.
- Reuse existing recording APIs and game-audio capture; **do not** open a second WASAPI loopback.
- Keep encode/network path free of disk/encode stalls for PNG/WAV (async write where needed).
- Dual output: video elementary + WAV; start WAV only after first video payload is written.

## Configuration

Extend `CaptureConfig` in `alvr/session/src/settings.rs` (Dashboard schema auto-generates UI).

| Field | Type | Default | Notes |
|-------|------|---------|--------|
| `startup_video_recording` | bool | false | existing |
| `rolling_video_files` | Switch | off | existing |
| `hotkeys_enabled` | bool | true | Master switch for global hotkeys |
| `screenshot_hotkey` | String | `"F8"` | See key syntax |
| `recording_hotkey` | String | `"F9"` | Toggle start/stop |
| `feedback_sounds_enabled` | bool | true | Screenshot / start / stop sounds |
| `recording_dir` | String | `"Captures/Records"` | Relative to program root, or absolute |
| `screenshot_dir` | String | `"Captures/Captures"` | Relative to program root, or absolute; replaces ad-hoc use of `capture_frame_dir` for user screenshots |

### Path resolution

- **Program root:** ALVR streamer layout root (`FILESYSTEM_LAYOUT` / directory containing Dashboard on Windows).
- Relative paths join program root; absolute paths used as-is.
- Create directories on first write if missing; log and fail the operation if create fails.
- **Recording files:** `{recording_dir}/recording.{YYYY-MM-DD.HH-MM-SS}.{ext}` and matching `.wav` (same stem).
- **Screenshots:** `{screenshot_dir}/screenshot.{YYYY-MM-DD.HH-MM-SS}.png`.

### Migration of `capture_frame_dir`

- Prefer introducing `screenshot_dir` with the new default and mapping/replacing `capture_frame_dir` so C++ `m_captureFrameDir` receives the resolved screenshot path at init / settings push.
- If renaming breaks session JSON compatibility, keep serializing a single field with a clear display name, or accept default for missing keys (ALVR session typically fills defaults for new fields).

### Hotkey string syntax

- Case-insensitive; modifiers joined with `+`: `Ctrl`, `Alt`, `Shift`, `Win`.
- Keys: `F1`–`F12`, `A`–`Z`, `0`–`9`, `PrintScreen`, etc.
- Parse failure → log error, skip that binding.
- Identical screenshot and recording bindings → log warning, refuse dual bind (do not register ambiguous combo).
- Reload on session settings change when practical; otherwise re-register when capture settings change is observed.

## Component Design

### 1. Hotkey thread (`server_core`)

- Spawn from `ServerCoreContext::new` (or shortly after), join on shutdown.
- **Windows:** message-only HWND + `RegisterHotKey` / `GetMessage` loop (reliable process-global hotkeys).
- **Linux:** best-effort (e.g. `EVIOCGRAB`-free global key APIs or document limitation); Windows is the primary target for this feature set.
- On key:
  - Screenshot → `events_sender.send(CaptureFrame)` + screenshot sound.
  - Recording → if `video_recording_file` is `None` (and not mid-start) → start; else → stop.
- Debounce ~300 ms.
- Do not require an active client for screenshot (may no-op in encoder if no frame); recording start without stream may create empty/short files—prefer start only when streaming if easy to gate; otherwise match current HTTP behavior.

### 2. SBS screenshot (PNG)

**Capture source:** RGB SBS after composition and color correction, **before** FFR and YUV. Full-resolution stereo still, not foveation-warped.

**Windows**

1. `CEncoder::CaptureFrame()` sets atomic/bool flag.
2. `FrameRender` retains a screenshot-source texture pointer (RGB).
3. Next frame: GPU→CPU staging copy; encode PNG via **WIC** on a worker thread.
4. Filename under resolved `screenshot_dir`.

**Linux**

- Extend existing capture path (`dumpImage` / CaptureOutputFrame) to write PNG with the same naming scheme (retire or supersede fixed `alvr_frame_*.ppm` names for this user-facing feature).

**Sound:** play on hotkey/API trigger (confirmation), not only after disk success.

### 3. Recording toggle

- **Start:** same as `create_recording_file` today (path uses `recording_dir` + codec extension), request IDR, arm audio writer.
- **Stop:** drop `video_recording_file`; finalize WAV.
- HTTP `/recording/start|stop` and Dashboard buttons call the same helpers so audio arming is shared (no hotkey-only path).
- Rolling video files: each roll closes previous video and should finalize/re-open WAV with a new stem (or document that rolling + audio uses new pair per segment).

### 4. Audio tee (game audio only)

**Source:** PCM already produced in `alvr_audio::record_audio_blocking` (same samples sent to the headset).

**Writer:** `AudioRecordingWriter`

- Format: WAV, PCM s16le, stereo, sample rate = loopback device rate.
- States: Idle → Armed (after start, waiting first video write) → Recording → Idle.
- **First video NAL/config written** transitions Armed → Recording and opens the WAV (eliminates IDR wait skew).
- Callback path: lock-free or short mutex; write raw frames only; no resample/encode.
- If game audio disabled: video-only recording; log once that WAV is skipped.
- On stop / client disconnect: finalize header (sizes); discard empty Armed-only WAV if zero samples.

**Sync expectations**

- Start aligned to first video write (primary mitigation).
- Dual-file remux (`ffmpeg -i video -i audio -c copy out.mkv`) is best-effort.
- Dropped video packets while audio continues can drift; acceptable for this design.

Optional later: sidecar JSON with wall-clock offsets (not required in v1).

### 5. Feedback sounds

| Event | Sound |
|-------|--------|
| Screenshot | Short click / shutter |
| Recording start | Higher “arm” beep |
| Recording stop | Lower “disarm” beep |

- Bundle small WAV/OGG under streamer resources; load once.
- Play via rodio on a dedicated low-priority path; never block encode/hotkey loop beyond “start playback”.
- Respect `feedback_sounds_enabled`.
- Failure to open device: log once, continue without sound.

## Data Flow

### Screenshot

```
Hotkey/HTTP → ServerCoreEvent::CaptureFrame
  → server_openvr polls event → CaptureFrame()
  → next rendered RGB SBS → PNG to screenshot_dir
Hotkey/HTTP → SoundPlayer (screenshot)
```

### Recording start

```
Hotkey/HTTP → create_recording_file(recording_dir)
  → write decoder config if any, RequestIDR
  → arm AudioRecordingWriter (same stem)
First send_video_nal / config write while armed
  → open WAV, state = Recording
game_audio callback → tee to WAV if Recording
SoundPlayer (start)
```

### Recording stop

```
Hotkey/HTTP → close video file; finalize WAV; disarm
SoundPlayer (stop)
```

## Error Handling

| Condition | Behavior |
|-----------|----------|
| Hotkey registration fails | Log warning; Dashboard/HTTP still work |
| Invalid hotkey string | Log; skip that binding |
| Disk full / write error | Log; stop recording / skip screenshot; no crash |
| No game audio | Video only; log |
| Capture while not streaming | No PNG or empty attempt; log |
| Shutdown mid-record | Close files best-effort |

## Performance

- Hotkey thread: idle on message wait; negligible CPU.
- Audio tee: memcpy of samples already captured; one extra file write.
- PNG: async encode; one frame GPU readback.
- No second audio device open; no video re-encode; no live muxer.

## Testing / Acceptance

1. F8 saves SBS PNG under `Captures/Captures` (or configured path); sound plays.
2. F9 starts recording → files under `Captures/Records`; start sound; second F9 stops; stop sound; `.wav` present when game audio on.
3. Remux short clip: A/V start roughly aligned (no multi-second offset).
4. Change hotkeys in settings; re-register works (or after documented reload).
5. Disable hotkeys / sounds via config.
6. Dashboard Start/Stop still works and also produces WAV.
7. Windows CaptureFrame no longer a no-op.
8. Custom absolute paths work; missing dirs are created.

## Implementation Touchpoints (expected)

| Area | Paths |
|------|--------|
| Settings | `alvr/session/src/settings.rs` |
| Core recording / context | `alvr/server_core/src/lib.rs`, `web_server.rs`, `connection.rs` |
| Hotkeys / sounds / audio writer | new modules under `alvr/server_core/src/` |
| Audio tee hook | `alvr/audio/src/lib.rs` |
| Windows capture | `alvr/server_openvr/cpp/platform/win32/CEncoder.*`, `FrameRender.*` |
| Linux capture | `alvr/server_openvr/cpp/platform/linux/*` |
| Bindings / capture dir | `bindings.h`, `server_openvr/src/lib.rs` |
| Resources | short WAV assets + install copy in xtask if needed |

## Resolved Decisions

| Topic | Decision |
|-------|----------|
| Hotkey host | SteamVR driver process (global) |
| Architecture | Approach A (minimal intrusion) |
| Audio sources | Game audio loopback only |
| Container | Dual file: elementary video + WAV |
| A/V sync | Open WAV after first video write |
| Screenshot | Encoder-side RGB SBS PNG (pre FFR/YUV) |
| Default keys | F8 screenshot, F9 recording toggle |
| Paths | Configurable; default `{root}/Captures/Records` and `{root}/Captures/Captures` |

## Future Extensions (out of scope)

- Auto-mux to MKV on stop via bundled ffmpeg.
- Optional sidecar JSON for mux offsets.
- Mic mix / multi-track audio.
- Linux parity for global hotkeys if initial port is incomplete.
