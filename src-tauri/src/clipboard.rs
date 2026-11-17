//! Короткая подсказка «Скопировано», когда в буфер обмена что-то попало.
//!
//! Использует системный слушатель `AddClipboardFormatListener` на скрытом message-only окне
//! (без постоянного опроса). При сбое регистрации откатывается на редкий опрос последовательности.
//! Приватные форматы менеджеров паролей не читаются вовсе.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Manager};
use windows::core::w;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, RemoveClipboardFormatListener,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, WM_CLIPBOARDUPDATE};

use crate::{publish, winmsg, Shared};

const CF_BITMAP: u32 = 2;
const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;

thread_local! {
    static APP_HANDLE: RefCell<Option<AppHandle>> = const { RefCell::new(None) };
}

fn exclude_fmt() -> u32 {
    static FMT: OnceLock<u32> = OnceLock::new();
    *FMT.get_or_init(|| unsafe { RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")) })
}

fn no_history_fmt() -> u32 {
    static FMT: OnceLock<u32> = OnceLock::new();
    *FMT.get_or_init(|| unsafe { RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")) })
}

unsafe fn available(fmt: u32) -> bool {
    fmt != 0 && IsClipboardFormatAvailable(fmt).is_ok()
}

unsafe fn read_dword(fmt: u32) -> Option<u32> {
    let h: HANDLE = GetClipboardData(fmt).ok()?;
    let g = HGLOBAL(h.0);
    if GlobalSize(g) < std::mem::size_of::<u32>() {
        return None;
    }
    let p = GlobalLock(g) as *const u32;
    if p.is_null() {
        return None;
    }
    let val = std::ptr::read_unaligned(p);
    let _ = GlobalUnlock(g);
    Some(val)
}

fn format_snippet(units: &[u16]) -> Option<String> {
    let s = String::from_utf16_lossy(units);
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    let short: String = flat.chars().take(60).collect();
    Some(if flat.chars().count() > 60 { format!("{short}…") } else { short })
}

unsafe fn read_text() -> Option<String> {
    let h: HANDLE = GetClipboardData(CF_UNICODETEXT).ok()?;
    let g = HGLOBAL(h.0);
    let max_units = (GlobalSize(g) / std::mem::size_of::<u16>()).min(4000);
    if max_units == 0 {
        return None;
    }
    let p = GlobalLock(g) as *const u16;
    if p.is_null() {
        return None;
    }
    let mut len = 0usize;
    while len < max_units && *p.add(len) != 0 {
        len += 1;
    }
    let out = format_snippet(std::slice::from_raw_parts(p, len));
    let _ = GlobalUnlock(g);
    out
}

/// Что сейчас в буфере: текст, картинка или файлы. `None`, если читать нельзя или нечего.
fn describe() -> Option<serde_json::Value> {
    unsafe {
        if available(exclude_fmt()) {
            return None;
        }
        let mut opened = false;
        for _ in 0..5 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        if !opened {
            return None;
        }
        let no_history = available(no_history_fmt()) && read_dword(no_history_fmt()) == Some(0);
        let v = if no_history || available(exclude_fmt()) {
            None
        } else if available(CF_HDROP) {
            GetClipboardData(CF_HDROP)
                .ok()
                .map(|h| DragQueryFileW(HDROP(h.0), u32::MAX, None))
                .filter(|n| *n > 0)
                .map(|n| json!({ "kind": "files", "count": n }))
        } else if available(CF_UNICODETEXT) {
            read_text().map(|t| json!({ "kind": "text", "text": t }))
        } else if available(CF_DIB) || available(CF_BITMAP) {
            Some(json!({ "kind": "image" }))
        } else {
            None
        };
        let _ = CloseClipboard();
        v
    }
}

fn handle_clipboard_change(app: &AppHandle) {
    if !crate::lock(&app.state::<Shared>().settings).clipboard {
        return;
    }
    // Даём программе-источнику дописать все форматы.
    std::thread::sleep(Duration::from_millis(60));
    if let Some(v) = describe() {
        publish(app, "clipboard", v);
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        APP_HANDLE.with(|cell| {
            if let Some(app) = cell.borrow().as_ref() {
                handle_clipboard_change(app);
            }
        });
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        APP_HANDLE.with(|cell| *cell.borrow_mut() = Some(app.clone()));
        let mut reg_hwnd = HWND::default();
        let ok = winmsg::run(w!("DynamicIslandClipboardListener"), Some(wnd_proc), |hwnd| {
            reg_hwnd = hwnd;
            unsafe { AddClipboardFormatListener(hwnd).is_ok() }
        });
        if ok {
            unsafe {
                let _ = RemoveClipboardFormatListener(reg_hwnd);
            }
            return;
        }
        crate::log_err(Some(&app), "AddClipboardFormatListener failed, using fallback polling");
        let mut last = unsafe { GetClipboardSequenceNumber() };
        loop {
            std::thread::sleep(Duration::from_millis(400));
            let seq = unsafe { GetClipboardSequenceNumber() };
            if seq == last {
                continue;
            }
            last = seq;
            handle_clipboard_change(&app);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_collapses_whitespace_and_truncates() {
        let units: Vec<u16> = "  hello \n\t world  ".encode_utf16().collect();
        assert_eq!(format_snippet(&units).as_deref(), Some("hello world"));

        let empty: Vec<u16> = "   \n\t ".encode_utf16().collect();
        assert_eq!(format_snippet(&empty), None);

        let long: Vec<u16> = "a".repeat(70).encode_utf16().collect();
        let res = format_snippet(&long).unwrap();
        assert_eq!(res.chars().count(), 61);
        assert!(res.ends_with('…'));
    }
}
