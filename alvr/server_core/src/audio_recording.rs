use alvr_common::anyhow::{Context, Result};
use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRecState {
    Idle,
    Armed,
    Recording,
}

/// Writes a PCM s16le WAV, opened only after the first video bytes are written (A/V start align).
pub struct AudioRecordingWriter {
    state: AudioRecState,
    stem: Option<PathBuf>,
    sample_rate: u32,
    channels: u16,
    file: Option<File>,
    data_bytes: u32,
    /// Drop loopback samples until this instant (keeps feedback beeps out of the WAV).
    mute_until: Option<Instant>,
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
            file: None,
            data_bytes: 0,
            mute_until: None,
        }
    }

    pub fn state(&self) -> AudioRecState {
        self.state
    }

    /// Drop game-audio samples for a short window (e.g. while playing a feedback beep).
    pub fn mute_for(&mut self, duration: Duration) {
        let until = Instant::now() + duration;
        self.mute_until = Some(match self.mute_until {
            Some(prev) if prev > until => prev,
            _ => until,
        });
    }

    /// Prepare to record; WAV file is created only on first video write.
    pub fn arm(&mut self, stem: PathBuf, sample_rate: u32, channels: u16) {
        let _ = self.finalize();
        self.stem = Some(stem);
        self.sample_rate = sample_rate.max(1);
        self.channels = channels.max(1);
        self.state = AudioRecState::Armed;
        self.data_bytes = 0;
        self.mute_until = None;
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
        self.file = Some(file);
        self.state = AudioRecState::Recording;
        Ok(())
    }

    pub fn write_pcm_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        if self.state != AudioRecState::Recording {
            return Ok(());
        }
        if let Some(until) = self.mute_until {
            if Instant::now() < until {
                return Ok(());
            }
            self.mute_until = None;
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
        } else if self.state == AudioRecState::Armed {
            // Never opened — nothing on disk
        }
        self.stem = None;
        self.state = AudioRecState::Idle;
        self.data_bytes = 0;
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
        let stem = dir.join("recording.test");
        let mut w = AudioRecordingWriter::new();
        w.arm(stem.clone(), 48000, 2);
        assert_eq!(w.state(), AudioRecState::Armed);
        w.on_video_bytes_written().unwrap();
        assert_eq!(w.state(), AudioRecState::Recording);
        // two stereo frames
        w.write_pcm_bytes(&[0, 0, 0, 0, 100, 0, 156, 255]).unwrap();
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
        w.write_pcm_bytes(&[1, 2, 3, 4]).unwrap();
        assert_eq!(w.state(), AudioRecState::Idle);
    }
}
