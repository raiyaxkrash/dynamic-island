//! Батарея: заряд в процентах и подключена ли зарядка (событийная модель).
//!
//! Подписывается на `RegisterPowerSettingNotification` (питание от сети, уровень заряда,
//! режим экономии энергии) на скрытом окне; при сбое регистрации работает редкий опрос.

use std::cell::RefCell;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::AppHandle;
use windows::core::{w, GUID};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, RegisterPowerSettingNotification, UnregisterPowerSettingNotification,
    HPOWERNOTIFY, SYSTEM_POWER_STATUS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DEVICE_NOTIFY_WINDOW_HANDLE, PBT_POWERSETTINGCHANGE, WM_POWERBROADCAST,
};

use crate::{publish, winmsg};

const GUID_ACDC_POWER_SOURCE: GUID = GUID::from_u128(0x5d3e9a59_e9d5_4b00_a6bd_ff34ff516548);
const GUID_BATTERY_PERCENTAGE_REMAINING: GUID = GUID::from_u128(0xa7ad8041_b45a_4cae_87a3_eecbb468a9e1);
const GUID_POWER_SAVING_STATUS: GUID = GUID::from_u128(0xe00958c0_c213_4ace_ac77_fecced2eeea5);

thread_local! {
    static STATE: RefCell<Option<(AppHandle, Option<Value>)>> = const { RefCell::new(None) };
}

fn read() -> Option<Value> {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s) }.ok()?;
    // 128 — батареи нет (настольный ПК), 255 — состояние неизвестно.
    let battery = s.BatteryFlag != 128 && s.BatteryFlag != 255;
    let percent = if s.BatteryLifePercent <= 100 { s.BatteryLifePercent } else { 100 };
    Some(json!({
        "ac": s.ACLineStatus == 1,
        "percent": percent,
        "battery": battery,
        "saver": s.SystemStatusFlag == 1,
    }))
}

fn push_if_changed() {
    STATE.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some((app, last)) = borrow.as_mut() else { return };
        if let Some(v) = read() {
            if last.as_ref() != Some(&v) {
                publish(app, "power", v.clone());
                *last = Some(v);
            }
        }
    });
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_POWERBROADCAST && wparam.0 as u32 == PBT_POWERSETTINGCHANGE {
        push_if_changed();
        return LRESULT(1);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        STATE.with(|cell| *cell.borrow_mut() = Some((app.clone(), None)));
        push_if_changed();

        let mut handles: Vec<HPOWERNOTIFY> = Vec::new();
        let ok = winmsg::run(w!("DynamicIslandPowerListener"), Some(wnd_proc), |hwnd| {
            let h = HANDLE(hwnd.0);
            for guid in [
                &GUID_ACDC_POWER_SOURCE,
                &GUID_BATTERY_PERCENTAGE_REMAINING,
                &GUID_POWER_SAVING_STATUS,
            ] {
                if let Ok(n) = unsafe { RegisterPowerSettingNotification(h, guid, DEVICE_NOTIFY_WINDOW_HANDLE) } {
                    handles.push(n);
                }
            }
            !handles.is_empty()
        });
        for h in handles {
            unsafe {
                let _ = UnregisterPowerSettingNotification(h);
            }
        }
        if ok {
            return;
        }

        crate::log_err(Some(&app), "RegisterPowerSettingNotification failed, using fallback polling");
        loop {
            std::thread::sleep(Duration::from_millis(2000));
            push_if_changed();
        }
    });
}
