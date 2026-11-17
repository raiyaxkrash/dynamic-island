//! Настройки (файл settings.json в папке программы) и автозапуск через реестр.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    /// Показывать «Скопировано».
    pub clipboard: bool,
    /// Точки «микрофон / камера включены».
    pub privacy: bool,
    /// Звук, когда таймер закончился.
    pub sounds: bool,
    /// Плавные анимации. Системный флаг «уменьшить движение» намеренно не учитываем.
    pub animations: bool,
    /// Показывать подключение наушников и колонок.
    pub devices: bool,
    /// Загрузка процессора и памяти на домашнем экране.
    pub sysload: bool,
    /// Сколько держать курсор над полоской над вкладками, прежде чем остров раскроется (мс).
    pub dwell_ms: u32,
    /// Показывать смену раскладки и Caps Lock.
    pub keyboard: bool,
    /// Сворачивать остров, если музыка давно на паузе.
    pub hide_paused: bool,
    /// На каком мониторе жить: `"primary"` или `"cursor"`.
    pub monitor_mode: String,
    /// Менять громкость колёсиком по свёрнутому острову (если под ним нет развёрнутого окна).
    pub wheel_collapsed: bool,
    /// Громкость звука таймера (0..=100).
    pub sound_volume: u8,
    /// Глобальные горячие клавиши.
    pub hotkeys: bool,
    /// Язык интерфейса: `"auto"`, `"ru"` или `"en"`.
    pub lang: String,
    /// Положение по горизонтали: `"left"`, `"center"` или `"right"`.
    pub align: String,
    /// Отступ сверху экрана в CSS-пикселях (`0..=24`).
    pub top_margin: i32,
    /// Сочетание клавиш для открытия/сворачивания острова.
    pub hotkey_toggle: String,
    /// Сочетание клавиш для запуска/паузы таймера.
    pub hotkey_timer: String,
    /// Стиль обводки острова: `"subtle"`, `"glow"` или `"none"`.
    pub border_style: String,
    /// Не показывать второстепенные уведомления во время активного помодоро.
    pub quiet_pomo: bool,
    /// Ручной «Тихий режим» (🌙), сохраняемый между перезапусками.
    pub dnd: bool,
    /// Акцентный цвет: `"auto"` (из обложки) или hex (`"#0a84ff"`, `"#bf5af2"`, и т.д.).
    pub accent_color: String,
    /// Материал фона острова: `"oled"`, `"graphite"` или `"glass"`.
    pub backdrop: String,
    /// Масштаб острова в процентах (`90`, `100`, `110`).
    pub ui_scale: u8,
    /// Вид свёрнутого острова в покое: `"pill"`, `"clock"` или `"hidden"`.
    pub idle_style: String,
    /// Способ раскрытия: `"hover"`, `"click"` или `"hotkey"`.
    pub expand_mode: String,
    /// Характер пружины: `"soft"`, `"normal"`, `"bouncy"` или `"snappy"`.
    pub spring_preset: String,
    /// 24-часовой формат времени (`true` = `14:30`, `false` = `2:30 PM`).
    pub clock_24h: bool,
    /// Показывать секунды на часах.
    pub clock_seconds: bool,
    /// Длительность рабочей сессии помодоро (мин).
    pub pomo_work_min: u16,
    /// Длительность перерыва помодоро (мин).
    pub pomo_break_min: u16,
    /// Режим «Просто капсула»: чистая click-through капсула без раскрытий и всплывающих уведомлений.
    pub capsule_only: bool,
    /// Горячая клавиша переключения режима «Просто капсула».
    pub hotkey_capsule: String,
    /// Вид свёрнутого острова над развёрнутым окном: `"sliver"`, `"pill"` или `"hide"`.
    pub over_maximized: String,
}

impl Settings {
    pub fn effective_lang(&self) -> &'static str {
        match self.lang.as_str() {
            "ru" => "ru",
            "en" => "en",
            _ => {
                let primary = unsafe { GetUserDefaultUILanguage() } & 0x3ff;
                if primary == 0x19 { "ru" } else { "en" }
            }
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            clipboard: true,
            privacy: true,
            sounds: true,
            animations: true,
            devices: true,
            sysload: true,
            dwell_ms: 450,
            keyboard: true,
            hide_paused: true,
            monitor_mode: "primary".to_string(),
            wheel_collapsed: false,
            sound_volume: 70,
            hotkeys: true,
            lang: "auto".to_string(),
            align: "center".to_string(),
            top_margin: 6,
            hotkey_toggle: "Ctrl+Alt+I".to_string(),
            hotkey_timer: "Ctrl+Alt+T".to_string(),
            border_style: "subtle".to_string(),
            quiet_pomo: false,
            dnd: false,
            accent_color: "auto".to_string(),
            backdrop: "oled".to_string(),
            ui_scale: 100,
            idle_style: "pill".to_string(),
            expand_mode: "hover".to_string(),
            spring_preset: "normal".to_string(),
            clock_24h: true,
            clock_seconds: false,
            pomo_work_min: 25,
            pomo_break_min: 5,
            capsule_only: false,
            hotkey_capsule: "Ctrl+Alt+M".to_string(),
            over_maximized: "sliver".to_string(),
        }
    }
}

/// Сколько помодоро сделано сегодня и за последние 30 дней (`stats.json` рядом с настройками).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Stats {
    pub date: String,
    pub pomodoros: u32,
    pub history: BTreeMap<String, u32>,
}

fn stats_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("stats.json"))
}

pub fn roll_stats(s: &mut Stats, today: &str) -> bool {
    if !s.date.is_empty() && s.pomodoros > 0 {
        s.history.entry(s.date.clone()).or_insert(s.pomodoros);
    }
    if s.date == today {
        return false;
    }
    s.date = today.to_string();
    s.pomodoros = *s.history.get(today).unwrap_or(&0);
    while s.history.len() > 30 {
        if let Some(oldest) = s.history.keys().next().cloned() {
            s.history.remove(&oldest);
        } else {
            break;
        }
    }
    true
}

pub fn record_pomodoro(s: &mut Stats, today: &str) {
    roll_stats(s, today);
    s.pomodoros += 1;
    s.history.insert(today.to_string(), s.pomodoros);
}

/// Возвращает `(за_сегодня, за_последние_7_календарных_дней, текущая_серия_дней)`.
pub fn stats_summary(s: &Stats, today: &str) -> (u32, u32, u32) {
    let today_count = if s.date == today { s.pomodoros } else { *s.history.get(today).unwrap_or(&0) };
    let count_of = |d: (i32, u32, u32)| -> u32 {
        let k = format!("{:04}-{:02}-{:02}", d.0, d.1, d.2);
        if k == today { today_count } else { *s.history.get(&k).unwrap_or(&0) }
    };

    let mut week = 0u32;
    let mut streak = 0u32;
    if let Some(start) = parse_ymd(today) {
        let mut cur = Some(start);
        for _ in 0..7 {
            let Some(d) = cur else { break };
            week += count_of(d);
            cur = prev_day(d);
        }

        let mut day = start;
        if count_of(day) == 0 {
            if let Some(prev) = prev_day(day) {
                day = prev;
            }
        }
        while count_of(day) > 0 {
            streak += 1;
            let Some(prev) = prev_day(day) else { break };
            day = prev;
        }
    } else {
        week = today_count;
    }
    (today_count, week, streak)
}

fn parse_ymd(s: &str) -> Option<(i32, u32, u32)> {
    let mut it = s.split('-');
    let y = it.next()?.parse().ok()?;
    let m = it.next()?.parse().ok()?;
    let d = it.next()?.parse().ok()?;
    if (1..=12).contains(&m) && (1..=31).contains(&d) { Some((y, m, d)) } else { None }
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 }
        }
        _ => 30,
    }
}

fn prev_day((y, m, d): (i32, u32, u32)) -> Option<(i32, u32, u32)> {
    if d > 1 {
        Some((y, m, d - 1))
    } else if m > 1 {
        Some((y, m - 1, days_in_month(y, m - 1)))
    } else if y > 1 {
        Some((y - 1, 12, 31))
    } else {
        None
    }
}

pub fn load_stats(app: &AppHandle, today: &str) -> Stats {
    let mut s: Stats = stats_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    roll_stats(&mut s, today);
    s
}

pub fn save_stats(app: &AppHandle, s: &Stats) {
    if let Some(p) = stats_path(app) {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, serde_json::to_string(s).unwrap_or_default());
    }
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("settings.json"))
}

pub fn load(app: &AppHandle) -> Settings {
    path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(app: &AppHandle, s: &Settings) {
    if let Some(p) = path(app) {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, serde_json::to_string_pretty(s).unwrap_or_default());
    }
}

const RUN: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const NAME: PCWSTR = w!("DynamicIsland");

fn autostart_matches_exe(reg_val: &str, exe: &Path) -> bool {
    let trimmed = reg_val.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(trimmed);
    !unquoted.is_empty() && unquoted.eq_ignore_ascii_case(&exe.to_string_lossy())
}

pub fn autostart_enabled() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    unsafe {
        let mut bytes: u32 = 0;
        if RegGetValueW(HKEY_CURRENT_USER, RUN, NAME, RRF_RT_REG_SZ, None, None, Some(&mut bytes)).is_err()
            || bytes == 0
        {
            return false;
        }
        let mut buf = vec![0u16; (bytes as usize + 1) / 2];
        if RegGetValueW(
            HKEY_CURRENT_USER,
            RUN,
            NAME,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut bytes),
        )
        .is_err()
        {
            return false;
        }
        let len = (bytes as usize / 2).min(buf.len());
        let end = buf[..len].iter().position(|&c| c == 0).unwrap_or(len);
        let val = String::from_utf16_lossy(&buf[..end]);
        autostart_matches_exe(&val, &exe)
    }
}

pub fn set_autostart(on: bool) {
    unsafe {
        if on {
            let Ok(exe) = std::env::current_exe() else { return };
            let cmd = HSTRING::from(format!("\"{}\"", exe.display()));
            let bytes = (cmd.len() + 1) * 2;
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN, NAME, REG_SZ.0, Some(cmd.as_ptr() as *const _), bytes as u32);
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, NAME);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_path_matching() {
        let exe = Path::new(r"C:\Program Files\Dynamic Island\dynamic-island.exe");
        assert!(autostart_matches_exe(r#""C:\Program Files\Dynamic Island\dynamic-island.exe""#, exe));
        assert!(autostart_matches_exe(r"c:\program files\dynamic island\dynamic-island.exe", exe));
        assert!(!autostart_matches_exe(r#""C:\dynamic-island\src-tauri\target\release\dynamic-island.exe""#, exe));
        assert!(!autostart_matches_exe("", exe));
    }

    #[test]
    fn pomodoro_history_and_streak() {
        let mut s = Stats::default();
        s.history.insert("2026-11-15".to_string(), 2);
        s.history.insert("2026-11-16".to_string(), 3);
        record_pomodoro(&mut s, "2026-11-17");
        record_pomodoro(&mut s, "2026-11-17");
        assert_eq!(stats_summary(&s, "2026-11-17"), (2, 7, 3));
        assert!(roll_stats(&mut s, "2026-11-18"));
        assert_eq!(s.pomodoros, 0);
        // Вчерашняя серия ещё жива до конца сегодняшнего дня.
        assert_eq!(stats_summary(&s, "2026-11-18"), (0, 7, 3));

        // Давняя запись за пределами 7 календарных дней не должна попадать в week.
        s.history.insert("2026-11-01".to_string(), 10);
        assert_eq!(stats_summary(&s, "2026-11-18"), (0, 7, 3));
    }
}
