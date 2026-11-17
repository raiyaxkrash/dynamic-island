//! Следит за курсором, Z-порядком и окнами под островом.
//!
//! - Сообщает интерфейсу, что курсор над островом (`hover`). Мышь окно ловит, только когда
//!   интерфейс разрешил (`interactive`): пока остров не раскрыт, клики проходят сквозь него,
//!   например на вкладки браузера.
//! - Замечает развёрнутое окно, чей верх под островом (`topbusy`): тогда остров ужимается
//!   в тонкую полоску (или ведёт себя согласно настройке `over_maximized`), не закрывая вкладки.
//! - Прячет остров только в настоящих полноэкранных играх и видео (`fullscreen`), не путая их
//!   с обычными развёрнутыми окнами при автоскрытии панели задач или режиме «Не беспокоить».
//! - Удерживает окно на самой вершине Z-порядка `HWND_TOPMOST` (в том числе после `Win + D`,
//!   смены окон, сна/разблокировки и сторонних topmost-окон) без кражи фокуса и рывков анимации.

use std::sync::atomic::Ordering;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetClientRect, GetCursorPos, GetForegroundWindow, GetWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, SetWindowPos,
    ShowWindow, GWL_EXSTYLE, GWL_STYLE, GW_HWNDPREV, HWND_TOPMOST, SWP_ASYNCWINDOWPOS,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSENDCHANGING, SWP_NOSIZE,
    SW_SHOWNOACTIVATE, WS_CAPTION, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT,
};

use crate::Shared;

unsafe fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf) as usize;
    String::from_utf16_lossy(&buf[..n])
}

unsafe fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        (&mut cloaked as *mut u32).cast(),
        std::mem::size_of::<u32>() as u32,
    )
    .is_ok()
        && cloaked != 0
}

fn is_shell_or_transient_class(cls: &str) -> bool {
    matches!(
        cls,
        "Progman"
            | "WorkerW"
            | "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "XamlExplorerHostIslandWindow"
            | "MultitaskingViewFrame"
            | "TaskListThumbnailWnd"
            | "ForegroundStaging"
            | "Windows.UI.Core.CoreWindow"
            | "#32768"
            | "tooltips_class32"
            | "Xaml_WindowedPopupClass"
    )
}

/// Настоящее полноэкранное приложение (игра, F11, видео во весь экран) на мониторе острова.
///
/// Нельзя полагаться на `QUNS_BUSY`: в Windows 10/11 этот код возвращается при включённой
/// «Фокусировке внимания» и для любого обычного развёрнутого окна при автоскрытии панели задач.
fn fullscreen_app(own: &WebviewWindow) -> bool {
    unsafe {
        let Ok(h) = own.hwnd() else { return false };
        let own_hwnd = HWND(h.0 as _);
        let fg = GetForegroundWindow();
        if fg.is_invalid()
            || fg.0 == own_hwnd.0
            || !IsWindowVisible(fg).as_bool()
            || IsIconic(fg).as_bool()
            || IsZoomed(fg).as_bool()
            || is_cloaked(fg)
        {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return false;
        }
        let cls = class_name(fg);
        if is_shell_or_transient_class(&cls) {
            return false;
        }
        let own_mon = MonitorFromWindow(own_hwnd, MONITOR_DEFAULTTONEAREST);
        let fg_mon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        if fg_mon != own_mon {
            return false;
        }
        if matches!(
            SHQueryUserNotificationState(),
            Ok(QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE)
        ) {
            return true;
        }
        let ex = GetWindowLongPtrW(fg, GWL_EXSTYLE) as u32;
        if ex & (WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) != 0 {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(fg_mon, &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        if r.left > m.left || r.top > m.top || r.right < m.right || r.bottom < m.bottom {
            return false;
        }
        let style = GetWindowLongPtrW(fg, GWL_STYLE) as u32;
        if style & WS_CAPTION.0 == 0
            || matches!(cls.as_str(), "ApplicationFrameWindow" | "CASCADIA_HOSTING_WINDOW_CLASS")
        {
            return true;
        }
        // Окна с WS_CAPTION, выносящие заголовок за верхнюю границу экрана (например GLFW/SDL/OpenGL).
        let mut cr = RECT::default();
        if GetClientRect(fg, &mut cr).is_ok() {
            let mut tl = POINT { x: cr.left, y: cr.top };
            let mut br = POINT { x: cr.right, y: cr.bottom };
            if ClientToScreen(fg, &mut tl).as_bool() && ClientToScreen(fg, &mut br).as_bool() {
                return tl.x <= m.left && tl.y <= m.top && br.x >= m.right && br.y >= m.bottom;
            }
        }
        false
    }
}

/// Верх активного окна упирается в верх экрана там, где стоит остров?
fn top_covered(own: &WebviewWindow) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid()
            || !IsWindowVisible(fg).as_bool()
            || IsIconic(fg).as_bool()
            || is_cloaked(fg)
        {
            return false;
        }
        let Ok(h) = own.hwnd() else { return false };
        let own_hwnd = HWND(h.0 as _);
        if own_hwnd.0 == fg.0 {
            return false;
        }
        let cls = class_name(fg);
        if is_shell_or_transient_class(&cls) {
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

unsafe fn covers_island_span(
    hwnd: HWND,
    own_hwnd: HWND,
    mon: windows::Win32::Graphics::Gdi::HMONITOR,
) -> bool {
    let mut r = RECT::default();
    if GetWindowRect(hwnd, &mut r).is_err() {
        return false;
    }
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
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

/// Поднимает окно острова на самую вершину группы `HWND_TOPMOST` без кражи фокуса и пересборки рамки.
pub(crate) fn raise_topmost(win: &WebviewWindow) {
    let Ok(h) = win.hwnd() else { return };
    unsafe {
        let _ = SetWindowPos(
            HWND(h.0 as _),
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_ASYNCWINDOWPOS
                | SWP_NOMOVE
                | SWP_NOSIZE
                | SWP_NOACTIVATE
                | SWP_NOOWNERZORDER
                | SWP_NOSENDCHANGING,
        );
    }
}

/// Проверяет, не сбросился ли флаг `WS_EX_TOPMOST` и не оказалось ли выше острова в Z-порядке
/// другое видимое окно (например `WorkerW` после `Win + D` или стороннее topmost-окно), перекрывающее его зону.
fn needs_raise_topmost(win: &WebviewWindow, interactive: bool) -> bool {
    let Ok(h) = win.hwnd() else { return false };
    unsafe {
        let own_hwnd = HWND(h.0 as _);
        let style = GetWindowLongPtrW(own_hwnd, GWL_EXSTYLE) as u32;
        if style & WS_EX_TOPMOST.0 == 0 {
            return true;
        }
        let mut own_r = RECT::default();
        if GetWindowRect(own_hwnd, &mut own_r).is_err() || own_r.right <= own_r.left {
            return false;
        }
        let band_bottom = if interactive {
            own_r.bottom
        } else {
            (own_r.top + 64).min(own_r.bottom)
        };
        let own_pid = GetCurrentProcessId();
        let mut cur = own_hwnd;
        for _ in 0..64 {
            let Ok(prev) = GetWindow(cur, GW_HWNDPREV) else { break };
            if prev.is_invalid() || prev.0.is_null() {
                break;
            }
            cur = prev;
            if !IsWindowVisible(cur).as_bool() || is_cloaked(cur) {
                continue;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(cur, Some(&mut pid));
            if pid == own_pid {
                continue;
            }
            let cls = class_name(cur);
            // Системные меню, подсказки и всплывающие панели оболочки Windows (Пуск, Alt+Tab, Win+Tab) не перебиваем.
            if matches!(
                cls.as_str(),
                "#32768"
                    | "tooltips_class32"
                    | "Xaml_WindowedPopupClass"
                    | "Windows.UI.Core.CoreWindow"
                    | "XamlExplorerHostIslandWindow"
                    | "MultitaskingViewFrame"
                    | "TaskListThumbnailWnd"
                    | "ForegroundStaging"
            ) {
                continue;
            }
            let mut r = RECT::default();
            if GetWindowRect(cur, &mut r).is_err() || r.right <= r.left || r.bottom <= r.top {
                continue;
            }
            if r.left < own_r.right
                && r.right > own_r.left
                && r.top < band_bottom
                && r.bottom > own_r.top
            {
                return true;
            }
        }
        false
    }
}

fn current_monitor(win: &WebviewWindow) -> Option<tauri::Monitor> {
    win.current_monitor()
        .ok()
        .flatten()
        .or_else(|| win.primary_monitor().ok().flatten())
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
        let mut last_fg: isize = 0;
        // Позиция и масштаб окна почти не меняются: спрашиваем их редко, а не 40 раз в секунду,
        // чтобы не дёргать главный поток, который в это время рисует анимацию.
        let mut geom: Option<(tauri::PhysicalPosition<i32>, f64)> = None;
        let mut screen: Option<(i32, i32, u32, u32, i32, f64)> = None;
        let mut was_lmb = false;
        let mut dragging_from_outside = false;
        let mut drag_dwell_ticks: u8 = 0;
        loop {
            let sleep_ms = if full { 150 } else { 25 };
            std::thread::sleep(Duration::from_millis(sleep_ms));
            tick = tick.wrapping_add(if full { 5 } else { 1 });
            let Some(win) = app.get_webview_window("island") else { continue };
            let shared = app.state::<Shared>();
            if shared.user_hidden.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            // Защита от непроизвольного сворачивания или скрытия окна системой (Win+D, Win+M, смена экрана).
            if let Ok(h) = win.hwnd() {
                let hwnd = HWND(h.0 as _);
                let iconic = unsafe { IsIconic(hwnd).as_bool() };
                let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
                if iconic || !visible {
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    }
                    shared.geom_dirty.store(true, Ordering::Relaxed);
                    raise_topmost(&win);
                }
            }

            let mut p = POINT::default();
            let has_cursor = unsafe { GetCursorPos(&mut p) }.is_ok();
            let interactive = shared.interactive.load(Ordering::Relaxed);
            let dirty = shared.geom_dirty.swap(false, Ordering::Relaxed);
            let (follow_cursor, align) = {
                let s = crate::lock(&shared.settings);
                (s.monitor_mode == "cursor", s.align.clone())
            };

            if dirty
                || geom.is_none()
                || tick % 80 == 0
                || (follow_cursor && tick % 12 == 0 && !inside && !interactive && has_cursor)
            {
                let target_mon = if follow_cursor && has_cursor {
                    monitor_at_point(&win, p.x, p.y).or_else(|| current_monitor(&win))
                } else {
                    win.primary_monitor().ok().flatten()
                };
                if let Some(m) = target_mon {
                    let now = mon_key(&m);
                    let pos_drifted = win
                        .outer_position()
                        .ok()
                        .zip(crate::expected_pos(&win, &m, &align))
                        .is_some_and(|(cur, exp)| (cur.x - exp.0).abs() > 2 || (cur.y - exp.1).abs() > 2);
                    if dirty || screen != Some(now) || pos_drifted {
                        let had_screen = screen.is_some();
                        crate::place_on(&win, &m, &align);
                        if had_screen && !dirty && screen != Some(now) {
                            let _ = app.emit("screen", ());
                        }
                        screen = Some(now);
                    }
                }
                if let (Ok(pos), Ok(scale)) = (win.outer_position(), win.scale_factor()) {
                    geom = Some((pos, scale));
                }
            }

            let cur_fg = unsafe { GetForegroundWindow() }.0 as isize;
            let fg_changed = cur_fg != last_fg;
            if fg_changed {
                last_fg = cur_fg;
            }

            if fg_changed || tick % 10 == 0 {
                let b = top_covered(&win);
                if b != busy {
                    busy = b;
                    let _ = app.emit("topbusy", b);
                }
            }
            if fg_changed || tick % 20 == 0 {
                let f = fullscreen_app(&win);
                if f != full {
                    full = f;
                    let _ = app.emit("fullscreen", f);
                }
                if !full && needs_raise_topmost(&win, interactive) {
                    raise_topmost(&win);
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

            // Приём файлов перетаскиванием включается строго в границах самой пилюли (`now`)
            // и только если курсор с зажатой извне ЛКМ задержался на ней ~175 мс — так перетаскивание
            // вкладок браузера или заголовков окон никогда не цепляется за свёрнутый остров.
            let lmb = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } < 0;
            if !lmb {
                dragging_from_outside = false;
                drag_dwell_ticks = 0;
            } else {
                if !was_lmb && !now {
                    dragging_from_outside = true;
                    drag_dwell_ticks = 0;
                }
                if dragging_from_outside && now {
                    drag_dwell_ticks = drag_dwell_ticks.saturating_add(1);
                } else {
                    drag_dwell_ticks = 0;
                }
            }
            was_lmb = lmb;
            let drag_ready = dragging_from_outside && now && drag_dwell_ticks >= 7;

            if now != inside {
                inside = now;
                let _ = app.emit("hover", now);
            }
            let want = (inside && interactive) || drag_ready;
            if want != capture {
                capture = want;
                let _ = win.set_ignore_cursor_events(!want);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::is_shell_or_transient_class;

    #[test]
    fn shell_and_transient_classes_are_ignored() {
        assert!(is_shell_or_transient_class("Progman"));
        assert!(is_shell_or_transient_class("WorkerW"));
        assert!(is_shell_or_transient_class("Shell_TrayWnd"));
        assert!(is_shell_or_transient_class("Windows.UI.Core.CoreWindow"));
        assert!(is_shell_or_transient_class("XamlExplorerHostIslandWindow"));
        assert!(!is_shell_or_transient_class("Chrome_WidgetWin_1"));
        assert!(!is_shell_or_transient_class("UnrealWindow"));
    }
}
