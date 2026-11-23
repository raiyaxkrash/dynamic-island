//! Общий помощник для фоновых скрытых message-only окон Win32 (`HWND_MESSAGE`).

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, RegisterClassW,
    TranslateMessage, HWND_MESSAGE, MSG, WNDCLASSW, WNDPROC,
};

/// Создаёт скрытое message-only окно и запускает цикл сообщений `GetMessageW`.
/// Возвращает `false`, если создать окно или завершить `setup` не удалось.
pub fn run(class_name: PCWSTR, proc: WNDPROC, setup: impl FnOnce(HWND) -> bool) -> bool {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: proc,
            hInstance: hinst.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            Default::default(),
            class_name,
            windows::core::w!(""),
            Default::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst.into()),
            None,
        ) else {
            return false;
        };
        if !setup(hwnd) {
            let _ = DestroyWindow(hwnd);
            return false;
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = DestroyWindow(hwnd);
        true
    }
}
