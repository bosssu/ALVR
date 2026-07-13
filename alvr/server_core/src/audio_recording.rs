use alvr_common::{
    anyhow::{Context, Result},
    parking_lot::Mutex as ParkingMutex,
};
use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRecState {
    Idle,
    Armed,
    Recording,
}

/// PCM s16le WAV writer. Loopback samples are queued (never block the audio callback);
/// a dedicated thread writes the file so video-path locks cannot drop audio.
pub struct AudioRecordingWriter {
    state: AudioRecState,
    stem: Option<PathBuf>,
    sample_rate: u32,
    channels: u16,
    /// True while Armed and waiting for first video bytes to open the WAV.
    pending_open: Arc<AtomicBool>,
    pcm_tx: Option<flume::Sender<Vec<u8>>>,
    writer_join: Option<JoinHandle<Result<u32>>>,
}

impl Default for AudioRecordingWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioRecordingWriter {
    pub fn new() -> Self {
        Self {
            state: AudioRecState::Idle,
            stem: None,
            sample_rate: 48000,
            channels: 2,
            pending_open: Arc::new(AtomicBool::new(false)),
            pcm_tx: None,
            writer_join: None,
        }
    }

    pub fn state(&self) -> AudioRecState {
        self.state
    }

    /// Cheap check for the video path: only take the audio mutex when this is true.
    pub fn needs_video_open(&self) -> bool {
        self.pending_open.load(Ordering::Acquire)
    }

    /// Prepare to record; WAV is created on first video write.
    pub fn arm(&mut self, stem: PathBuf, sample_rate: u32, channels: u16) {
        let _ = self.finalize();
        self.stem = Some(stem);
        self.sample_rate = sample_rate.max(1);
        self.channels = channels.max(1);
        self.state = AudioRecState::Armed;
        self.pending_open.store(true, Ordering::Release);
    }

    pub fn on_video_bytes_written(&mut self) -> Result<()> {
        if self.state != AudioRecState::Armed {
            return Ok(());
        }
        let path = self
            .stem
            .as_ref()
            .map(|s| crate::capture_paths::with_media_ext(s, "wav"))
            .context("no recording stem")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = File::create(&path)?;
        write_wav_header_placeholder(&mut file, self.sample_rate, self.channels)?;

        let (tx, rx) = flume::unbounded::<Vec<u8>>();
        let join = thread::spawn(move || -> Result<u32> {
            let mut data_bytes: u32 = 0;
            while let Ok(chunk) = rx.recv() {
                file.write_all(&chunk)?;
                data_bytes = data_bytes.saturating_add(chunk.len() as u32);
            }
            // Channel closed: patch sizes before file drops.
            patch_wav_sizes(&mut file, data_bytes)?;
            file.flush()?;
            Ok(data_bytes)
        });

        self.pcm_tx = Some(tx);
        self.writer_join = Some(join);
        self.state = AudioRecState::Recording;
        self.pending_open.store(false, Ordering::Release);
        Ok(())
    }

    /// Non-blocking: queue samples for the writer thread. Safe to call from WASAPI/cpal callback.
    pub fn write_pcm_bytes(&mut self, bytes: &[u8]) {
        if self.state != AudioRecState::Recording {
            return;
        }
        if let Some(tx) = &self.pcm_tx {
            // Unbounded queue — never blocks the audio callback.
            let _ = tx.send(bytes.to_vec());
        }
    }

    pub fn finalize(&mut self) -> Result<()> {
        self.pending_open.store(false, Ordering::Release);
        // Drop sender so writer thread exits and patches the header.
        self.pcm_tx = None;
        if let Some(join) = self.writer_join.take() {
            match join.join() {
                Ok(Ok(_bytes)) => {}
                Ok(Err(e)) => {
                    self.stem = None;
                    self.state = AudioRecState::Idle;
                    return Err(e);
                }
                Err(_) => {
                    self.stem = None;
                    self.state = AudioRecState::Idle;
                    alvr_common::error!("Audio recording writer thread panicked");
                }
            }
        }
        self.stem = None;
        self.state = AudioRecState::Idle;
        Ok(())
    }
}

fn write_wav_header_placeholder(file: &mut File, sample_rate: u32, channels: u16) -> Result<()> {
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
    let block_align = channels * bits_per_sample / 8;
    file.write_all(b"RIFF")?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(b"WAVE")?;
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&bits_per_sample.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&0u32.to_le_bytes())?;
    Ok(())
}

fn patch_wav_sizes(file: &mut File, data_bytes: u32) -> Result<()> {
    let riff_size = 36u32.saturating_add(data_bytes);
    file.seek(SeekFrom::Start(4))?;
    file.write_all(&riff_size.to_le_bytes())?;
    file.seek(SeekFrom::Start(40))?;
    file.write_all(&data_bytes.to_le_bytes())?;
    Ok(())
}

/// Shared flag for video path without taking the full writer lock every frame.
pub fn note_video_if_pending(writer: &ParkingMutex<AudioRecordingWriter>) {
    // Fast path: no open pending.
    let pending = {
        let w = writer.lock();
        if !w.needs_video_open() {
            return;
        }
        true
    };
    if !pending {
        return;
    }
    if let Err(e) = writer.lock().on_video_bytes_written() {
        alvr_common::error!("Failed to open recording WAV: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn arm_then_first_video_opens_wav() {
        let dir = std::env::temp_dir().join(format!(
            "alvr_audio_rec_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let stem = dir.join("recording_test");
        let mut w = AudioRecordingWriter::new();
        w.arm(stem.clone(), 48000, 2);
        assert_eq!(w.state(), AudioRecState::Armed);
        assert!(w.needs_video_open());
        w.on_video_bytes_written().unwrap();
        assert_eq!(w.state(), AudioRecState::Recording);
        assert!(!w.needs_video_open());
        w.write_pcm_bytes(&[0, 0, 0, 0, 100, 0, 156, 255]);
        w.finalize().unwrap();
        let mut f = File::open(crate::capture_paths::with_media_ext(&stem, "wav")).unwrap();
        let mut hdr = [0u8; 12];
        f.read_exact(&mut hdr).unwrap();
        assert_eq!(&hdr[0..4], b"RIFF");
        assert_eq!(&hdr[8..12], b"WAVE");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finalize_armed_without_open_is_ok() {
        let mut w = AudioRecordingWriter::new();
        w.arm(std::env::temp_dir().join("alvr_nope_recording"), 44100, 2);
        w.finalize().unwrap();
        assert_eq!(w.state(), AudioRecState::Idle);
    }

    #[test]
    fn write_while_idle_is_noop() {
        let mut w = AudioRecordingWriter::new();
        w.write_pcm_bytes(&[1, 2, 3, 4]);
        assert_eq!(w.state(), AudioRecState::Idle);
    }
}
