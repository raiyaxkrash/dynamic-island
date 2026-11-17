//! Dynamic Island для Windows.
//!
//! Окно острова прозрачное и всегда поверх остальных. Клики сквозь него проходят,
//! пока остров не раскрыт: поток `hover` следит за курсором и включает приём мыши
//! только в прямоугольнике, который сообщает интерфейс.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use serde_json::{json, Map, Value};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Monitor, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

mod audio;
mod clipboard;
mod devices;
mod hotkeys;
mod hover;
mod keyboard;
mod media;
mod power;
mod privacy;
mod settings;
mod sysinfo;
mod winmsg;

use settings::{Settings, Stats};

/// Общее состояние: зона острова (в CSS-пикселях окна), настройки и последние снимки данных.
#[derive(Default)]
pub struct Shared {
    pub rect: Mutex<[f64; 4]>,
    /// Остров раскрыт и должен ловить мышь; иначе клики идут сквозь него.
    pub interactive: AtomicBool,
    pub settings: Mutex<Settings>,
    pub stats: Mutex<Stats>,
    pub snap: Mutex<Map<String, Value>>,
}

/// Захват мьютекса без каскадных паник при отравлении.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
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
fn volume_step(delta: f32) {
    audio::step(delta);
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
    v["autostart"] = json!(settings::autostart_enabled());
    v["version"] = json!(env!("CARGO_PKG_VERSION"));
    v["pomodoros_today"] = json!(today);
    v["pomodoros_week"] = json!(week);
    v["pomodoros_streak"] = json!(streak);
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
                if updated.monitor_mode != "cursor" {
                    updated.monitor_mode = "primary".to_string();
                }
                *s = updated;
            }
            s.clone()
        };
        settings::save(&app, &snapshot);
        if key == "monitor_mode" && snapshot.monitor_mode == "primary" {
            if let Some(w) = app.get_webview_window("island") {
                place(&w);
            }
        }
        if key == "hotkeys" {
            hotkeys::set_enabled(snapshot.hotkeys);
        }
        publish_settings(&app);
    }
    get_settings(app.state::<Shared>())
}

#[tauri::command]
fn open_settings(app: AppHandle) {
    show_settings(&app);
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("Настройки Dynamic Island")
        .inner_size(440.0, 640.0)
        .resizable(false)
        .maximizable(false)
        .decorations(false)
        .center()
        .background_color(tauri::window::Color(11, 11, 13, 255))
        .build();
}

/// Ставит окно по центру верхнего края указанного монитора (учитывая панель задач, если она сверху).
pub(crate) fn place_on(win: &WebviewWindow, mon: &Monitor) {
    let Ok(size) = win.outer_size() else { return };
    let x = mon.position().x + (mon.size().width as i32 - size.width as i32) / 2;
    let wa_top = mon.work_area().position.y;
    let y = if wa_top > mon.position().y { wa_top } else { mon.position().y };
    let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
}

/// Ставит окно по центру верхнего края основного монитора.
pub(crate) fn place(win: &WebviewWindow) {
    let Ok(Some(mon)) = win.primary_monitor() else { return };
    place_on(win, &mon);
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let settings_i = MenuItem::with_id(app, "settings", "Настройки…", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Скрыть остров", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&settings_i, &sep, &toggle, &quit])?;
    let toggle_i = toggle.clone();

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
            "toggle" => {
                if let Some(w) = app.get_webview_window("island") {
                    let visible = w.is_visible().unwrap_or(true);
                    if visible {
                        let _ = w.hide();
                    } else {
                        let _ = w.show();
                        place(&w);
                    }
                    let _ = toggle_i.set_text(if visible { "Показать остров" } else { "Скрыть остров" });
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

/// Вторая копия острова не нужна: если он уже запущен, тихо выходим.
fn already_running() -> bool {
    unsafe {
        let _ = CreateMutexW(None, true, w!("Local\\DynamicIsland.SingleInstance"));
        GetLastError() == ERROR_ALREADY_EXISTS
    }
}

pub fn run() {
    if already_running() {
        return;
    }
    tauri::Builder::default()
        .manage(Shared::default())
        .invoke_handler(tauri::generate_handler![
            set_hit_rect,
            snapshot,
            media_action,
            media_seek,
            media_cycle,
            volume_step,
            perf,
            pomodoro_done,
            get_settings,
            set_setting,
            open_settings,
            open_privacy_settings,
            open_sound_settings
        ])
        .on_window_event(|win, event| {
            if win.label() == "island" {
                if let tauri::WindowEvent::ScaleFactorChanged { .. } = event {
                    if let Some(w) = win.app_handle().get_webview_window("island") {
                        if let Ok(Some(mon)) = w.current_monitor().or_else(|_| w.primary_monitor()) {
                            place_on(&w, &mon);
                        }
                        let _ = win.app_handle().emit("screen", ());
                    }
                }
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            *lock(&app.state::<Shared>().settings) = settings::load(&handle);
            *lock(&app.state::<Shared>().stats) = settings::load_stats(&handle, &sysinfo::today());
            publish_settings(&handle);
            publish_stats(&handle);
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
            hotkeys::spawn(handle);

            build_tray(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("не удалось запустить Dynamic Island");
}
