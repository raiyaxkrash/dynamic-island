//! Короткая подсказка «Скопировано» и оперативная история буфера обмена (только в ОЗУ).
//!
//! Использует системный слушатель `AddClipboardFormatListener` на скрытом message-only окне.
//! Приватные форматы менеджеров паролей не читаются и не попадают в историю.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Manager};
use windows::core::w;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, RemoveClipboardFormatListener, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, WM_CLIPBOARDUPDATE};

use crate::{publish, winmsg, Shared};

const CF_BITMAP: u32 = 2;
const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const MAX_CLIPS: usize = 6;

static SELF_COPY: AtomicBool = AtomicBool::new(false);

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

unsafe fn open_retry() -> bool {
    for _ in 0..5 {
        if OpenClipboard(None).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    false
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

fn format_snippet(s: &str) -> Option<String> {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    let short: String = flat.chars().take(60).collect();
    Some(if flat.chars().count() > 60 { format!("{short}…") } else { short })
}

/// Возвращает `(полный_текст_до_4000_символов, короткий_сниппет)`.
unsafe fn read_text_pair() -> Option<(String, String)> {
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
    let full = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
    let _ = GlobalUnlock(g);
    let snippet = format_snippet(&full)?;
    Some((full.trim().to_string(), snippet))
}

pub fn push_clip_entry(list: &mut Vec<String>, entry: String) -> bool {
    if entry.is_empty() {
        return false;
    }
    list.retain(|x| x != &entry);
    list.insert(0, entry);
    if list.len() > MAX_CLIPS {
        list.truncate(MAX_CLIPS);
    }
    true
}

/// Что сейчас в буфере: `(событие_для_HUD, текст_для_истории)`.
fn describe() -> (Option<serde_json::Value>, Option<String>) {
    unsafe {
        if available(exclude_fmt()) || !open_retry() {
            return (None, None);
        }
        let no_history = available(no_history_fmt()) && read_dword(no_history_fmt()) == Some(0);
        let out = if no_history || available(exclude_fmt()) {
            (None, None)
        } else if available(CF_HDROP) {
            let v = GetClipboardData(CF_HDROP)
                .ok()
                .map(|h| DragQueryFileW(HDROP(h.0), u32::MAX, None))
                .filter(|n| *n > 0)
                .map(|n| json!({ "kind": "files", "count": n }));
            (v, None)
        } else if available(CF_UNICODETEXT) {
            match read_text_pair() {
                Some((full, snip)) => (Some(json!({ "kind": "text", "text": snip })), Some(full)),
                None => (None, None),
            }
        } else if available(CF_DIB) || available(CF_BITMAP) {
            (Some(json!({ "kind": "image" })), None)
        } else {
            (None, None)
        };
        let _ = CloseClipboard();
        out
    }
}

/// Записывает строку в системный буфер обмена (не вызывая повторный HUD-тост острова).
pub fn write_text(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    unsafe {
        if !open_retry() {
            return false;
        }
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = wide.len() * std::mem::size_of::<u16>();
        let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, bytes) else {
            let _ = CloseClipboard();
            return false;
        };
        let dst = GlobalLock(hmem) as *mut u16;
        if dst.is_null() {
            let _ = GlobalFree(Some(hmem));
            let _ = CloseClipboard();
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
        let _ = GlobalUnlock(hmem);
        let _ = EmptyClipboard();
        SELF_COPY.store(true, Ordering::Relaxed);
        let ok = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hmem.0))).is_ok();
        if !ok {
            SELF_COPY.store(false, Ordering::Relaxed);
            let _ = GlobalFree(Some(hmem));
        }
        let _ = CloseClipboard();
        ok
    }
}

/// Читает список скопированных в Проводнике файлов (`CF_HDROP`) для добавления на полку.
pub fn read_files() -> Vec<PathBuf> {
    unsafe {
        if !available(CF_HDROP) || !open_retry() {
            return Vec::new();
        }
        let mut paths = Vec::new();
        if let Ok(h) = GetClipboardData(CF_HDROP) {
            let hdrop = HDROP(h.0);
            let count = DragQueryFileW(hdrop, u32::MAX, None).min(16);
            for i in 0..count {
                let len = DragQueryFileW(hdrop, i, None);
                if len == 0 || len > 4096 {
                    continue;
                }
                let mut buf = vec![0u16; len as usize + 1];
                let got = DragQueryFileW(hdrop, i, Some(&mut buf));
                if got > 0 {
                    let s = String::from_utf16_lossy(&buf[..got as usize]);
                    paths.push(PathBuf::from(s));
                }
            }
        }
        let _ = CloseClipboard();
        paths
    }
}

fn handle_clipboard_change(app: &AppHandle) {
    if SELF_COPY.swap(false, Ordering::Relaxed) {
        return;
    }
    if !crate::lock(&app.state::<Shared>().settings).clipboard {
        return;
    }
    std::thread::sleep(Duration::from_millis(60));
    let (hud, full_text) = describe();
    if let Some(txt) = full_text {
        let items = {
            let shared = app.state::<Shared>();
            let mut clips = crate::lock(&shared.clips);
            push_clip_entry(&mut clips, txt);
            clips.clone()
        };
        publish(app, "clips", json!(items));
    }
    if let Some(v) = hud {
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
        assert_eq!(format_snippet("  hello \n\t world  ").as_deref(), Some("hello world"));
        assert_eq!(format_snippet("   \n\t "), None);
        let res = format_snippet(&"a".repeat(70)).unwrap();
        assert_eq!(res.chars().count(), 61);
        assert!(res.ends_with('…'));
    }

    #[test]
    fn clip_history_deduplicates_and_caps() {
        let mut list = Vec::new();
        for i in 1..=8 {
            push_clip_entry(&mut list, format!("item-{i}"));
        }
        assert_eq!(list.len(), MAX_CLIPS);
        assert_eq!(list[0], "item-8");
        push_clip_entry(&mut list, "item-5".to_string());
        assert_eq!(list[0], "item-5");
        assert_eq!(list.len(), MAX_CLIPS);
    }
}
