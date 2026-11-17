//! Раскладка клавиатуры и Caps Lock.
//!
//! Раскладку в Windows помнит каждое окно своя, поэтому при переключении между окнами она
//! «меняется» сама. Показываем её, только если пользователь переключил язык в том же окне.

use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Manager};
use windows::Win32::Foundation::HWND;
use windows::Win32::Globalization::{LCIDToLocaleName, LOCALE_ALLOW_NEUTRAL_NAMES};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, GetKeyboardLayout, VK_CAPITAL};
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId};

use crate::{publish, Shared};

/// Код языка → («РУ», «Русская»).
fn label(langid: u16) -> (String, String) {
    let (short, long) = match langid & 0x3ff {
        0x19 => ("РУ", "Русская"),
        0x09 => ("EN", "English"),
        0x22 => ("УК", "Українська"),
        0x23 => ("БЕ", "Беларуская"),
        0x3f => ("ҚАЗ", "Қазақша"),
        0x07 => ("DE", "Deutsch"),
        0x0c => ("FR", "Français"),
        0x0a => ("ES", "Español"),
        0x10 => ("IT", "Italiano"),
        0x16 => ("PT", "Português"),
        0x15 => ("PL", "Polski"),
        0x1f => ("TR", "Türkçe"),
        0x11 => ("JA", "日本語"),
        0x12 => ("KO", "한국어"),
        0x04 => ("ZH", "中文"),
        0x0d => ("HE", "עברית"),
        0x01 => ("AR", "العربية"),
        _ => ("", ""),
    };
    if !short.is_empty() {
        return (short.to_string(), long.to_string());
    }
    let mut buf = [0u16; 85];
    let n = unsafe { LCIDToLocaleName(langid as u32, Some(&mut buf), LOCALE_ALLOW_NEUTRAL_NAMES) } as usize;
    let name = String::from_utf16_lossy(&buf[..n.saturating_sub(1)]);
    let code = name.split('-').next().unwrap_or("").to_uppercase();
    (code, name)
}

fn is_transient_shell_flyout(hwnd: HWND) -> bool {
    if hwnd.is_invalid() {
        return true;
    }
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    let class = String::from_utf16_lossy(&buf[..n]);
    matches!(
        class.as_str(),
        "Shell_InputSwitchTopLevelWindow"
            | "Shell_InputSwitchDismissOverlay"
            | "XamlExplorerHostIslandWindow"
            | "Windows.UI.Core.CoreWindow"
    )
}

fn current() -> (isize, u16, bool) {
    unsafe {
        let fg = GetForegroundWindow();
        let thread = GetWindowThreadProcessId(fg, None);
        let hkl = GetKeyboardLayout(thread);
        (fg.0 as isize, (hkl.0 as usize & 0xffff) as u16, is_transient_shell_flyout(fg))
    }
}

fn caps() -> bool {
    unsafe { GetKeyState(VK_CAPITAL.0 as i32) & 1 == 1 }
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        let (mut last_win, mut last_lang, _) = current();
        let mut last_caps = caps();
        loop {
            std::thread::sleep(Duration::from_millis(120));
            if !crate::lock(&app.state::<Shared>().settings).keyboard {
                std::thread::sleep(Duration::from_millis(880));
                continue;
            }
            let (win, lang, transient) = current();
            let c = caps();

            if c != last_caps {
                last_caps = c;
                publish(&app, "caps", json!(c));
            }
            // Всплывающее меню Win+Space кратковременно забирает фокус, не считаем его сменой окна.
            if transient {
                continue;
            }
            if lang != last_lang {
                // язык сменился в том же окне — это переключение раскладки, а не смена окна
                if win == last_win && lang != 0 {
                    let (short, long) = label(lang);
                    if !short.is_empty() {
                        publish(&app, "layout", json!({ "short": short, "long": long }));
                    }
                }
                last_lang = lang;
            }
            last_win = win;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_language_ids() {
        assert_eq!(label(0x0419), ("РУ".to_string(), "Русская".to_string()));
        assert_eq!(label(0x0409), ("EN".to_string(), "English".to_string()));
        assert_eq!(label(0x0411), ("JA".to_string(), "日本語".to_string()));
    }
}
