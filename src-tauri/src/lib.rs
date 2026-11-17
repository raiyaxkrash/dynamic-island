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
mod shelf;
mod sysinfo;
mod updater;
mod winmsg;

use settings::{Settings, Stats};
use shelf::ShelfItem;

type TrayItems = (MenuItem<tauri::Wry>, MenuItem<tauri::Wry>, MenuItem<tauri::Wry>);

/// Общее состояние: зона острова (в CSS-пикселях окна), настройки и последние снимки данных.
#[derive(Default)]
pub struct Shared {
    pub rect: Mutex<[f64; 4]>,
    /// Остров раскрыт и должен ловить мышь; иначе клики идут сквозь него.
    pub interactive: AtomicBool,
    /// Позиция или масштаб окна изменились — потоку `hover` пора перечитать геометрию.
    pub geom_dirty: AtomicBool,
    pub settings: Mutex<Settings>,
    pub stats: Mutex<Stats>,
    pub shelf: Mutex<Vec<ShelfItem>>,
    pub clips: Mutex<Vec<String>>,
    pub tray_items: Mutex<Option<TrayItems>>,
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
fn get_audio_devices() -> Value {
    json!(audio::list_devices())
}

#[tauri::command]
fn set_audio_device(id: String) -> bool {
    audio::set_default_device(&id)
}

#[tauri::command]
fn check_update(app: AppHandle) -> Value {
    let info = updater::check(env!("CARGO_PKG_VERSION"));
    if info.available {
        publish(&app, "update", json!(info));
    }
    json!(info)
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
fn shelf_reveal(path: String) {
    shelf::reveal_in_explorer(path);
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
fn clip_copy(text: String) -> bool {
    clipboard::write_text(&text)
}

#[tauri::command]
fn clips_clear(app: AppHandle) {
    lock(&app.state::<Shared>().clips).clear();
    publish(&app, "clips", json!([]));
}

#[tauri::command]
fn volume_step(delta: f32) {
    audio::step(delta);
}

#[tauri::command]
fn volume_mute_toggle() {
    audio::toggle_mute();
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
                updated.top_margin = updated.top_margin.clamp(0, 24);
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
        if key == "lang" {
            sync_tray_labels(&app);
        }
        if matches!(key.as_str(), "hotkeys" | "hotkey_toggle" | "hotkey_timer") {
            hotkeys::sync();
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

/// Ставит окно у верхнего края указанного монитора с учётом выравнивания (`left`/`center`/`right`) и верхней панели задач.
pub(crate) fn place_on(win: &WebviewWindow, mon: &Monitor, align: &str) {
    let Ok(size) = win.outer_size() else { return };
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
    let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
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

fn sync_tray_labels(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let en = lock(&shared.settings).effective_lang() == "en";
    let visible = app
        .get_webview_window("island")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(true);
    if let Some((settings_i, toggle_i, quit_i)) = lock(&shared.tray_items).as_ref() {
        let _ = settings_i.set_text(if en { "Settings…" } else { "Настройки…" });
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
    let toggle = MenuItem::with_id(app, "toggle", "Скрыть остров", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&settings_i, &sep, &toggle, &quit])?;
    *lock(&app.state::<Shared>().tray_items) = Some((settings_i.clone(), toggle.clone(), quit.clone()));
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
            "toggle" => {
                if let Some(w) = app.get_webview_window("island") {
                    let visible = w.is_visible().unwrap_or(true);
                    if visible {
                        let _ = w.hide();
                    } else {
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
            volume_mute_toggle,
            perf,
            pomodoro_done,
            get_settings,
            set_setting,
            open_settings,
            open_privacy_settings,
            open_sound_settings,
            get_audio_devices,
            set_audio_device,
            check_update,
            open_release_url,
            shelf_remove,
            shelf_clear,
            shelf_drag,
            shelf_reveal,
            shelf_from_clipboard,
            clip_copy,
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

            // Тихая проверка новой версии на GitHub через 10 секунд после старта.
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(10));
                let info = updater::check(env!("CARGO_PKG_VERSION"));
                if info.available {
                    publish(&handle, "update", json!(info));
                }
            });

            build_tray(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("не удалось запустить Dynamic Island");
}
