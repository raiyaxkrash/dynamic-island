//! Dynamic Island для Windows.
//!
//! Окно острова прозрачное и всегда поверх остальных. Клики сквозь него проходят,
//! пока остров не раскрыт: поток `hover` следит за курсором и включает приём мыши
//! только в прямоугольнике, который сообщает интерфейс.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use serde_json::{json, Map, Value};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Monitor, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

mod audio;
mod brightness;
mod clipboard;
mod devices;
mod hotkeys;
mod hover;
mod keyboard;
mod media;
mod power;
mod privacy;
mod settings;
mod shelf;
mod sysinfo;
mod updater;
mod wic;
mod winmsg;

use settings::{Settings, Stats};
use shelf::ShelfItem;

type TrayItems = (
    MenuItem<tauri::Wry>,
    MenuItem<tauri::Wry>,
    MenuItem<tauri::Wry>,
    MenuItem<tauri::Wry>,
);

/// Общее состояние: зона острова (в CSS-пикселях окна), настройки и последние снимки данных.
#[derive(Default)]
pub struct Shared {
    pub rect: Mutex<[f64; 4]>,
    /// Остров раскрыт и должен ловить мышь; иначе клики идут сквозь него.
    pub interactive: AtomicBool,
    /// Позиция или масштаб окна изменились — потоку `hover` пора перечитать геометрию.
    pub geom_dirty: AtomicBool,
    /// Пользователь явно скрыл остров через пункт меню в системном трее.
    pub user_hidden: AtomicBool,
    pub settings: Mutex<Settings>,
    pub stats: Mutex<Stats>,
    pub shelf: Mutex<Vec<ShelfItem>>,
    pub clips: Mutex<Vec<clipboard::ClipItem>>,
    pub tray_items: Mutex<Option<TrayItems>>,
    pub snap: Mutex<Map<String, Value>>,
}

/// Захват мьютекса без каскадных паник при отравлении.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Атомарная запись файла через `.tmp` + `rename`, чтобы при внезапном выключении ПК конфиг не обнулился.
pub(crate) fn write_atomic(path: &std::path::Path, data: &[u8]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, data).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::write(path, data);
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Пишет диагностическое сообщение в `%APPDATA%\app.dynamicisland.desktop\island.log` с ротацией на 256 КБ.
pub(crate) fn log_err(app: Option<&AppHandle>, msg: &str) {
    let Some(dir) = app.and_then(|a| a.path().app_config_dir().ok()) else {
        if cfg!(debug_assertions) {
            eprintln!("[island] {msg}");
        }
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("island.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            let _ = std::fs::rename(&path, dir.join("island.log.old"));
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let ts = sysinfo::today();
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

/// Запоминает свежие данные и отправляет их интерфейсу событием `kind`.
pub fn publish(app: &AppHandle, kind: &str, value: Value) {
    if let Some(s) = app.try_state::<Shared>() {
        lock(&s.snap).insert(kind.to_string(), value.clone());
    }
    let _ = app.emit(kind, value);
}

fn publish_settings(app: &AppHandle) {
    let s = lock(&app.state::<Shared>().settings).clone();
    publish(app, "settings", json!(s));
}

pub(crate) fn publish_stats(app: &AppHandle) {
    let s = lock(&app.state::<Shared>().stats).clone();
    let (_, week, streak) = settings::stats_summary(&s, &sysinfo::today());
    publish(app, "stats", json!({ "pomodoros": s.pomodoros, "week": week, "streak": streak }));
}

#[tauri::command]
fn set_hit_rect(state: tauri::State<Shared>, x: f64, y: f64, w: f64, h: f64, interactive: bool) {
    *lock(&state.rect) = [x, y, w, h];
    state.interactive.store(interactive, Ordering::Relaxed);
}

#[tauri::command]
fn snapshot(state: tauri::State<Shared>) -> Value {
    Value::Object(lock(&state.snap).clone())
}

#[tauri::command]
fn media_action(action: String) {
    media::action(&action);
}

#[tauri::command]
fn media_seek(seconds: f64) {
    media::seek(seconds);
}

#[tauri::command]
fn media_cycle() {
    media::cycle_session();
}

#[tauri::command]
fn open_privacy_settings() {
    unsafe {
        let uri = HSTRING::from("ms-settings:privacy-microphone");
        let _ = ShellExecuteW(None, w!("open"), PCWSTR(uri.as_ptr()), None, None, SW_SHOWNORMAL);
    }
}

#[tauri::command]
fn open_sound_settings() {
    unsafe {
        let uri = HSTRING::from("ms-settings:sound");
        let _ = ShellExecuteW(None, w!("open"), PCWSTR(uri.as_ptr()), None, None, SW_SHOWNORMAL);
    }
}

#[tauri::command]
fn get_audio_devices() -> Value {
    json!(audio::list_devices())
}

#[tauri::command]
fn set_audio_device(id: String) -> bool {
    audio::set_default_device(&id)
}

#[tauri::command]
fn get_audio_sessions() -> Value {
    json!(audio::list_sessions())
}

#[tauri::command]
fn set_app_volume(pid: u32, level: f32) {
    audio::set_session_volume(pid, level);
}

#[tauri::command]
fn toggle_app_mute(pid: u32) -> bool {
    audio::toggle_session_mute(pid)
}

#[tauri::command]
async fn check_update(app: AppHandle) -> Value {
    let info = tauri::async_runtime::spawn_blocking(|| updater::check(env!("CARGO_PKG_VERSION")))
        .await
        .unwrap_or_default();
    if info.available {
        publish(&app, "update", json!(info));
    }
    json!(info)
}

#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    let cached = lock(&app.state::<Shared>().snap).get("update").cloned();
    let info: updater::UpdateInfo = match cached.and_then(|v| serde_json::from_value(v).ok()) {
        Some(i) => i,
        None => tauri::async_runtime::spawn_blocking(|| updater::check(env!("CARGO_PKG_VERSION")))
            .await
            .map_err(|_| "network".to_string())?,
    };
    if !info.available {
        return Err("latest".to_string());
    }

    // 1. Бесшовное обновление на месте: подмена текущего .exe без установщика и без удаления старой версии.
    if let Some(portable_url) = info.portable_url.clone() {
        let handle = app.clone();
        let res = tauri::async_runtime::spawn_blocking(move || {
            updater::self_update_in_place(&portable_url, |pct| {
                let _ = handle.emit("update-progress", pct);
            })
        })
        .await
        .map_err(|_| "network".to_string())?;

        match res {
            Ok(new_exe) => {
                release_single_instance();
                if std::process::Command::new(&new_exe).arg("--relaunch").spawn().is_ok() {
                    app.exit(0);
                    return Ok(());
                }
                return Err("launch".to_string());
            }
            Err("readonly") => {
                // Папка защищена от записи (например Program Files) — переходим к тихому setup.exe /S.
            }
            Err(e) => return Err(e.to_string()),
        }
    }

    // 2. Фоллбэк: скачиваем NSIS-установщик и запускаем его в тихом режиме (`/S`).
    let (Some(dl_url), Some(asset_name)) = (info.download_url.clone(), info.asset_name.clone()) else {
        open_release_url(info.url);
        return Ok(());
    };
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        updater::download_and_install(&dl_url, &asset_name, |pct| {
            let _ = handle.emit("update-progress", pct);
        })
    })
    .await
    .map_err(|_| "network".to_string())?
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_release_url(url: String) {
    if updater::allowed_release_url(&url) {
        unsafe {
            let wide = HSTRING::from(url.as_str());
            let _ = ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), None, None, SW_SHOWNORMAL);
        }
    }
}

fn publish_shelf(app: &AppHandle) {
    let items = lock(&app.state::<Shared>().shelf).clone();
    shelf::save(app, &items);
    publish(app, "shelf", json!(items));
}

#[tauri::command]
fn shelf_remove(app: AppHandle, path: String) {
    {
        let shared = app.state::<Shared>();
        lock(&shared.shelf).retain(|item| item.path != path);
    }
    publish_shelf(&app);
}

#[tauri::command]
fn shelf_clear(app: AppHandle) {
    lock(&app.state::<Shared>().shelf).clear();
    publish_shelf(&app);
}

#[tauri::command]
fn shelf_drag(path: String) {
    shelf::start_drag(path);
}

#[tauri::command]
fn shelf_drag_all(state: tauri::State<Shared>) {
    let paths: Vec<String> = lock(&state.shelf).iter().map(|i| i.path.clone()).collect();
    shelf::start_drag_many(paths);
}

#[tauri::command]
fn shelf_reveal(path: String) {
    shelf::reveal_in_explorer(path);
}

#[tauri::command]
fn shelf_open(path: String) {
    shelf::open_file(path);
}

#[tauri::command]
fn shelf_from_clipboard(app: AppHandle) -> bool {
    let paths = clipboard::read_files();
    if paths.is_empty() {
        return false;
    }
    let changed = {
        let shared = app.state::<Shared>();
        let mut sh = lock(&shared.shelf);
        shelf::add_paths(&mut sh, paths)
    };
    if changed {
        publish_shelf(&app);
    }
    changed
}

#[tauri::command]
fn clip_copy(state: tauri::State<Shared>, id: Option<u64>, text: Option<String>) -> bool {
    if let Some(target_id) = id {
        let found = lock(&state.clips).iter().find(|c| c.id == target_id).cloned();
        if let Some(item) = found {
            return if item.kind == "image" {
                item.png.as_deref().is_some_and(clipboard::write_image)
            } else {
                clipboard::write_text(&item.text)
            };
        }
    }
    if let Some(t) = text {
        return clipboard::write_text(&t);
    }
    false
}

#[tauri::command]
fn clip_drag(state: tauri::State<Shared>, id: u64) {
    let png = lock(&state.clips)
        .iter()
        .find(|c| c.id == id && c.kind == "image")
        .and_then(|c| c.png.clone());
    if let Some(bytes) = png {
        clipboard::drag_image(id, &bytes);
    }
}

#[tauri::command]
fn clips_clear(app: AppHandle) {
    lock(&app.state::<Shared>().clips).clear();
    clipboard::clear_temp_files();
    publish(&app, "clips", json!([]));
}

#[tauri::command]
fn clip_pin(app: AppHandle, id: u64) {
    let items = {
        let shared = app.state::<Shared>();
        let mut clips = lock(&shared.clips);
        if !clipboard::toggle_pin(&mut clips, id) {
            return;
        }
        clips.clone()
    };
    publish(&app, "clips", json!(items));
}

#[tauri::command]
fn volume_step(delta: f32) {
    audio::step(delta);
}

#[tauri::command]
fn volume_set(level: f32) {
    audio::set_volume(level);
}

#[tauri::command]
fn volume_mute_toggle() {
    audio::toggle_mute();
}

#[tauri::command]
fn mic_mute_toggle(app: AppHandle) -> Option<bool> {
    toggle_mic_mute_global(&app)
}

#[tauri::command]
async fn get_brightness() -> Option<u8> {
    tauri::async_runtime::spawn_blocking(brightness::get)
        .await
        .ok()
        .flatten()
}

#[tauri::command]
async fn set_brightness(app: AppHandle, level: u8) -> Option<u8> {
    let val = tauri::async_runtime::spawn_blocking(move || brightness::set(level))
        .await
        .ok()
        .flatten()?;
    publish(&app, "brightness", json!({ "level": val, "silent": true }));
    Some(val)
}

#[tauri::command]
async fn step_brightness(app: AppHandle, delta: i32) -> Option<u8> {
    let val = tauri::async_runtime::spawn_blocking(move || brightness::step(delta))
        .await
        .ok()
        .flatten()?;
    publish(&app, "brightness", json!({ "level": val, "silent": false }));
    Some(val)
}

pub(crate) fn toggle_mic_mute_global(app: &AppHandle) -> Option<bool> {
    let muted = audio::toggle_mic_mute()?;
    publish(app, "mic-mute", json!({ "muted": muted, "silent": false }));
    Some(muted)
}

/// Отчёт о плавности анимации из демо-режима (`--demo`): печатается только в отладочной сборке.
#[tauri::command]
fn perf(label: String, frames: u32, ms: u32, avg: f32, max: f32) {
    if cfg!(debug_assertions) {
        eprintln!("[perf] {label}: {frames} кадров за {ms} мс, в среднем {avg} мс, худший {max} мс");
    }
}

/// Помодоро завершён: +1 к счётчику за сегодня и в 30-дневную историю.
#[tauri::command]
fn pomodoro_done(app: AppHandle) {
    let today = sysinfo::today();
    let snapshot = {
        let shared = app.state::<Shared>();
        let mut s = lock(&shared.stats);
        settings::record_pomodoro(&mut s, &today);
        s.clone()
    };
    settings::save_stats(&app, &snapshot);
    publish_stats(&app);
}

/// Настройки для окна настроек: всё из settings.json плюс автозапуск и сводка помодоро.
#[tauri::command]
fn get_settings(state: tauri::State<Shared>) -> Value {
    let mut v = json!(*lock(&state.settings));
    let stats = lock(&state.stats);
    let (today, week, streak) = settings::stats_summary(&stats, &sysinfo::today());
    let (hk_toggle_ok, hk_timer_ok, hk_capsule_ok, hk_mic_ok) = hotkeys::status();
    v["autostart"] = json!(settings::autostart_enabled());
    v["version"] = json!(env!("CARGO_PKG_VERSION"));
    v["pomodoros_today"] = json!(today);
    v["pomodoros_week"] = json!(week);
    v["pomodoros_streak"] = json!(streak);
    v["hotkey_toggle_ok"] = json!(hk_toggle_ok);
    v["hotkey_timer_ok"] = json!(hk_timer_ok);
    v["hotkey_capsule_ok"] = json!(hk_capsule_ok);
    v["hotkey_mic_ok"] = json!(hk_mic_ok);
    if let Some(upd) = lock(&state.snap).get("update").cloned() {
        v["update"] = upd;
    }
    v
}

#[tauri::command]
fn set_setting(app: AppHandle, key: String, value: Value) -> Value {
    if key == "autostart" {
        settings::set_autostart(value.as_bool().unwrap_or(false));
    } else {
        let snapshot = {
            let shared = app.state::<Shared>();
            let mut s = lock(&shared.settings);
            let mut obj = json!(*s);
            obj[key.as_str()] = value;
            if let Ok(mut updated) = serde_json::from_value::<Settings>(obj) {
                updated.sound_volume = updated.sound_volume.min(100);
                updated.top_margin = updated.top_margin.clamp(0, 24);
                updated.ui_scale = updated.ui_scale.clamp(85, 115);
                updated.pomo_work_min = updated.pomo_work_min.clamp(5, 120);
                updated.pomo_break_min = updated.pomo_break_min.clamp(1, 60);
                if updated.monitor_mode != "cursor" {
                    updated.monitor_mode = "primary".to_string();
                }
                if !matches!(updated.lang.as_str(), "auto" | "ru" | "en") {
                    updated.lang = "auto".to_string();
                }
                if !matches!(updated.align.as_str(), "left" | "center" | "right") {
                    updated.align = "center".to_string();
                }
                if !matches!(updated.border_style.as_str(), "subtle" | "glow" | "none") {
                    updated.border_style = "subtle".to_string();
                }
                if !matches!(updated.backdrop.as_str(), "oled" | "graphite" | "glass") {
                    updated.backdrop = "oled".to_string();
                }
                if !matches!(updated.idle_style.as_str(), "pill" | "clock" | "hidden") {
                    updated.idle_style = "pill".to_string();
                }
                if !matches!(updated.expand_mode.as_str(), "hover" | "click" | "hotkey") {
                    updated.expand_mode = "hover".to_string();
                }
                if !matches!(updated.spring_preset.as_str(), "soft" | "normal" | "bouncy" | "snappy") {
                    updated.spring_preset = "normal".to_string();
                }
                if !matches!(updated.over_maximized.as_str(), "sliver" | "pill" | "hide") {
                    updated.over_maximized = "sliver".to_string();
                }
                if !matches!(updated.capsule_size.as_str(), "compact" | "normal" | "large") {
                    updated.capsule_size = "normal".to_string();
                }
                let valid_accent = updated.accent_color == "auto"
                    || (updated.accent_color.len() == 7
                        && updated.accent_color.starts_with('#')
                        && updated.accent_color[1..].bytes().all(|b| b.is_ascii_hexdigit()));
                if !valid_accent {
                    updated.accent_color = "auto".to_string();
                }
                if matches!(key.as_str(), "hotkey_toggle" | "hotkey_timer" | "hotkey_capsule" | "hotkey_mic") {
                    let vals = [
                        updated.hotkey_toggle.to_ascii_lowercase(),
                        updated.hotkey_timer.to_ascii_lowercase(),
                        updated.hotkey_capsule.to_ascii_lowercase(),
                        updated.hotkey_mic.to_ascii_lowercase(),
                    ];
                    let has_dup = (0..vals.len()).any(|i| vals[i + 1..].contains(&vals[i]));
                    if has_dup {
                        updated.hotkey_toggle = s.hotkey_toggle.clone();
                        updated.hotkey_timer = s.hotkey_timer.clone();
                        updated.hotkey_capsule = s.hotkey_capsule.clone();
                        updated.hotkey_mic = s.hotkey_mic.clone();
                    }
                }
                *s = updated;
            }
            s.clone()
        };
        settings::save(&app, &snapshot);
        if matches!(key.as_str(), "monitor_mode" | "align") {
            if let Some(w) = app.get_webview_window("island") {
                if snapshot.monitor_mode == "primary" {
                    place(&w);
                } else if let Ok(Some(m)) = w.current_monitor().or_else(|_| w.primary_monitor()) {
                    place_on(&w, &m, &snapshot.align);
                }
                let _ = app.emit("screen", ());
            }
        }
        if matches!(key.as_str(), "lang" | "capsule_only") {
            sync_tray_labels(&app);
        }
        if matches!(key.as_str(), "hotkeys" | "hotkey_toggle" | "hotkey_timer" | "hotkey_capsule" | "hotkey_mic") {
            hotkeys::sync();
        }
        publish_settings(&app);
    }
    get_settings(app.state::<Shared>())
}

#[tauri::command]
fn reset_settings(app: AppHandle) -> Value {
    let defaults = Settings::default();
    {
        let shared = app.state::<Shared>();
        *lock(&shared.settings) = defaults.clone();
    }
    settings::save(&app, &defaults);
    if let Some(w) = app.get_webview_window("island") {
        place(&w);
        let _ = app.emit("screen", ());
    }
    sync_tray_labels(&app);
    hotkeys::sync();
    publish_settings(&app);
    get_settings(app.state::<Shared>())
}

static SETTINGS_OPENING: AtomicBool = AtomicBool::new(false);

#[tauri::command]
async fn open_settings(app: AppHandle) {
    show_settings(&app);
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    if SETTINGS_OPENING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    // На Windows создание WebviewWindow обязательно выполняется вне главного UI-потока,
    // иначе CreateCoreWebView2Controller блокирует очередь сообщений (серый экран и зависание).
    let handle = app.clone();
    std::thread::spawn(move || {
        let built = WebviewWindowBuilder::new(&handle, "settings", WebviewUrl::App("settings.html".into()))
            .title("Настройки Dynamic Island")
            .inner_size(468.0, 660.0)
            .resizable(false)
            .maximizable(false)
            .decorations(false)
            .center()
            .visible(false)
            .background_color(tauri::window::Color(11, 11, 13, 255))
            .build();
        SETTINGS_OPENING.store(false, Ordering::SeqCst);
        if let Ok(w) = built {
            std::thread::sleep(std::time::Duration::from_millis(160));
            let _ = w.show();
            let _ = w.set_focus();
        }
    });
}

/// Вычисляет целевые физические координаты окна острова у верхнего края монитора.
pub(crate) fn expected_pos(win: &WebviewWindow, mon: &Monitor, align: &str) -> Option<(i32, i32)> {
    let size = win.outer_size().ok()?;
    let mx = mon.position().x;
    let mw = mon.size().width as i32;
    let sw = size.width as i32;
    let pad = (12.0 * mon.scale_factor()).round() as i32;
    let x = match align {
        "left" => mx + pad,
        "right" => mx + mw - sw - pad,
        _ => mx + (mw - sw) / 2,
    };
    let wa_top = mon.work_area().position.y;
    let y = if wa_top > mon.position().y { wa_top } else { mon.position().y };
    Some((x, y))
}

/// Ставит окно у верхнего края указанного монитора с учётом выравнивания (`left`/`center`/`right`) и верхней панели задач.
pub(crate) fn place_on(win: &WebviewWindow, mon: &Monitor, align: &str) {
    let Some((x, y)) = expected_pos(win, mon, align) else { return };
    let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    hover::raise_topmost(win);
    if let Some(shared) = win.app_handle().try_state::<Shared>() {
        shared.geom_dirty.store(true, Ordering::Relaxed);
    }
}

/// Ставит окно у верхнего края основного монитора согласно настройке `align`.
pub(crate) fn place(win: &WebviewWindow) {
    let Ok(Some(mon)) = win.primary_monitor() else { return };
    let align = lock(&win.app_handle().state::<Shared>().settings).align.clone();
    place_on(win, &mon, &align);
}

pub(crate) fn toggle_capsule_mode(app: &AppHandle) {
    let (snapshot, on) = {
        let shared = app.state::<Shared>();
        let mut s = lock(&shared.settings);
        s.capsule_only = !s.capsule_only;
        (s.clone(), s.capsule_only)
    };
    settings::save(app, &snapshot);
    sync_tray_labels(app);
    publish_settings(app);
    let _ = app.emit("capsule-mode", on);
}

fn sync_tray_labels(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let (en, capsule) = {
        let s = lock(&shared.settings);
        (s.effective_lang() == "en", s.capsule_only)
    };
    let visible = !shared.user_hidden.load(Ordering::Relaxed);
    if let Some((settings_i, capsule_i, toggle_i, quit_i)) = lock(&shared.tray_items).as_ref() {
        let _ = settings_i.set_text(if en { "Settings…" } else { "Настройки…" });
        let _ = capsule_i.set_text(match (en, capsule) {
            (true, true) => "✓ Capsule Only Mode",
            (true, false) => "Capsule Only Mode",
            (false, true) => "✓ Режим «Просто капсула»",
            (false, false) => "Режим «Просто капсула»",
        });
        let _ = toggle_i.set_text(match (en, visible) {
            (true, true) => "Hide Island",
            (true, false) => "Show Island",
            (false, true) => "Скрыть остров",
            (false, false) => "Показать остров",
        });
        let _ = quit_i.set_text(if en { "Quit" } else { "Выход" });
    };
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let settings_i = MenuItem::with_id(app, "settings", "Настройки…", true, None::<&str>)?;
    let capsule_i = MenuItem::with_id(app, "capsule", "Режим «Просто капсула»", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Скрыть остров", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&settings_i, &capsule_i, &sep, &toggle, &quit])?;
    *lock(&app.state::<Shared>().tray_items) = Some((
        settings_i.clone(),
        capsule_i.clone(),
        toggle.clone(),
        quit.clone(),
    ));
    sync_tray_labels(app.handle());

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Dynamic Island")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_settings(tray.app_handle());
            }
        })
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "quit" => app.exit(0),
            "settings" => show_settings(app),
            "capsule" => toggle_capsule_mode(app),
            "toggle" => {
                if let Some(w) = app.get_webview_window("island") {
                    let shared = app.state::<Shared>();
                    let hide_now = !shared.user_hidden.load(Ordering::Relaxed);
                    shared.user_hidden.store(hide_now, Ordering::Relaxed);
                    if hide_now {
                        let _ = w.hide();
                    } else {
                        let _ = w.unminimize();
                        let _ = w.show();
                        place(&w);
                    }
                    sync_tray_labels(app);
                }
            }
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

static SINGLE_INSTANCE_MUTEX: AtomicIsize = AtomicIsize::new(0);

fn release_single_instance() {
    let raw = SINGLE_INSTANCE_MUTEX.swap(0, Ordering::SeqCst);
    if raw != 0 {
        unsafe {
            let _ = CloseHandle(HANDLE(raw as _));
        }
    }
}

/// Вторая копия острова не нужна: если он уже запущен, будим окно настроек у первой копии и выходим.
/// При `--relaunch` (после бесшовного самообновления) ждём до ~2 секунд выхода старого процесса.
fn already_running() -> bool {
    let relaunch = std::env::args().any(|a| a == "--relaunch");
    let attempts = if relaunch { 15 } else { 1 };
    unsafe {
        for i in 0..attempts {
            if let Ok(h) = CreateMutexW(None, true, w!("Local\\DynamicIsland.SingleInstance")) {
                if GetLastError() != ERROR_ALREADY_EXISTS {
                    SINGLE_INSTANCE_MUTEX.store(h.0 as isize, Ordering::SeqCst);
                    return false;
                }
                let _ = CloseHandle(h);
            }
            if relaunch && i + 1 < attempts {
                std::thread::sleep(std::time::Duration::from_millis(150));
                continue;
            }
            if !relaunch {
                if let Ok(ev) = OpenEventW(EVENT_MODIFY_STATE, false, w!("Local\\DynamicIsland.WakeSettings")) {
                    let _ = SetEvent(ev);
                    let _ = CloseHandle(ev);
                }
            }
            return true;
        }
        false
    }
}

pub fn run() {
    if already_running() {
        return;
    }
    updater::cleanup_old_exe();
    tauri::Builder::default()
        .manage(Shared::default())
        .invoke_handler(tauri::generate_handler![
            set_hit_rect,
            snapshot,
            media_action,
            media_seek,
            media_cycle,
            volume_step,
            volume_set,
            volume_mute_toggle,
            mic_mute_toggle,
            get_brightness,
            set_brightness,
            step_brightness,
            perf,
            pomodoro_done,
            get_settings,
            set_setting,
            reset_settings,
            open_settings,
            open_privacy_settings,
            open_sound_settings,
            get_audio_devices,
            set_audio_device,
            get_audio_sessions,
            set_app_volume,
            toggle_app_mute,
            check_update,
            install_update,
            open_release_url,
            shelf_remove,
            shelf_clear,
            shelf_drag,
            shelf_drag_all,
            shelf_reveal,
            shelf_open,
            shelf_from_clipboard,
            clip_copy,
            clip_drag,
            clip_pin,
            clips_clear
        ])
        .on_window_event(|win, event| {
            if win.label() == "island" {
                match event {
                    tauri::WindowEvent::ScaleFactorChanged { .. } => {
                        if let Some(w) = win.app_handle().get_webview_window("island") {
                            let align = lock(&win.app_handle().state::<Shared>().settings).align.clone();
                            if let Ok(Some(mon)) = w.current_monitor().or_else(|_| w.primary_monitor()) {
                                place_on(&w, &mon, &align);
                            }
                            let _ = win.app_handle().emit("screen", ());
                        }
                    }
                    tauri::WindowEvent::DragDrop(dd) => match dd {
                        tauri::DragDropEvent::Enter { .. } => {
                            let _ = win.app_handle().emit("drag-hover", true);
                        }
                        tauri::DragDropEvent::Leave => {
                            let _ = win.app_handle().emit("drag-hover", false);
                        }
                        tauri::DragDropEvent::Drop { paths, .. } => {
                            let app = win.app_handle();
                            let changed = {
                                let shared = app.state::<Shared>();
                                let mut sh = lock(&shared.shelf);
                                shelf::add_paths(&mut sh, paths.clone())
                            };
                            let _ = app.emit("drag-hover", false);
                            if changed {
                                publish_shelf(app);
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            *lock(&app.state::<Shared>().settings) = settings::load(&handle);
            *lock(&app.state::<Shared>().stats) = settings::load_stats(&handle, &sysinfo::today());
            let loaded_shelf = shelf::load(&handle);
            *lock(&app.state::<Shared>().shelf) = loaded_shelf.clone();
            publish_settings(&handle);
            publish_stats(&handle);
            publish(&handle, "shelf", json!(loaded_shelf));
            publish(&handle, "clips", json!([]));
            if cfg!(debug_assertions) && std::env::args().any(|a| a == "--demo") {
                publish(&handle, "demo", json!(true));
            }

            let win = app.get_webview_window("island").expect("нет окна острова");
            place(&win);
            let _ = win.set_ignore_cursor_events(true);
            // клик по острову не должен забирать фокус у окна, в котором вы печатаете
            let _ = win.set_focusable(false);

            hover::spawn(handle.clone());
            media::spawn(handle.clone());
            audio::spawn(handle.clone());
            power::spawn(handle.clone());
            privacy::spawn(handle.clone());
            sysinfo::spawn(handle.clone());
            keyboard::spawn(handle.clone());
            clipboard::spawn(handle.clone());
            hotkeys::spawn(handle.clone());

            // Повторный запуск ярлыка Dynamic Island открывает окно настроек у живого процесса.
            let wake_handle = handle.clone();
            std::thread::spawn(move || unsafe {
                if let Ok(ev) = CreateEventW(None, false, false, w!("Local\\DynamicIsland.WakeSettings")) {
                    loop {
                        if WaitForSingleObject(ev, u32::MAX) == WAIT_OBJECT_0 {
                            show_settings(&wake_handle);
                        } else {
                            break;
                        }
                    }
                }
            });

            // Тихая проверка новой версии на GitHub: старт через 10с (с ретраями, если сеть ещё не поднялась)
            // и далее периодически каждые 6 часов.
            std::thread::spawn(move || {
                let mut wait = std::time::Duration::from_secs(10);
                let mut retries = 0u8;
                loop {
                    std::thread::sleep(wait);
                    let info = updater::check(env!("CARGO_PKG_VERSION"));
                    if info.available {
                        publish(&handle, "update", json!(info));
                        wait = std::time::Duration::from_secs(6 * 3600);
                        retries = 0;
                    } else if info.error.as_deref() == Some("network") && retries < 2 {
                        retries += 1;
                        wait = std::time::Duration::from_secs(if retries == 1 { 60 } else { 300 });
                    } else {
                        wait = std::time::Duration::from_secs(6 * 3600);
                        retries = 0;
                    }
                }
            });

            build_tray(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("не удалось запустить Dynamic Island");
}
