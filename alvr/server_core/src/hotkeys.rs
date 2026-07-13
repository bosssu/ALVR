use alvr_common::{
    anyhow::{bail, Result},
    error, info, warn,
};
use alvr_session::Settings;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

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
    let parts: Vec<&str> = s
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
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
    if u == "PRINTSCREEN" || u == "PRTSC" || u == "PRTSCN" {
        return Ok("PRINTSCREEN".into());
    }
    if u.len() == 1 {
        let c = u.chars().next().unwrap();
        if c.is_ascii_alphanumeric() {
            return Ok(u);
        }
    }
    if u.starts_with('F')
        && u[1..]
            .parse::<u8>()
            .ok()
            .is_some_and(|n| (1..=12).contains(&n))
    {
        return Ok(u);
    }
    if matches!(
        u.as_str(),
        "PAUSE"
            | "SCROLLLOCK"
            | "INSERT"
            | "DELETE"
            | "HOME"
            | "END"
            | "PRIOR"
            | "NEXT"
            | "LEFT"
            | "RIGHT"
            | "UP"
            | "DOWN"
            | "SPACE"
    ) {
        return Ok(u);
    }
    bail!("unsupported key: {key}")
}

#[derive(Debug, Clone, Copy)]
pub enum HotkeyAction {
    Screenshot,
    ToggleRecording,
}

pub struct HotkeyThread {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    #[cfg(windows)]
    thread_id: Arc<std::sync::Mutex<Option<u32>>>,
}

impl HotkeyThread {
    pub fn start(
        get_settings: Arc<dyn Fn() -> Settings + Send + Sync>,
        on_action: Arc<dyn Fn(HotkeyAction) + Send + Sync>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        #[cfg(windows)]
        let thread_id = Arc::new(std::sync::Mutex::new(None));
        #[cfg(windows)]
        let thread_id2 = Arc::clone(&thread_id);

        let join = thread::spawn(move || {
            #[cfg(windows)]
            {
                windows_hotkey_loop(stop2, get_settings, on_action, thread_id2);
            }
            #[cfg(not(windows))]
            {
                let _ = (get_settings, on_action);
                while !stop2.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(200));
                }
            }
        });

        Self {
            stop,
            join: Some(join),
            #[cfg(windows)]
            thread_id,
        }
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        #[cfg(windows)]
        {
            use windows::Win32::{
                Foundation::{LPARAM, WPARAM},
                UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT},
            };
            if let Ok(guard) = self.thread_id.lock()
                && let Some(tid) = *guard
            {
                unsafe {
                    let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
                }
            }
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[cfg(windows)]
fn windows_hotkey_loop(
    stop: Arc<AtomicBool>,
    get_settings: Arc<dyn Fn() -> Settings + Send + Sync>,
    on_action: Arc<dyn Fn(HotkeyAction) + Send + Sync>,
    thread_id_slot: Arc<std::sync::Mutex<Option<u32>>>,
) {
    use windows::Win32::{
        Foundation::HWND,
        System::Threading::GetCurrentThreadId,
        UI::{
            Input::KeyboardAndMouse::{
                RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, RegisterClassW,
                TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, MSG, PM_REMOVE, WM_HOTKEY,
                WM_QUIT, WNDCLASSW, WS_OVERLAPPED,
            },
        },
    };
    use windows::core::PCWSTR;

    unsafe {
        let tid = GetCurrentThreadId();
        *thread_id_slot.lock().unwrap() = Some(tid);

        let class_name: Vec<u16> = "ALVR_HotkeyWindow\0".encode_utf16().collect();
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(def_wnd_proc),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            error!("Failed to register hotkey window class");
            return;
        }

        let hwnd = match CreateWindowExW(
            Default::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(class_name.as_ptr()),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            None,
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                error!("Failed to create hotkey message window: {e}");
                return;
            }
        };

        if hwnd == HWND::default() {
            error!("Failed to create hotkey message window (null hwnd)");
            return;
        }

        const ID_SCREENSHOT: i32 = 1;
        const ID_RECORDING: i32 = 2;

        let mut last_settings_hash = String::new();
        let mut last_fire = Instant::now() - Duration::from_secs(1);

        let mut msg = MSG::default();
        while !stop.load(Ordering::Relaxed) {
            let settings = get_settings();
            let hash = format!(
                "{}|{}|{}",
                settings.extra.capture.hotkeys_enabled,
                settings.extra.capture.screenshot_hotkey,
                settings.extra.capture.recording_hotkey,
            );
            if hash != last_settings_hash {
                last_settings_hash = hash;
                let _ = UnregisterHotKey(Some(hwnd), ID_SCREENSHOT);
                let _ = UnregisterHotKey(Some(hwnd), ID_RECORDING);

                if settings.extra.capture.hotkeys_enabled {
                    match (
                        parse_hotkey(&settings.extra.capture.screenshot_hotkey),
                        parse_hotkey(&settings.extra.capture.recording_hotkey),
                    ) {
                        (Ok(shot), Ok(rec)) if shot == rec => {
                            warn!(
                                "Screenshot and recording hotkeys are identical ({}); not registering",
                                settings.extra.capture.screenshot_hotkey
                            );
                        }
                        (Ok(shot), Ok(rec)) => {
                            if let Some((mods, vk)) = to_win_hotkey(&shot) {
                                let mods = HOT_KEY_MODIFIERS(mods | MOD_NOREPEAT.0 as u32);
                                if RegisterHotKey(Some(hwnd), ID_SCREENSHOT, mods, vk).is_ok() {
                                    info!(
                                        "Registered screenshot hotkey: {}",
                                        settings.extra.capture.screenshot_hotkey
                                    );
                                } else {
                                    error!(
                                        "Failed to register screenshot hotkey {}",
                                        settings.extra.capture.screenshot_hotkey
                                    );
                                }
                            } else {
                                error!("Unsupported screenshot key: {}", shot.vk_name);
                            }
                            if let Some((mods, vk)) = to_win_hotkey(&rec) {
                                let mods = HOT_KEY_MODIFIERS(mods | MOD_NOREPEAT.0 as u32);
                                if RegisterHotKey(Some(hwnd), ID_RECORDING, mods, vk).is_ok() {
                                    info!(
                                        "Registered recording hotkey: {}",
                                        settings.extra.capture.recording_hotkey
                                    );
                                } else {
                                    error!(
                                        "Failed to register recording hotkey {}",
                                        settings.extra.capture.recording_hotkey
                                    );
                                }
                            } else {
                                error!("Unsupported recording key: {}", rec.vk_name);
                            }
                        }
                        (Err(e), _) => error!("Invalid screenshot hotkey: {e}"),
                        (_, Err(e)) => error!("Invalid recording hotkey: {e}"),
                    }
                }
            }

            let mut had = false;
            while PeekMessageW(&mut msg, Some(hwnd), 0, 0, PM_REMOVE).as_bool() {
                had = true;
                if msg.message == WM_QUIT {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
                if msg.message == WM_HOTKEY {
                    if last_fire.elapsed() < Duration::from_millis(300) {
                        continue;
                    }
                    last_fire = Instant::now();
                    match msg.wParam.0 as i32 {
                        ID_SCREENSHOT => on_action(HotkeyAction::Screenshot),
                        ID_RECORDING => on_action(HotkeyAction::ToggleRecording),
                        _ => {}
                    }
                } else {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if !had {
                thread::sleep(Duration::from_millis(50));
            }
        }

        let _ = UnregisterHotKey(Some(hwnd), ID_SCREENSHOT);
        let _ = UnregisterHotKey(Some(hwnd), ID_RECORDING);
        let _ = DefWindowProcW; // keep import used if needed
    }
}

#[cfg(windows)]
unsafe extern "system" fn def_wnd_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    unsafe { windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam) }
}

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
        "F1" => VK_F1.0 as u32,
        "F2" => VK_F2.0 as u32,
        "F3" => VK_F3.0 as u32,
        "F4" => VK_F4.0 as u32,
        "F5" => VK_F5.0 as u32,
        "F6" => VK_F6.0 as u32,
        "F7" => VK_F7.0 as u32,
        "F8" => VK_F8.0 as u32,
        "F9" => VK_F9.0 as u32,
        "F10" => VK_F10.0 as u32,
        "F11" => VK_F11.0 as u32,
        "F12" => VK_F12.0 as u32,
        "PRINTSCREEN" => VK_SNAPSHOT.0 as u32,
        "PAUSE" => VK_PAUSE.0 as u32,
        "SPACE" => VK_SPACE.0 as u32,
        "INSERT" => VK_INSERT.0 as u32,
        "DELETE" => VK_DELETE.0 as u32,
        "HOME" => VK_HOME.0 as u32,
        "END" => VK_END.0 as u32,
        "PRIOR" => VK_PRIOR.0 as u32,
        "NEXT" => VK_NEXT.0 as u32,
        "LEFT" => VK_LEFT.0 as u32,
        "RIGHT" => VK_RIGHT.0 as u32,
        "UP" => VK_UP.0 as u32,
        "DOWN" => VK_DOWN.0 as u32,
        s if s.len() == 1 => {
            let c = s.chars().next()?.to_ascii_uppercase();
            if c.is_ascii_alphanumeric() {
                c as u32
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some((mods, vk))
}

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

    #[test]
    fn parse_printscreen() {
        let k = parse_hotkey("PrintScreen").unwrap();
        assert_eq!(k.vk_name, "PRINTSCREEN");
    }
}
