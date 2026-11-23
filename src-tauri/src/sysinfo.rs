//! Загрузка процессора и памяти для домашнего экрана острова, и счётчик помодоро за день.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::System::SystemInformation::{GetLocalTime, GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;

use crate::settings::{roll_stats, save_stats};
use crate::{publish, publish_stats, Shared};

fn net_bytes() -> Option<(u64, u64)> {
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table).is_err() || table.is_null() {
            return None;
        }
        let count = (*table).NumEntries as usize;
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), count);
        let (mut rx, mut tx) = (0u64, 0u64);
        for row in rows {
            // Исключаем loopback (24), неактивные и виртуальные фильтры без физического MAC.
            if row.Type != 24 && row.OperStatus == IfOperStatusUp && row.PhysicalAddressLength > 0 {
                rx = rx.saturating_add(row.InOctets);
                tx = tx.saturating_add(row.OutOctets);
            }
        }
        FreeMibTable(table as *const _);
        Some((rx, tx))
    }
}

fn ft(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

fn times() -> Option<(u64, u64)> {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    // время ядра уже включает простой
    Some((ft(idle), ft(kernel) + ft(user)))
}

fn memory() -> Option<(u32, f64, f64)> {
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe { GlobalMemoryStatusEx(&mut m) }.ok()?;
    let gib = 1024.0 * 1024.0 * 1024.0;
    let total = (m.ullTotalPhys as f64 / gib * 10.0).round() / 10.0;
    let used = (m.ullTotalPhys.saturating_sub(m.ullAvailPhys) as f64 / gib * 10.0).round() / 10.0;
    Some((m.dwMemoryLoad, used, total))
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        let mut prev = times();
        let mut prev_net = net_bytes().map(|b| (b, Instant::now()));
        let mut last: Option<Value> = None;
        let mut tick: u32 = 0;
        let mut was_interactive = false;
        loop {
            std::thread::sleep(Duration::from_millis(1500));
            tick = tick.wrapping_add(1);
            // Раз в ~15 секунд проверяем смену суток, чтобы обнулить дневной счётчик помодоро в полночь.
            if tick.is_multiple_of(10) {
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
            let shared = app.state::<Shared>();
            if !crate::lock(&shared.settings).sysload {
                was_interactive = false;
                continue;
            }
            let interactive = shared.interactive.load(Ordering::Relaxed);
            // В свёрнутом покое не будим WebView2 каждые 1.5с; первый снимок делаем сразу,
            // а дальше обновляем только когда остров раскрыт.
            if !interactive && last.is_some() {
                if was_interactive {
                    prev = times();
                    prev_net = net_bytes().map(|b| (b, Instant::now()));
                    was_interactive = false;
                }
                continue;
            }
            was_interactive = interactive;
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
            let now_net = net_bytes().map(|b| (b, Instant::now()));
            let (rx_bps, tx_bps) = match (prev_net, now_net) {
                (Some(((r0, t0), at0)), Some(((r1, t1), at1))) => {
                    let dt = at1.duration_since(at0).as_secs_f64();
                    if dt > 0.2 {
                        (
                            (r1.saturating_sub(r0) as f64 / dt).round() as u64,
                            (t1.saturating_sub(t0) as f64 / dt).round() as u64,
                        )
                    } else {
                        (0, 0)
                    }
                }
                _ => (0, 0),
            };
            prev_net = now_net;
            let (mem, used_gb, total_gb) = memory().unwrap_or((0, 0.0, 0.0));
            let v = json!({
                "cpu": cpu,
                "mem": mem,
                "usedGb": used_gb,
                "totalGb": total_gb,
                "rxBps": rx_bps,
                "txBps": tx_bps
            });
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
