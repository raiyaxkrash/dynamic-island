//! Заряд Bluetooth-наушников: Windows хранит его в свойстве устройства
//! «Bluetooth battery level», то же число, что видно в «Параметрах» → Bluetooth.

use windows::core::GUID;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDevicePropertyW,
    SetupDiGetDeviceRegistryPropertyW, DIGCF_ALLCLASSES, DIGCF_PRESENT, SPDRP_FRIENDLYNAME, SP_DEVINFO_DATA,
};
use windows::Win32::Devices::Properties::DEVPROPTYPE;
use windows::Win32::Foundation::DEVPROPKEY;

const BATTERY: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5),
    pid: 2,
};

/// Ищет среди подключённых устройств то, чьё имя совпадает с `name`, и у которого есть заряд.
pub fn bluetooth_battery(name: &str) -> Option<u8> {
    let want = name.to_lowercase();
    if want.is_empty() {
        return None;
    }
    unsafe {
        let set = SetupDiGetClassDevsW(None, None, None, DIGCF_PRESENT | DIGCF_ALLCLASSES).ok()?;
        let mut found = None;
        let mut i = 0u32;
        loop {
            let mut info = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInfo(set, i, &mut info).is_err() {
                break;
            }
            i += 1;

            let mut ty = DEVPROPTYPE::default();
            let mut level = [0u8; 1];
            if SetupDiGetDevicePropertyW(set, &info, &BATTERY, &mut ty, Some(&mut level), None, 0).is_err() {
                continue;
            }
            let mut buf = [0u8; 512];
            if SetupDiGetDeviceRegistryPropertyW(set, &info, SPDRP_FRIENDLYNAME, None, Some(&mut buf), None).is_err() {
                continue;
            }
            let wide: Vec<u16> = buf
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| u16::from_le_bytes(c))
                .take_while(|&c| c != 0)
                .collect();
            let dev_name = String::from_utf16_lossy(&wide).to_lowercase();
            if (dev_name.contains(&want) || want.contains(&dev_name)) && level[0] <= 100 {
                found = Some(level[0]);
                break;
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
        found
    }
}
