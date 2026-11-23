//! Короткая подсказка «Скопировано» и оперативная история буфера обмена (текст + скриншоты в ОЗУ).
//!
//! Использует системный слушатель `AddClipboardFormatListener` на скрытом message-only окне.
//! Приватные форматы менеджеров паролей не читаются и не попадают в историю.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use serde::Serialize;
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

use crate::{publish, shelf, wic, winmsg, Shared};

const CF_BITMAP: u32 = 2;
const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const MAX_CLIPS: usize = 6;
const THUMB_PX: u32 = 80;

static SELF_COPY: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Serialize, Clone, Debug)]
pub struct ClipItem {
    pub id: u64,
    pub kind: &'static str, // "text" | "image"
    pub text: String,
    pub preview: Option<String>,
    pub pinned: bool,
    #[serde(skip)]
    pub png: Option<Vec<u8>>,
}

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

fn png_fmt() -> u32 {
    static FMT: OnceLock<u32> = OnceLock::new();
    *FMT.get_or_init(|| unsafe { RegisterClipboardFormatW(w!("PNG")) })
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

unsafe fn read_raw_bytes(fmt: u32, max_bytes: usize) -> Option<Vec<u8>> {
    let h: HANDLE = GetClipboardData(fmt).ok()?;
    let g = HGLOBAL(h.0);
    let size = GlobalSize(g);
    if size == 0 || size > max_bytes {
        return None;
    }
    let p = GlobalLock(g) as *const u8;
    if p.is_null() {
        return None;
    }
    let out = std::slice::from_raw_parts(p, size).to_vec();
    let _ = GlobalUnlock(g);
    Some(out)
}

/// Оборачивает `CF_DIB` (`BITMAPINFOHEADER` + пиксели) в валидный BMP-буфер для декодера WIC.
pub fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let bi_size = u32::from_le_bytes(dib[0..4].try_into().ok()?) as usize;
    if bi_size < 40 || bi_size > dib.len() {
        return None;
    }
    let bit_count = u16::from_le_bytes(dib[14..16].try_into().ok()?);
    let compression = u32::from_le_bytes(dib[16..20].try_into().ok()?);
    let clr_used = u32::from_le_bytes(dib[32..36].try_into().ok()?) as usize;

    let extra = if bi_size == 40 {
        if compression == 3 {
            12
        } else if compression == 6 {
            16
        } else if bit_count > 0 && bit_count <= 8 {
            let colors = if clr_used > 0 { clr_used } else { 1usize << bit_count };
            colors * 4
        } else {
            0
        }
    } else if bit_count > 0 && bit_count <= 8 {
        let colors = if clr_used > 0 { clr_used } else { 1usize << bit_count };
        colors * 4
    } else {
        0
    };

    let off_bits = (14 + bi_size + extra) as u32;
    let file_size = (14 + dib.len()) as u32;
    let mut bmp = Vec::with_capacity(14 + dib.len());
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&off_bits.to_le_bytes());
    bmp.extend_from_slice(dib);
    Some(bmp)
}

/// Декодирует изображение (PNG или BMP) через WIC и возвращает `(полный_PNG, миниатюра_data_url, w, h)`.
pub fn decode_to_png_and_thumb(raw: &[u8], already_png: bool) -> Option<(Vec<u8>, String, u32, u32)> {
    wic::decode_to_png_and_thumb(raw, already_png, THUMB_PX)
}

/// Конвертирует PNG в 32-битный `CF_DIB` (`BITMAPINFOHEADER` + BGRA top-down) для классических Win32-приложений.
fn png_to_dib(png: &[u8]) -> Option<Vec<u8>> {
    wic::png_to_dib(png)
}

fn format_snippet(s: &str) -> Option<String> {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    let short: String = flat.chars().take(60).collect();
    Some(if flat.chars().count() > 60 { format!("{short}…") } else { short })
}

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

unsafe fn read_image_item() -> Option<ClipItem> {
    let decoded = if available(png_fmt()) {
        read_raw_bytes(png_fmt(), 12 * 1024 * 1024)
            .and_then(|b| decode_to_png_and_thumb(&b, true))
    } else if available(CF_DIB) {
        read_raw_bytes(CF_DIB, 24 * 1024 * 1024)
            .and_then(|dib| dib_to_bmp(&dib))
            .and_then(|bmp| decode_to_png_and_thumb(&bmp, false))
    } else {
        None
    }?;
    let (png, preview, w, h) = decoded;
    Some(ClipItem {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        kind: "image",
        text: format!("{w}×{h}"),
        preview: Some(preview),
        pinned: false,
        png: Some(png),
    })
}

fn sort_pinned_first(list: &mut [ClipItem]) {
    list.sort_by_key(|item| !item.pinned);
}

pub fn push_clip_item(list: &mut Vec<ClipItem>, mut item: ClipItem) -> bool {
    if item.kind == "text" && item.text.is_empty() {
        return false;
    }
    if item.kind == "text" {
        if let Some(pos) = list.iter().position(|x| x.kind == "text" && x.text == item.text) {
            item.pinned = list[pos].pinned;
            list.remove(pos);
        }
    } else if let Some(new_png) = item.png.as_ref() {
        if list
            .iter()
            .any(|old| old.png.as_ref().is_some_and(|b| b == new_png))
        {
            return false;
        }
    }
    list.insert(0, item);
    sort_pinned_first(list);
    // Не вытесняем закреплённые записи: обрезаем только незакреплённый хвост сверх MAX_CLIPS.
    while list.len() > MAX_CLIPS {
        if let Some(idx) = list.iter().rposition(|x| !x.pinned) {
            list.remove(idx);
        } else {
            break;
        }
    }
    true
}

pub fn toggle_pin(list: &mut [ClipItem], id: u64) -> bool {
    let Some(item) = list.iter_mut().find(|x| x.id == id) else {
        return false;
    };
    item.pinned = !item.pinned;
    sort_pinned_first(list);
    true
}

fn describe() -> (Option<serde_json::Value>, Option<ClipItem>) {
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
        } else {
            let text_res = if available(CF_UNICODETEXT) {
                read_text_pair()
            } else {
                None
            };
            if let Some((full, snip)) = text_res {
                (
                    Some(json!({ "kind": "text", "text": snip })),
                    Some(ClipItem {
                        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                        kind: "text",
                        text: full,
                        preview: None,
                        pinned: false,
                        png: None,
                    }),
                )
            } else if available(png_fmt()) || available(CF_DIB) || available(CF_BITMAP) {
                let img = read_image_item();
                (Some(json!({ "kind": "image" })), img)
            } else {
                (None, None)
            }
        };
        let _ = CloseClipboard();
        out
    }
}

unsafe fn set_clipboard_bytes(fmt: u32, bytes: &[u8]) -> bool {
    let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else {
        return false;
    };
    let dst = GlobalLock(hmem) as *mut u8;
    if dst.is_null() {
        let _ = GlobalFree(Some(hmem));
        return false;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
    let _ = GlobalUnlock(hmem);
    if SetClipboardData(fmt, Some(HANDLE(hmem.0))).is_err() {
        let _ = GlobalFree(Some(hmem));
        return false;
    }
    true
}

/// Записывает строку в системный буфер обмена (не вызывая повторный HUD острова).
pub fn write_text(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    unsafe {
        if !open_retry() {
            return false;
        }
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
        let _ = EmptyClipboard();
        SELF_COPY.store(true, Ordering::Relaxed);
        let ok = set_clipboard_bytes(CF_UNICODETEXT, bytes);
        if !ok {
            SELF_COPY.store(false, Ordering::Relaxed);
        }
        let _ = CloseClipboard();
        ok
    }
}

/// Записывает скопированное изображение обратно в системный буфер (`PNG` + `CF_DIB`).
pub fn write_image(png: &[u8]) -> bool {
    if png.is_empty() {
        return false;
    }
    let dib = png_to_dib(png);
    unsafe {
        if !open_retry() {
            return false;
        }
        let _ = EmptyClipboard();
        SELF_COPY.store(true, Ordering::Relaxed);
        let mut ok = false;
        let pfmt = png_fmt();
        if pfmt != 0 {
            ok |= set_clipboard_bytes(pfmt, png);
        }
        if let Some(d) = dib.as_deref() {
            ok |= set_clipboard_bytes(CF_DIB, d);
        }
        if !ok {
            SELF_COPY.store(false, Ordering::Relaxed);
        }
        let _ = CloseClipboard();
        ok
    }
}

fn temp_clip_dir() -> PathBuf {
    std::env::temp_dir().join("DynamicIslandClips")
}

/// Очищает временные PNG-файлы от предыдущих перетаскиваний скриншотов.
pub fn clear_temp_files() {
    let dir = temp_clip_dir();
    if dir.exists() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Запускает нативный OLE Drag & Drop для скриншота из ОЗУ как `.png`-файла.
pub fn drag_image(id: u64, png: &[u8]) {
    if png.is_empty() {
        return;
    }
    let dir = temp_clip_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let file = dir.join(format!("screenshot-{id}.png"));
    if std::fs::write(&file, png).is_ok() {
        shelf::start_drag(file.to_string_lossy().to_string());
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
    let (hud, entry) = describe();
    if let Some(item) = entry {
        let items = {
            let shared = app.state::<Shared>();
            let mut clips = crate::lock(&shared.clips);
            push_clip_item(&mut clips, item);
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
    clear_temp_files();
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
            push_clip_item(
                &mut list,
                ClipItem {
                    id: i,
                    kind: "text",
                    text: format!("item-{i}"),
                    preview: None,
                    pinned: false,
                    png: None,
                },
            );
        }
        assert_eq!(list.len(), MAX_CLIPS);
        assert_eq!(list[0].text, "item-8");
        push_clip_item(
            &mut list,
            ClipItem {
                id: 99,
                kind: "text",
                text: "item-5".to_string(),
                preview: None,
                pinned: false,
                png: None,
            },
        );
        assert_eq!(list[0].text, "item-5");
        assert_eq!(list.len(), MAX_CLIPS);

        // Закрепление переносит запись наверх и защищает от вытеснения новыми копированиями.
        assert!(toggle_pin(&mut list, 6));
        assert!(list[0].pinned);
        assert_eq!(list[0].text, "item-6");
        for i in 100..108 {
            push_clip_item(
                &mut list,
                ClipItem {
                    id: i,
                    kind: "text",
                    text: format!("new-{i}"),
                    preview: None,
                    pinned: false,
                    png: None,
                },
            );
        }
        assert!(list.iter().any(|x| x.text == "item-6" && x.pinned));
        assert_eq!(list.len(), MAX_CLIPS);
    }

    #[test]
    fn dib_converts_to_valid_png_and_roundtrips() {
        // 4x4 32-bit DIB
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&4i32.to_le_bytes());
        dib.extend_from_slice(&4i32.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&0u32.to_le_bytes());
        dib.extend_from_slice(&64u32.to_le_bytes());
        dib.extend_from_slice(&[0u8; 16]);
        dib.extend_from_slice(&[180u8; 64]);

        let bmp = dib_to_bmp(&dib).expect("dib_to_bmp");
        let (png, preview, w, h) = decode_to_png_and_thumb(&bmp, false).expect("wic encode");
        assert_eq!((w, h), (4, 4));
        assert!(png.starts_with(b"\x89PNG"));
        assert!(preview.starts_with("data:image/png;base64,"));
        let round_dib = png_to_dib(&png).expect("png_to_dib");
        assert_eq!(round_dib.len(), 40 + 64);
    }
}
