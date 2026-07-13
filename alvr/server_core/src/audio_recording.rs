use alvr_common::anyhow::{Context, Result};
use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    thread::{self, JoinHandle},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRecState {
    Idle,
    Recording,
}

/// PCM s16le WAV writer. Loopback samples are queued on a dedicated thread so the
/// audio callback never blocks on disk I/O or the video path.
pub struct AudioRecordingWriter {
    state: AudioRecState,
    sample_rate: u32,
    channels: u16,
    pcm_tx: Option<flume::Sender<Vec<u8>>>,
    writer_join: Option<JoinHandle<Result<u32>>>,
    path: Option<PathBuf>,
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
            sample_rate: 48000,
            channels: 2,
            pcm_tx: None,
            writer_join: None,
            path: None,
        }
    }

    pub fn state(&self) -> AudioRecState {
        self.state
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Start recording immediately (same wall-clock moment as the video file is created).
    /// `sample_rate` must match the live loopback device rate used by `record_audio_blocking`.
    pub fn start(&mut self, stem: PathBuf, sample_rate: u32, channels: u16) -> Result<()> {
        let _ = self.finalize();

        let sample_rate = sample_rate.max(1);
        let channels = channels.max(1);
        let path = crate::capture_paths::with_media_ext(&stem, "wav");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = File::create(&path)
            .with_context(|| format!("create wav {}", path.display()))?;
        write_wav_header_placeholder(&mut file, sample_rate, channels)?;

        let (tx, rx) = flume::unbounded::<Vec<u8>>();
        let join = thread::spawn(move || -> Result<u32> {
            let mut data_bytes: u32 = 0;
            while let Ok(chunk) = rx.recv() {
                file.write_all(&chunk)?;
                data_bytes = data_bytes.saturating_add(chunk.len() as u32);
            }
            patch_wav_sizes(&mut file, data_bytes)?;
            file.flush()?;
            Ok(data_bytes)
        });

        self.sample_rate = sample_rate;
        self.channels = channels;
        self.pcm_tx = Some(tx);
        self.writer_join = Some(join);
        self.path = Some(path);
        self.state = AudioRecState::Recording;

        alvr_common::info!(
            "Recording WAV started ({} Hz, {} ch): {}",
            sample_rate,
            channels,
            self.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
        );
        Ok(())
    }

    /// Non-blocking queue; safe from the WASAPI/cpal callback.
    pub fn write_pcm_bytes(&mut self, bytes: &[u8]) {
        if self.state != AudioRecState::Recording {
            return;
        }
        if let Some(tx) = &self.pcm_tx {
            let _ = tx.send(bytes.to_vec());
        }
    }

    pub fn finalize(&mut self) -> Result<()> {
        self.pcm_tx = None;
        if let Some(join) = self.writer_join.take() {
            match join.join() {
                Ok(Ok(data_bytes)) => {
                    let secs = data_bytes as f64
                        / (self.sample_rate as f64 * self.channels as f64 * 2.0);
                    alvr_common::info!(
                        "Recording WAV finished: {data_bytes} bytes, ~{secs:.2}s at {} Hz",
                        self.sample_rate
                    );
                }
                Ok(Err(e)) => {
                    self.state = AudioRecState::Idle;
                    self.path = None;
                    return Err(e);
                }
                Err(_) => {
                    alvr_common::error!("Audio recording writer thread panicked");
                }
            }
        }
        self.path = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn start_writes_wav_immediately() {
        let dir = std::env::temp_dir().join(format!(
            "alvr_audio_rec_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let stem = dir.join("recording_test");
        let mut w = AudioRecordingWriter::new();
        w.start(stem.clone(), 44100, 2).unwrap();
        assert_eq!(w.state(), AudioRecState::Recording);
        assert_eq!(w.sample_rate(), 44100);
        w.write_pcm_bytes(&[0, 0, 0, 0, 100, 0, 156, 255]);
        w.finalize().unwrap();
        let mut f = File::open(crate::capture_paths::with_media_ext(&stem, "wav")).unwrap();
        let mut hdr = [0u8; 44];
        f.read_exact(&mut hdr).unwrap();
        assert_eq!(&hdr[0..4], b"RIFF");
        assert_eq!(&hdr[8..12], b"WAVE");
        // sample rate little-endian at offset 24
        let rate = u32::from_le_bytes([hdr[24], hdr[25], hdr[26], hdr[27]]);
        assert_eq!(rate, 44100);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_while_idle_is_noop() {
        let mut w = AudioRecordingWriter::new();
        w.write_pcm_bytes(&[1, 2, 3, 4]);
        assert_eq!(w.state(), AudioRecState::Idle);
    }
}
