//! Следит за курсором и окнами под островом.
//!
//! - Сообщает интерфейсу, что курсор над островом (`hover`). Мышь окно ловит, только когда
//!   интерфейс разрешил (`interactive`): пока остров не раскрыт, клики проходят сквозь него,
//!   например на вкладки браузера.
//! - Замечает развёрнутое окно, чей верх под островом (`topbusy`): тогда остров ужимается
//!   в тонкую полоску, чтобы не закрывать вкладки и заголовки.
//! - Прячет остров в полноэкранных приложениях (`fullscreen`).

use std::sync::atomic::Ordering;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, IsIconic, IsZoomed,
    GWL_EXSTYLE, WS_EX_TOPMOST,
};

use crate::Shared;

fn fullscreen_app() -> bool {
    match unsafe { SHQueryUserNotificationState() } {
        Ok(s) => s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE,
        Err(_) => false,
    }
}

/// Верх активного окна упирается в верх экрана там, где стоит остров?
fn top_covered(own: &WebviewWindow) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || IsIconic(fg).as_bool() {
            return false;
        }
        let Ok(h) = own.hwnd() else { return false };
        let own_hwnd = HWND(h.0 as _);
        if own_hwnd.0 as isize == fg.0 as isize {
            return false;
        }
        // Рабочий стол и панель задач не в счёт.
        let mut buf = [0u16; 64];
        let n = GetClassNameW(fg, &mut buf) as usize;
        let class = String::from_utf16_lossy(&buf[..n]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return false;
        }
        // Развёрнутое окно на другом мониторе верх основного экрана не закрывает.
        let own_mon = MonitorFromWindow(own_hwnd, MONITOR_DEFAULTTONEAREST);
        let fg_mon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        if fg_mon != own_mon {
            return false;
        }
        if IsZoomed(fg).as_bool() {
            return true;
        }
        covers_island_span(fg, own_hwnd, fg_mon)
    }
}

unsafe fn covers_island_span(hwnd: HWND, own_hwnd: HWND, mon: windows::Win32::Graphics::Gdi::HMONITOR) -> bool {
    let mut r = RECT::default();
    if GetWindowRect(hwnd, &mut r).is_err() {
        return false;
    }
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !GetMonitorInfoW(mon, &mut mi).as_bool() {
        return false;
    }
    let m = mi.rcMonitor;
    let mut own_r = RECT::default();
    let island_cx = if GetWindowRect(own_hwnd, &mut own_r).is_ok() && own_r.right > own_r.left {
        (own_r.left + own_r.right) / 2
    } else {
        (m.left + m.right) / 2
    };
    // Окно начинается у самого верха (с небольшим допуском на рамку) и проходит под островом.
    r.top <= m.top + 12 && r.left < island_cx + 150 && r.right > island_cx - 150
}

/// Окно потеряло «поверх всех»? Проверяем стиль напрямую: повторный SetWindowPos
/// без нужды перестраивает окно и даёт заметную заминку анимации.
fn lost_topmost(win: &WebviewWindow) -> bool {
    let Ok(h) = win.hwnd() else { return false };
    let style = unsafe { GetWindowLongPtrW(HWND(h.0 as _), GWL_EXSTYLE) } as u32;
    style & WS_EX_TOPMOST.0 == 0
}

fn current_monitor(win: &WebviewWindow) -> Option<tauri::Monitor> {
    win.current_monitor().ok().flatten().or_else(|| win.primary_monitor().ok().flatten())
}

fn monitor_at_point(win: &WebviewWindow, x: i32, y: i32) -> Option<tauri::Monitor> {
    win.available_monitors().ok()?.into_iter().find(|m| {
        let p = m.position();
        let s = m.size();
        x >= p.x && x < p.x + s.width as i32 && y >= p.y && y < p.y + s.height as i32
    })
}

fn mon_key(m: &tauri::Monitor) -> (i32, i32, u32, u32, i32, f64) {
    (
        m.position().x,
        m.position().y,
        m.size().width,
        m.size().height,
        m.work_area().position.y,
        m.scale_factor(),
    )
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        let mut inside = false;
        let mut capture = false;
        let mut full = false;
        let mut busy = false;
        let mut tick: u32 = 0;
        // Позиция и масштаб окна почти не меняются: спрашиваем их редко, а не 40 раз в секунду,
        // чтобы не дёргать главный поток, который в это время рисует анимацию.
        let mut geom: Option<(tauri::PhysicalPosition<i32>, f64)> = None;
        let mut screen: Option<(i32, i32, u32, u32, i32, f64)> = None;
        loop {
            let sleep_ms = if full { 200 } else { 25 };
            std::thread::sleep(Duration::from_millis(sleep_ms));
            tick = tick.wrapping_add(if full { 8 } else { 1 });
            let Some(win) = app.get_webview_window("island") else { continue };
            if !win.is_visible().unwrap_or(true) {
                std::thread::sleep(Duration::from_millis(225));
                continue;
            }

            let mut p = POINT::default();
            let has_cursor = unsafe { GetCursorPos(&mut p) }.is_ok();
            let shared = app.state::<Shared>();
            let interactive = shared.interactive.load(Ordering::Relaxed);
            let dirty = shared.geom_dirty.swap(false, Ordering::Relaxed);
            let (follow_cursor, align) = {
                let s = crate::lock(&shared.settings);
                (s.monitor_mode == "cursor", s.align.clone())
            };

            if dirty || geom.is_none() || tick % 80 == 0 || (follow_cursor && tick % 12 == 0 && !inside && !interactive && has_cursor) {
                let target_mon = if follow_cursor && has_cursor {
                    monitor_at_point(&win, p.x, p.y).or_else(|| current_monitor(&win))
                } else {
                    win.primary_monitor().ok().flatten()
                };
                if let Some(m) = target_mon {
                    let now = mon_key(&m);
                    if dirty || screen != Some(now) {
                        let had_screen = screen.is_some();
                        crate::place_on(&win, &m, &align);
                        if had_screen && !dirty {
                            let _ = app.emit("screen", ());
                        }
                        screen = Some(now);
                        if let (Ok(pos), Ok(scale)) = (win.outer_position(), win.scale_factor()) {
                            geom = Some((pos, scale));
                        }
                    }
                }
                if dirty || geom.is_none() || tick % 80 == 0 {
                    if let (Ok(pos), Ok(scale)) = (win.outer_position(), win.scale_factor()) {
                        geom = Some((pos, scale));
                    }
                }
            }
            if tick % 10 == 0 {
                let b = top_covered(&win);
                if b != busy {
                    busy = b;
                    let _ = app.emit("topbusy", b);
                }
            }
            if tick % 40 == 0 {
                let f = fullscreen_app();
                if f != full {
                    full = f;
                    let _ = app.emit("fullscreen", f);
                }
                if !full && lost_topmost(&win) {
                    let _ = win.set_always_on_top(true);
                }
            }

            if !has_cursor {
                continue;
            }
            let Some((pos, scale)) = geom else { continue };
            let r = *crate::lock(&shared.rect);
            let x0 = pos.x as f64 + r[0] * scale;
            let y0 = pos.y as f64 + r[1] * scale;
            let x1 = x0 + r[2] * scale;
            let y1 = y0 + r[3] * scale;
            let (px, py) = (p.x as f64, p.y as f64);
            let now = !full && r[2] > 0.0 && px >= x0 && px <= x1 && py >= y0 && py <= y1;

            if now != inside {
                inside = now;
                let _ = app.emit("hover", now);
            }
            let want = inside && interactive;
            if want != capture {
                capture = want;
                let _ = win.set_ignore_cursor_events(!want);
            }
        }
    });
}
