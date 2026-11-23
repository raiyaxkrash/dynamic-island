//! Управление яркостью экрана без сторонних зависимостей:
//! 1. Ноутбуки (встроенная матрица): `\\.\LCD` через `DeviceIoControl` (`IOCTL_VIDEO_*_DISPLAY_BRIGHTNESS`) — мгновенно (<1 мс).
//! 2. Внешние мониторы: DDC/CI через `dxva2` (`GetMonitorBrightness` / `SetMonitorBrightness`).

use windows::core::w;
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, SetMonitorBrightness, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, POINT};
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;

const GENERIC_READ_WRITE: u32 = 0xC000_0000;
const IOCTL_VIDEO_QUERY_DISPLAY_BRIGHTNESS: u32 = 0x0023_0498;
const IOCTL_VIDEO_SET_DISPLAY_BRIGHTNESS: u32 = 0x0023_049C;

#[repr(C)]
#[derive(Default)]
struct DisplayBrightness {
    display_policy: u8,
    ac_brightness: u8,
    dc_brightness: u8,
}

unsafe fn open_lcd() -> Option<HANDLE> {
    let h = CreateFileW(
        w!(r"\\.\LCD"),
        GENERIC_READ_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL,
        None,
    )
    .ok()?;
    if h.is_invalid() { None } else { Some(h) }
}

fn lcd_get() -> Option<u8> {
    unsafe {
        let h = open_lcd()?;
        let mut db = DisplayBrightness::default();
        let mut returned = 0u32;
        let ok = DeviceIoControl(
            h,
            IOCTL_VIDEO_QUERY_DISPLAY_BRIGHTNESS,
            None,
            0,
            Some(&mut db as *mut _ as *mut _),
            std::mem::size_of::<DisplayBrightness>() as u32,
            Some(&mut returned),
            None,
        )
        .is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return None;
        }
        // display_policy: 1 = AC, 2 = DC, 3 = both
        let val = if db.display_policy == 2 { db.dc_brightness } else { db.ac_brightness };
        Some(val.min(100))
    }
}

fn lcd_set(pct: u8) -> Option<u8> {
    let target = pct.min(100);
    unsafe {
        let h = open_lcd()?;
        let mut db = DisplayBrightness {
            display_policy: 3, // DISPLAYPOLICY_BOTH (AC + DC)
            ac_brightness: target,
            dc_brightness: target,
        };
        let mut returned = 0u32;
        let ok = DeviceIoControl(
            h,
            IOCTL_VIDEO_SET_DISPLAY_BRIGHTNESS,
            Some(&mut db as *mut _ as *const _),
            std::mem::size_of::<DisplayBrightness>() as u32,
            None,
            0,
            Some(&mut returned),
            None,
        )
        .is_ok();
        let _ = CloseHandle(h);
        if ok { Some(target) } else { None }
    }
}

fn ddcci_with_primary<T>(f: impl FnOnce(HANDLE) -> Option<T>) -> Option<T> {
    unsafe {
        let hmon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut count = 0u32;
        if GetNumberOfPhysicalMonitorsFromHMONITOR(hmon, &mut count).is_err() || count == 0 {
            return None;
        }
        let mut mons = vec![PHYSICAL_MONITOR::default(); count as usize];
        if GetPhysicalMonitorsFromHMONITOR(hmon, &mut mons).is_err() {
            return None;
        }
        let res = f(mons[0].hPhysicalMonitor);
        let _ = DestroyPhysicalMonitors(&mons);
        res
    }
}

fn ddcci_get() -> Option<u8> {
    ddcci_with_primary(|h| unsafe {
        let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
        if GetMonitorBrightness(h, &mut min, &mut cur, &mut max) == 0 || max <= min {
            return None;
        }
        let pct = ((cur.saturating_sub(min) as f64) / ((max - min) as f64) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8;
        Some(pct)
    })
}

fn ddcci_set(pct: u8) -> Option<u8> {
    let target = pct.min(100);
    ddcci_with_primary(|h| unsafe {
        let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
        if GetMonitorBrightness(h, &mut min, &mut cur, &mut max) == 0 || max <= min {
            return None;
        }
        let raw = min + (((max - min) as f64) * (target as f64) / 100.0).round() as u32;
        if SetMonitorBrightness(h, raw.clamp(min, max)) != 0 {
            Some(target)
        } else {
            None
        }
    })
}

/// Возвращает текущую яркость основного экрана (`0..=100`), если она поддерживается.
pub fn get() -> Option<u8> {
    lcd_get().or_else(ddcci_get)
}

/// Устанавливает яркость основного экрана (`0..=100`).
pub fn set(pct: u8) -> Option<u8> {
    lcd_set(pct).or_else(|| ddcci_set(pct))
}

/// Изменяет яркость на `delta` (например `+5` или `-5`) и возвращает новое значение.
pub fn step(delta: i32) -> Option<u8> {
    let cur = get()? as i32;
    let next = (cur + delta).clamp(0, 100) as u8;
    set(next)
}
