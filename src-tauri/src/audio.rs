//! Громкость и устройство вывода звука (Core Audio, событийная модель).
//!
//! Подписывается на `IMMNotificationClient` (смена устройства вывода) и
//! `IAudioEndpointVolumeCallback` (громкость / Mute) вместо постоянного опроса.
//! При подключении наушников или колонки показывает название, тип и заряд Bluetooth.

use std::sync::{mpsc, OnceLock};
use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Manager};
use windows::core::{implement, interface, GUID, HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    eCommunications, eConsole, eMultimedia, eRender, EDataFlow, ERole, IMMDevice,
    IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator,
    PKEY_AudioEndpoint_FormFactor, AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};

use crate::{devices, publish, Shared};

#[derive(Clone, Copy)]
enum AudioEvt {
    DeviceChanged,
    VolumeChanged,
}

#[implement(IMMNotificationClient)]
struct DeviceNotify {
    tx: mpsc::Sender<AudioEvt>,
}

impl IMMNotificationClient_Impl for DeviceNotify_Impl {
    fn OnDeviceStateChanged(&self, _pwstrdeviceid: &PCWSTR, _dwnewstate: DEVICE_STATE) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDeviceAdded(&self, _pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _pwstrdefaultdeviceid: &PCWSTR) -> windows::core::Result<()> {
        if flow == eRender && role == eConsole {
            let _ = self.tx.send(AudioEvt::DeviceChanged);
        }
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _pwstrdeviceid: &PCWSTR, _key: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeNotify {
    tx: mpsc::Sender<AudioEvt>,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeNotify_Impl {
    fn OnNotify(&self, _pnotify: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        let _ = self.tx.send(AudioEvt::VolumeChanged);
        Ok(())
    }
}

fn enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

fn default_device(en: &IMMDeviceEnumerator) -> windows::core::Result<IMMDevice> {
    unsafe { en.GetDefaultAudioEndpoint(eRender, eConsole) }
}

fn device_id(dev: &IMMDevice) -> Option<String> {
    unsafe {
        let p: PWSTR = dev.GetId().ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s
    }
}

/// Название («Наушники (AirPods Pro)») и тип устройства.
fn describe(dev: &IMMDevice) -> Option<(String, &'static str)> {
    unsafe {
        let store = dev.OpenPropertyStore(STGM_READ).ok()?;
        let name = store.GetValue(&PKEY_Device_FriendlyName).ok()?.to_string();
        // EndpointFormFactor: 1 колонки, 3 наушники, 5 гарнитура, 6 трубка, 9 HDMI/монитор.
        let ff = store
            .GetValue(&PKEY_AudioEndpoint_FormFactor)
            .ok()
            .and_then(|v| u32::try_from(&v).ok())
            .unwrap_or(0);
        let kind = match ff {
            3 => "headphones",
            5 | 6 => "headset",
            9 => "display",
            _ => "speakers",
        };
        Some((name, kind))
    }
}

/// «Наушники (AirPods Pro)» → «AirPods Pro».
fn short_name(name: &str) -> String {
    match (name.find('('), name.rfind(')')) {
        (Some(a), Some(b)) if b > a + 1 => name[a + 1..b].trim().to_string(),
        _ => name.trim().to_string(),
    }
}

fn read_volume(ep: &IAudioEndpointVolume) -> Option<(i32, bool)> {
    let (level, mute) = unsafe { (ep.GetMasterVolumeLevelScalar().ok()?, ep.GetMute().ok()?) };
    Some(((level * 100.0).round() as i32, mute.as_bool()))
}

fn emit_device(
    app: &AppHandle,
    short: String,
    kind: &'static str,
    expected_id: Option<String>,
    current_id: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    silent: bool,
) {
    let battery = devices::bluetooth_battery(&short);
    publish(app, "device", json!({ "name": short, "kind": kind, "battery": battery, "silent": silent }));
    if battery.is_none() {
        // Windows иногда выставляет DEVPROPKEY заряда Bluetooth через 1-2 секунды после подключения.
        let app2 = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            if *crate::lock(&current_id) != expected_id {
                return;
            }
            if let Some(bat) = devices::bluetooth_battery(&short) {
                publish(&app2, "device", json!({ "name": short, "kind": kind, "battery": Some(bat), "silent": silent }));
            }
        });
    }
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let (tx, rx) = mpsc::channel::<AudioEvt>();
        let en = loop {
            match enumerator() {
                Ok(e) => break e,
                Err(e) => {
                    crate::log_err(Some(&app), &format!("audio enumerator failed: {e}"));
                    std::thread::sleep(Duration::from_secs(3));
                }
            }
        };

        let dev_cb: IMMNotificationClient = DeviceNotify { tx: tx.clone() }.into();
        let registered_en = unsafe { en.RegisterEndpointNotificationCallback(&dev_cb) }.is_ok();
        let vol_cb: IAudioEndpointVolumeCallback = VolumeNotify { tx: tx.clone() }.into();

        let mut ep: Option<IAudioEndpointVolume> = None;
        let mut last: Option<(i32, bool)> = None;
        let mut dev_id: Option<String> = None;
        let current_id = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let mut quiet_volume = true;

        let refresh_device = |ep: &mut Option<IAudioEndpointVolume>,
                              dev_id: &mut Option<String>,
                              quiet_volume: &mut bool| {
            let Ok(dev) = default_device(&en) else { return };
            let id = device_id(&dev);
            if id == *dev_id && ep.is_some() {
                return;
            }
            if let Some(old) = ep.take() {
                unsafe {
                    let _ = old.UnregisterControlChangeNotify(&vol_cb);
                }
            }
            let first = dev_id.is_none();
            *dev_id = id.clone();
            *crate::lock(&current_id) = id.clone();
            *quiet_volume = true;
            if let Ok(new_ep) = unsafe { dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) } {
                unsafe {
                    let _ = new_ep.RegisterControlChangeNotify(&vol_cb);
                }
                *ep = Some(new_ep);
            }
            if crate::lock(&app.state::<Shared>().settings).devices {
                if let Some((name, kind)) = describe(&dev) {
                    let short = short_name(&name);
                    let app2 = app.clone();
                    let cid = current_id.clone();
                    std::thread::spawn(move || emit_device(&app2, short, kind, id, cid, first));
                }
            }
        };

        refresh_device(&mut ep, &mut dev_id, &mut quiet_volume);
        if let Some(e) = &ep {
            if let Some(cur) = read_volume(e) {
                last = Some(cur);
                publish(&app, "volume", json!({ "level": cur.0, "muted": cur.1, "silent": true }));
                quiet_volume = false;
            }
        }

        loop {
            let evt = if registered_en {
                rx.recv().ok()
            } else {
                // Фоллбэк на редкий опрос, если подписка не удалась.
                Some(rx.recv_timeout(Duration::from_millis(500)).unwrap_or(AudioEvt::DeviceChanged))
            };
            let Some(mut first_evt) = evt else { break };
            let mut need_dev = matches!(first_evt, AudioEvt::DeviceChanged);
            while let Ok(more) = rx.try_recv() {
                first_evt = more;
                if matches!(first_evt, AudioEvt::DeviceChanged) {
                    need_dev = true;
                }
            }

            if need_dev || ep.is_none() {
                refresh_device(&mut ep, &mut dev_id, &mut quiet_volume);
            }
            if let Some(e) = &ep {
                if let Some(cur) = read_volume(e) {
                    if last != Some(cur) {
                        let silent = last.is_none() || quiet_volume;
                        last = Some(cur);
                        publish(&app, "volume", json!({ "level": cur.0, "muted": cur.1, "silent": silent }));
                    }
                    quiet_volume = false;
                }
            }
        }
    });
}

/// Меняет громкость на `delta` (от -1.0 до 1.0) через единый фоновый поток.
pub fn step(delta: f32) {
    static TX: OnceLock<mpsc::Sender<f32>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<f32>();
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            while let Ok(first) = rx.recv() {
                let mut total = first;
                while let Ok(more) = rx.try_recv() {
                    total += more;
                }
                if let Ok(en) = enumerator() {
                    if let Ok(dev) = default_device(&en) {
                        if let Ok(e) = unsafe { dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) } {
                            unsafe {
                                if total > 0.0 && e.GetMute().map(|m| m.as_bool()).unwrap_or(false) {
                                    let _ = e.SetMute(false, std::ptr::null());
                                }
                                if let Ok(level) = e.GetMasterVolumeLevelScalar() {
                                    let _ = e.SetMasterVolumeLevelScalar(
                                        (level + total).clamp(0.0, 1.0),
                                        std::ptr::null(),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        });
        tx
    });
    let _ = tx.send(delta);
}

/// Устанавливает точную громкость `0.0..=1.0` (для ползунка в панели звука).
pub fn set_volume(level: f32) {
    let target = level.clamp(0.0, 1.0);
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Ok(en) = enumerator() {
            if let Ok(dev) = default_device(&en) {
                if let Ok(e) = dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) {
                    if target > 0.0 && e.GetMute().map(|m| m.as_bool()).unwrap_or(false) {
                        let _ = e.SetMute(false, std::ptr::null());
                    }
                    let _ = e.SetMasterVolumeLevelScalar(target, std::ptr::null());
                }
            }
        }
    });
}

/// Переключает системный Mute (вкл/выкл).
pub fn toggle_mute() {
    std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Ok(en) = enumerator() {
            if let Ok(dev) = default_device(&en) {
                if let Ok(e) = dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) {
                    if let Ok(m) = e.GetMute() {
                        let _ = e.SetMute(!m.as_bool(), std::ptr::null());
                    }
                }
            }
        }
    });
}

#[derive(Serialize, Clone, Debug)]
pub struct AudioDeviceInfo {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub active: bool,
}

#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    unsafe fn _pad3(&self) -> HRESULT;
    unsafe fn _pad4(&self) -> HRESULT;
    unsafe fn _pad5(&self) -> HRESULT;
    unsafe fn _pad6(&self) -> HRESULT;
    unsafe fn _pad7(&self) -> HRESULT;
    unsafe fn _pad8(&self) -> HRESULT;
    unsafe fn _pad9(&self) -> HRESULT;
    unsafe fn _pad10(&self) -> HRESULT;
    unsafe fn _pad11(&self) -> HRESULT;
    unsafe fn _pad12(&self) -> HRESULT;
    unsafe fn set_default_endpoint(&self, device_id: PCWSTR, role: ERole) -> HRESULT;
}

const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// Возвращает список активных устройств вывода звука.
pub fn list_devices() -> Vec<AudioDeviceInfo> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(en) = enumerator() else { return Vec::new() };
        let def_id = default_device(&en).ok().and_then(|d| device_id(&d));
        let Ok(col) = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else {
            return Vec::new();
        };
        let count = col.GetCount().unwrap_or(0);
        let mut out = Vec::new();
        for i in 0..count {
            let Ok(dev) = col.Item(i) else { continue };
            let Some(id) = device_id(&dev) else { continue };
            let Some((full_name, kind)) = describe(&dev) else { continue };
            out.push(AudioDeviceInfo {
                active: def_id.as_deref() == Some(id.as_str()),
                id,
                name: short_name(&full_name),
                kind,
            });
        }
        out
    }
}

/// Переключает устройство вывода звука по умолчанию на `id`.
pub fn set_default_device(id: &str) -> bool {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(pc): windows::core::Result<IPolicyConfig> =
            CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL)
        else {
            return false;
        };
        let wide = HSTRING::from(id);
        let ptr = PCWSTR(wide.as_ptr());
        let ok1 = pc.set_default_endpoint(ptr, eConsole).is_ok();
        let _ = pc.set_default_endpoint(ptr, eMultimedia);
        let _ = pc.set_default_endpoint(ptr, eCommunications);
        ok1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_name_extracts_device_name() {
        assert_eq!(short_name("Наушники (AirPods Pro)"), "AirPods Pro");
        assert_eq!(short_name("Speakers (Realtek(R) Audio)"), "Realtek(R) Audio");
        assert_eq!(short_name("HDMI Audio"), "HDMI Audio");
    }
}
