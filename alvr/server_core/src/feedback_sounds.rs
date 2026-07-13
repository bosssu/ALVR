use alvr_common::error;
use std::{
    sync::{
        mpsc,
        Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Debug, Clone, Copy)]
pub enum FeedbackKind {
    Screenshot,
    RecStart,
    RecStop,
}

/// Lazy feedback tones: the audio device is opened only on first play, not at driver load.
pub struct FeedbackSounds {
    inner: Mutex<Option<FeedbackInner>>,
}

struct FeedbackInner {
    tx: mpsc::Sender<FeedbackKind>,
    join: Option<JoinHandle<()>>,
}

impl FeedbackSounds {
    pub fn start() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    fn ensure_started(inner: &mut Option<FeedbackInner>) {
        if inner.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel::<FeedbackKind>();
        let join = thread::spawn(move || {
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
                use rodio::Source;
                let (freq, ms) = match kind {
                    FeedbackKind::Screenshot => (1200.0, 60),
                    FeedbackKind::RecStart => (880.0, 120),
                    FeedbackKind::RecStop => (440.0, 120),
                };
                let src = rodio::source::SineWave::new(freq)
                    .take_duration(Duration::from_millis(ms))
                    .amplify(0.2);
                sink.append(src);
            }
        });
        *inner = Some(FeedbackInner {
            tx,
            join: Some(join),
        });
    }

    pub fn play(&self, kind: FeedbackKind, enabled: bool) {
        if !enabled {
            return;
        }
        let mut guard = self.inner.lock().unwrap();
        Self::ensure_started(&mut guard);
        if let Some(inner) = guard.as_ref() {
            let _ = inner.tx.send(kind);
        }
    }
}

impl Drop for FeedbackSounds {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(mut inner) = guard.take() {
                // Drop sender to end the thread, then join.
                drop(inner.tx);
                if let Some(join) = inner.join.take() {
                    let _ = join.join();
                }
            }
        }
    }
}
