use alvr_common::error;
use std::{
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Debug, Clone, Copy)]
pub enum FeedbackKind {
    Screenshot,
    RecStart,
    RecStop,
}

pub struct FeedbackSounds {
    tx: Option<mpsc::Sender<FeedbackKind>>,
    join: Option<JoinHandle<()>>,
}

impl FeedbackSounds {
    pub fn start() -> Self {
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
        Self {
            tx: Some(tx),
            join: Some(join),
        }
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
        self.tx.take();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
