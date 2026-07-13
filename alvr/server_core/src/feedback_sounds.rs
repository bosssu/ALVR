use std::thread;

#[derive(Debug, Clone, Copy)]
pub enum FeedbackKind {
    Screenshot,
    RecStart,
    RecStop,
}

/// Driver-safe feedback: never opens WASAPI/rodio inside vrserver (that can crash SteamVR).
/// On Windows uses kernel `Beep` on a short-lived thread; elsewhere is a no-op.
pub struct FeedbackSounds;

impl FeedbackSounds {
    pub fn start() -> Self {
        Self
    }

    pub fn play(&self, kind: FeedbackKind, enabled: bool) {
        if !enabled {
            return;
        }

        #[cfg(windows)]
        {
            let (freq, ms) = match kind {
                FeedbackKind::Screenshot => (1200u32, 60u32),
                FeedbackKind::RecStart => (880, 120),
                FeedbackKind::RecStop => (440, 120),
            };
            // Never block the hotkey / server thread on Beep.
            // kernel32 Beep does not open WASAPI devices (safe inside vrserver).
            thread::spawn(move || {
                unsafe {
                    unsafe extern "system" {
                        fn Beep(dwFreq: u32, dwDuration: u32) -> i32;
                    }
                    Beep(freq, ms);
                }
            });
        }

        #[cfg(not(windows))]
        {
            let _ = kind;
        }
    }
}
