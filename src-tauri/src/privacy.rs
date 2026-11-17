//! Какие программы сейчас используют микрофон и камеру (событийная модель).
//!
//! Подписывается через `RegNotifyChangeKeyValue` на ветки `ConsentStore\microphone` и
//! `ConsentStore\webcam` вместо постоянного опроса реестра.

use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Manager};
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY,
    HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME,
    RRF_RT_REG_QWORD,
};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};

use crate::{publish, Shared};

const STORE: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";

/// «C:#Program Files#Discord#app-1.0#Discord.exe» → «Discord»,
/// «Microsoft.WindowsSoundRecorder_8wekyb3d8bbwe» → «WindowsSoundRecorder».
fn friendly(key: &str) -> String {
    if key.contains('#') {
        let file = key.rsplit('#').next().unwrap_or(key);
        return file.trim_end_matches(".exe").trim_end_matches(".EXE").to_string();
    }
    let pkg = key.split('_').next().unwrap_or(key);
    pkg.rsplit('.').next().unwrap_or(pkg).to_string()
}

unsafe fn active_in(path: &str, out: &mut Vec<String>) {
    let mut key = HKEY::default();
    if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), None, KEY_READ, &mut key).is_err() {
        return;
    }
    let mut i = 0u32;
    loop {
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let r = RegEnumKeyExW(key, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None);
        if r.is_err() {
            break;
        }
        i += 1;
        let name = String::from_utf16_lossy(&buf[..len as usize]);
        if name == "NonPackaged" {
            continue;
        }
        let mut stop: u64 = 1;
        let mut size = std::mem::size_of::<u64>() as u32;
        let sub = HSTRING::from(name.as_str());
        let ok = RegGetValueW(
            key,
            PCWSTR(sub.as_ptr()),
            w!("LastUsedTimeStop"),
            RRF_RT_REG_QWORD,
            None,
            Some(&mut stop as *mut u64 as *mut _),
            Some(&mut size),
        )
        .is_ok();
        if ok && stop == 0 {
            let n = friendly(&name);
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    let _ = RegCloseKey(key);
}

fn in_use(device: &str) -> Vec<String> {
    let mut out = Vec::new();
    let base = format!(r"{STORE}\{device}");
    unsafe {
        active_in(&base, &mut out);
        active_in(&format!(r"{base}\NonPackaged"), &mut out);
    }
    out
}

unsafe fn arm_watch(path: &str, evt: HANDLE) -> Option<HKEY> {
    let mut key = HKEY::default();
    if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), None, KEY_NOTIFY, &mut key).is_err() {
        return None;
    }
    let filter = REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET;
    if RegNotifyChangeKeyValue(key, true, filter, Some(evt), true).is_err() {
        let _ = RegCloseKey(key);
        return None;
    }
    Some(key)
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        let evt = unsafe { CreateEventW(None, true, false, None) }.ok();
        let mut last = None;
        let mic_path = format!(r"{STORE}\microphone");
        let cam_path = format!(r"{STORE}\webcam");

        loop {
            let enabled = crate::lock(&app.state::<Shared>().settings).privacy;
            let v = if enabled {
                json!({ "mic": in_use("microphone"), "cam": in_use("webcam") })
            } else {
                json!({ "mic": [], "cam": [] })
            };
            if last.as_ref() != Some(&v) {
                publish(&app, "privacy", v.clone());
                last = Some(v);
            }

            if let Some(ev) = evt {
                unsafe {
                    let _ = ResetEvent(ev);
                    let k_mic = arm_watch(&mic_path, ev);
                    let k_cam = arm_watch(&cam_path, ev);
                    if k_mic.is_some() || k_cam.is_some() {
                        // Просыпаемся мгновенно при изменении реестра или раз в 4 с для синхронизации флага privacy.
                        let _ = WaitForSingleObject(ev, 4000) == WAIT_OBJECT_0;
                    } else {
                        std::thread::sleep(Duration::from_millis(1500));
                    }
                    if let Some(k) = k_mic {
                        let _ = RegCloseKey(k);
                    }
                    if let Some(k) = k_cam {
                        let _ = RegCloseKey(k);
                    }
                }
            } else {
                std::thread::sleep(Duration::from_millis(1500));
            }
        }
        #[allow(unreachable_code)]
        if let Some(ev) = evt {
            unsafe {
                let _ = CloseHandle(ev);
            }
        }
    });
}
