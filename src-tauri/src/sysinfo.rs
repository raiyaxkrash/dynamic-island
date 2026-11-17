//! Загрузка процессора и памяти для домашнего экрана острова, и счётчик помодоро за день.

use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::{GetLocalTime, GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;

use crate::settings::{roll_stats, save_stats};
use crate::{publish, publish_stats, Shared};

fn ft(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

fn times() -> Option<(u64, u64)> {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    // время ядра уже включает простой
    Some((ft(idle), ft(kernel) + ft(user)))
}

fn memory() -> Option<u32> {
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe { GlobalMemoryStatusEx(&mut m) }.ok()?;
    Some(m.dwMemoryLoad)
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        let mut prev = times();
        let mut last: Option<Value> = None;
        let mut tick: u32 = 0;
        loop {
            std::thread::sleep(Duration::from_millis(1500));
            tick = tick.wrapping_add(1);
            // Раз в ~15 секунд проверяем смену суток, чтобы обнулить дневной счётчик помодоро в полночь.
            if tick % 10 == 0 {
                let d = today();
                let rolled = {
                    let shared = app.state::<Shared>();
                    let mut st = crate::lock(&shared.stats);
                    if roll_stats(&mut st, &d) {
                        Some(st.clone())
                    } else {
                        None
                    }
                };
                if let Some(st) = rolled {
                    save_stats(&app, &st);
                    publish_stats(&app);
                }
            }
            if !crate::lock(&app.state::<Shared>().settings).sysload {
                continue;
            }
            let now = times();
            let cpu = match (prev, now) {
                (Some((i0, t0)), Some((i1, t1))) if t1 > t0 => {
                    let total = (t1 - t0) as f64;
                    let busy = total - (i1 - i0) as f64;
                    (busy / total * 100.0).round().clamp(0.0, 100.0) as u32
                }
                _ => 0,
            };
            prev = now;
            let v = json!({ "cpu": cpu, "mem": memory().unwrap_or(0) });
            if last.as_ref() != Some(&v) {
                publish(&app, "sys", v.clone());
                last = Some(v);
            }
        }
    });
}

/// Сегодняшняя дата, например «2026-11-17».
pub fn today() -> String {
    let t = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay)
}
