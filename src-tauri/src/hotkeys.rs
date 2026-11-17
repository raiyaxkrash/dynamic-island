//! Глобальные горячие клавиши:
//! - `Ctrl + Alt + I` — раскрыть / свернуть остров
//! - `Ctrl + Alt + T` — запустить Помодоро 25 или поставить / снять паузу таймера

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};

use tauri::{AppHandle, Emitter, Manager};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
};
use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, PostMessageW, WM_APP, WM_HOTKEY};

use crate::{winmsg, Shared};

const HK_TOGGLE: i32 = 1;
const HK_TIMER: i32 = 2;
const WM_SYNC_HOTKEYS: u32 = WM_APP + 1;

static HOTKEY_HWND: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static APP: RefCell<Option<AppHandle>> = const { RefCell::new(None) };
}

unsafe fn apply_registration(hwnd: HWND, on: bool) {
    let _ = UnregisterHotKey(Some(hwnd), HK_TOGGLE);
    let _ = UnregisterHotKey(Some(hwnd), HK_TIMER);
    if !on {
        return;
    }
    let mods: HOT_KEY_MODIFIERS = MOD_CONTROL | MOD_ALT | MOD_NOREPEAT;
    if RegisterHotKey(Some(hwnd), HK_TOGGLE, mods, b'I' as u32).is_err() {
        APP.with(|c| {
            crate::log_err(c.borrow().as_ref(), "RegisterHotKey Ctrl+Alt+I failed");
        });
    }
    if RegisterHotKey(Some(hwnd), HK_TIMER, mods, b'T' as u32).is_err() {
        APP.with(|c| {
            crate::log_err(c.borrow().as_ref(), "RegisterHotKey Ctrl+Alt+T failed");
        });
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_HOTKEY {
        let action = match wparam.0 as i32 {
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
        apply_registration(hwnd, wparam.0 != 0);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

pub fn set_enabled(on: bool) {
    let raw = HOTKEY_HWND.load(Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(raw as _)), WM_SYNC_HOTKEYS, WPARAM(on as usize), LPARAM(0));
        }
    }
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        APP.with(|c| *c.borrow_mut() = Some(app.clone()));
        let enabled = crate::lock(&app.state::<Shared>().settings).hotkeys;
        winmsg::run(w!("DynamicIslandHotkeys"), Some(wnd_proc), |hwnd| {
            HOTKEY_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
            unsafe { apply_registration(hwnd, enabled) };
            true
        });
    });
}
