//! Настраиваемые глобальные горячие клавиши (`RegisterHotKey`).
//! По умолчанию: `Ctrl+Alt+I` (остров), `Ctrl+Alt+T` (помодоро / пауза), `Ctrl+Alt+M` (режим капсулы).

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use tauri::{AppHandle, Emitter, Manager};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, SendMessageW, WM_APP, WM_HOTKEY};

use crate::{winmsg, Shared};

const HK_TOGGLE: i32 = 1;
const HK_TIMER: i32 = 2;
const HK_CAPSULE: i32 = 3;
const HK_MIC: i32 = 4;
const WM_SYNC_HOTKEYS: u32 = WM_APP + 1;

static HOTKEY_HWND: AtomicIsize = AtomicIsize::new(0);
static TOGGLE_OK: AtomicBool = AtomicBool::new(true);
static TIMER_OK: AtomicBool = AtomicBool::new(true);
static CAPSULE_OK: AtomicBool = AtomicBool::new(true);
static MIC_OK: AtomicBool = AtomicBool::new(true);

thread_local! {
    static APP: RefCell<Option<AppHandle>> = const { RefCell::new(None) };
}

/// Разбирает строку вида `"Ctrl+Alt+I"` или `"Ctrl+Shift+Space"` в `(модификаторы, VK)`.
pub fn parse_hotkey(spec: &str) -> Option<(u32, u32)> {
    let mut mods = MOD_NOREPEAT.0;
    let mut vk: Option<u32> = None;

    for raw in spec.split('+') {
        let tok = raw.trim().to_ascii_uppercase();
        if tok.is_empty() {
            continue;
        }
        match tok.as_str() {
            "CTRL" | "CONTROL" => mods |= MOD_CONTROL.0,
            "ALT" => mods |= MOD_ALT.0,
            "SHIFT" => mods |= MOD_SHIFT.0,
            "SPACE" => vk = Some(0x20),
            "ENTER" | "RETURN" => vk = Some(0x0D),
            "TAB" => vk = Some(0x09),
            "ESC" | "ESCAPE" => vk = Some(0x1B),
            s if s.len() == 1 => {
                let b = s.as_bytes()[0];
                if b.is_ascii_alphanumeric() {
                    vk = Some(b as u32);
                } else {
                    return None;
                }
            }
            s if s.starts_with('F') => {
                let n: u32 = s[1..].parse().ok()?;
                if (1..=12).contains(&n) {
                    vk = Some(0x70 + n - 1);
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }

    let code = vk?;
    if mods == MOD_NOREPEAT.0 {
        return None;
    }
    Some((mods, code))
}

unsafe fn register_one(hwnd: HWND, id: i32, spec: &str) -> bool {
    let Some((mods, vk)) = parse_hotkey(spec) else { return false };
    if RegisterHotKey(Some(hwnd), id, HOT_KEY_MODIFIERS(mods), vk).is_err() {
        APP.with(|c| {
            crate::log_err(c.borrow().as_ref(), &format!("RegisterHotKey {spec} failed"));
        });
        return false;
    }
    true
}

unsafe fn apply_registration(hwnd: HWND) {
    let _ = UnregisterHotKey(Some(hwnd), HK_TOGGLE);
    let _ = UnregisterHotKey(Some(hwnd), HK_TIMER);
    let _ = UnregisterHotKey(Some(hwnd), HK_CAPSULE);
    let _ = UnregisterHotKey(Some(hwnd), HK_MIC);
    APP.with(|c| {
        if let Some(app) = c.borrow().as_ref() {
            let s = crate::lock(&app.state::<Shared>().settings).clone();
            if s.hotkeys {
                let ok1 = register_one(hwnd, HK_TOGGLE, &s.hotkey_toggle);
                let ok2 = if s.hotkey_timer.eq_ignore_ascii_case(&s.hotkey_toggle) {
                    false
                } else {
                    register_one(hwnd, HK_TIMER, &s.hotkey_timer)
                };
                let ok3 = if s.hotkey_capsule.eq_ignore_ascii_case(&s.hotkey_toggle)
                    || s.hotkey_capsule.eq_ignore_ascii_case(&s.hotkey_timer)
                {
                    false
                } else {
                    register_one(hwnd, HK_CAPSULE, &s.hotkey_capsule)
                };
                let ok4 = if s.hotkey_mic.eq_ignore_ascii_case(&s.hotkey_toggle)
                    || s.hotkey_mic.eq_ignore_ascii_case(&s.hotkey_timer)
                    || s.hotkey_mic.eq_ignore_ascii_case(&s.hotkey_capsule)
                {
                    false
                } else {
                    register_one(hwnd, HK_MIC, &s.hotkey_mic)
                };
                TOGGLE_OK.store(ok1, Ordering::Relaxed);
                TIMER_OK.store(ok2, Ordering::Relaxed);
                CAPSULE_OK.store(ok3, Ordering::Relaxed);
                MIC_OK.store(ok4, Ordering::Relaxed);
            } else {
                TOGGLE_OK.store(true, Ordering::Relaxed);
                TIMER_OK.store(true, Ordering::Relaxed);
                CAPSULE_OK.store(true, Ordering::Relaxed);
                MIC_OK.store(true, Ordering::Relaxed);
            }
        }
    });
}

pub fn status() -> (bool, bool, bool, bool) {
    (
        TOGGLE_OK.load(Ordering::Relaxed),
        TIMER_OK.load(Ordering::Relaxed),
        CAPSULE_OK.load(Ordering::Relaxed),
        MIC_OK.load(Ordering::Relaxed),
    )
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_HOTKEY {
        let id = wparam.0 as i32;
        if id == HK_CAPSULE {
            APP.with(|c| {
                if let Some(app) = c.borrow().as_ref() {
                    crate::toggle_capsule_mode(app);
                }
            });
            return LRESULT(0);
        }
        if id == HK_MIC {
            APP.with(|c| {
                if let Some(app) = c.borrow().as_ref() {
                    crate::toggle_mic_mute_global(app);
                }
            });
            return LRESULT(0);
        }
        let action = match id {
            HK_TOGGLE => Some("toggle"),
            HK_TIMER => Some("timer"),
            _ => None,
        };
        if let Some(name) = action {
            APP.with(|c| {
                if let Some(app) = c.borrow().as_ref() {
                    let _ = app.emit("hotkey", name);
                }
            });
        }
        return LRESULT(0);
    }
    if msg == WM_SYNC_HOTKEYS {
        apply_registration(hwnd);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

pub fn sync() {
    let raw = HOTKEY_HWND.load(Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = SendMessageW(HWND(raw as _), WM_SYNC_HOTKEYS, Some(WPARAM(0)), Some(LPARAM(0)));
        }
    }
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        APP.with(|c| *c.borrow_mut() = Some(app.clone()));
        winmsg::run(w!("DynamicIslandHotkeys"), Some(wnd_proc), |hwnd| {
            HOTKEY_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
            unsafe { apply_registration(hwnd) };
            true
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_and_rejects_bare_keys() {
        assert_eq!(parse_hotkey("Ctrl+Alt+I"), Some(((MOD_NOREPEAT | MOD_CONTROL | MOD_ALT).0, b'I' as u32)));
        assert_eq!(parse_hotkey("Ctrl+Alt+M"), Some(((MOD_NOREPEAT | MOD_CONTROL | MOD_ALT).0, b'M' as u32)));
        assert_eq!(parse_hotkey("Ctrl+Shift+Space"), Some(((MOD_NOREPEAT | MOD_CONTROL | MOD_SHIFT).0, 0x20)));
        assert_eq!(parse_hotkey("Alt+F5"), Some(((MOD_NOREPEAT | MOD_ALT).0, 0x74)));
        assert_eq!(parse_hotkey("I"), None);
        assert_eq!(parse_hotkey("Ctrl+Unknown"), None);
    }
}
