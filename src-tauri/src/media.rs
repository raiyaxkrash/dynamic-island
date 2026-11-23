//! Что сейчас играет: системный медиаплеер Windows (GSMTC, событийная модель).
//!
//! Подписывается на `CurrentSessionChanged` и события сессии (`MediaPropertiesChanged`,
//! `PlaybackInfoChanged`, `TimelinePropertiesChanged`) вместо постоянного опроса.
//! Поддерживает переключение между активными сессиями и сжимает обложки через WIC до 128×128.

use std::sync::{mpsc, OnceLock};
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use tauri::AppHandle;
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    CurrentSessionChangedEventArgs, GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionMediaProperties as Props,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    MediaPropertiesChangedEventArgs, PlaybackInfoChangedEventArgs, TimelinePropertiesChangedEventArgs,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::{publish, wic};

/// Разница между эпохой Windows (1601) и Unix (1970) в сотнях наносекунд.
const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
const MAX_ART_PX: u32 = 128;

enum MediaCmd {
    Refresh,
    Action(String),
    Seek(f64),
    CycleSession,
}

static CMD_TX: OnceLock<mpsc::Sender<MediaCmd>> = OnceLock::new();

fn manager() -> windows::core::Result<Manager> {
    Manager::RequestAsync()?.join()
}

fn friendly(app_id: &str) -> String {
    let id = app_id.to_lowercase();
    let known = [
        ("spotify", "Spotify"),
        ("yandex", "Яндекс"),
        ("applemusic", "Apple Music"),
        ("itunes", "Apple Music"),
        ("tidal", "Tidal"),
        ("deezer", "Deezer"),
        ("msedge", "Edge"),
        ("brave", "Brave"),
        ("vivaldi", "Vivaldi"),
        ("arc", "Arc"),
        ("zen", "Zen"),
        ("chrome", "Chrome"),
        ("firefox", "Firefox"),
        ("opera", "Opera"),
        ("telegram", "Telegram"),
        ("zunemusic", "Медиаплеер"),
        ("wmplayer", "Media Player"),
        ("potplayer", "PotPlayer"),
        ("mpc-", "MPC"),
        ("mpv", "mpv"),
        ("vlc", "VLC"),
        ("aimp", "AIMP"),
        ("foobar", "foobar2000"),
        ("discord", "Discord"),
    ];
    for (needle, name) in known {
        if id.contains(needle) {
            return name.to_string();
        }
    }
    let tail = app_id.rsplit(['\\', '!']).next().unwrap_or(app_id);
    tail.trim_end_matches(".exe").to_string()
}

fn sanitize_image_mime(mime: &str) -> String {
    let trimmed = mime.trim();
    if trimmed.starts_with("image/")
        && trimmed.len() <= 64
        && trimmed[6..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    {
        trimmed.to_string()
    } else {
        "image/png".to_string()
    }
}

fn unix_ms_from_filetime(universal_time: i64) -> i64 {
    if universal_time <= EPOCH_DIFF {
        0
    } else {
        (universal_time - EPOCH_DIFF) / 10_000
    }
}

#[cfg(test)]
fn fit_128(w: u32, h: u32) -> (u32, u32) {
    let max_dim = w.max(h);
    if max_dim <= MAX_ART_PX || max_dim == 0 {
        return (w, h);
    }
    let scale = MAX_ART_PX as f64 / max_dim as f64;
    (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1))
}

/// Уменьшает обложку через встроенный WIC до 128×128 PNG, чтобы не гонять мегабайты по IPC.
fn downscale_wic(raw: &[u8]) -> Option<Vec<u8>> {
    wic::downscale_png(raw, MAX_ART_PX, 65_536)
}

fn art(props: &Props) -> Option<String> {
    let reference = props.Thumbnail().ok()?;
    let stream = reference.OpenReadAsync().ok()?.join().ok()?;
    let size = stream.Size().ok()? as u32;
    if size == 0 || size > 8_000_000 {
        return None;
    }
    let raw_mime = stream.ContentType().map(|h| h.to_string()).unwrap_or_default();
    let reader = DataReader::CreateDataReader(&stream).ok()?;
    reader.LoadAsync(size).ok()?.join().ok()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf).ok()?;

    let (final_bytes, mime) = match downscale_wic(&buf) {
        Some(png) => (png, "image/png".to_string()),
        None => (buf, sanitize_image_mime(&raw_mime)),
    };
    Some(format!(
        "data:{};base64,{}",
        mime,
        base64::engine::general_purpose::STANDARD.encode(&final_bytes)
    ))
}

struct Cache {
    key: String,
    art: Option<String>,
    art_retries: u8,
}

struct TrackedSession {
    session: Session,
    source: String,
    t_props: i64,
    t_play: i64,
    t_time: i64,
}

impl TrackedSession {
    fn attach(session: Session, source: String, tx: &mpsc::Sender<MediaCmd>) -> Self {
        let tx1 = tx.clone();
        let t_props = session
            .MediaPropertiesChanged(&TypedEventHandler::<Session, MediaPropertiesChangedEventArgs>::new(
                move |_, _| {
                    let _ = tx1.send(MediaCmd::Refresh);
                    Ok(())
                },
            ))
            .unwrap_or_default();
        let tx2 = tx.clone();
        let t_play = session
            .PlaybackInfoChanged(&TypedEventHandler::<Session, PlaybackInfoChangedEventArgs>::new(
                move |_, _| {
                    let _ = tx2.send(MediaCmd::Refresh);
                    Ok(())
                },
            ))
            .unwrap_or_default();
        let tx3 = tx.clone();
        let t_time = session
            .TimelinePropertiesChanged(&TypedEventHandler::<Session, TimelinePropertiesChangedEventArgs>::new(
                move |_, _| {
                    let _ = tx3.send(MediaCmd::Refresh);
                    Ok(())
                },
            ))
            .unwrap_or_default();
        Self { session, source, t_props, t_play, t_time }
    }
}

impl Drop for TrackedSession {
    fn drop(&mut self) {
        let _ = self.session.RemoveMediaPropertiesChanged(self.t_props);
        let _ = self.session.RemovePlaybackInfoChanged(self.t_play);
        let _ = self.session.RemoveTimelinePropertiesChanged(self.t_time);
    }
}

fn collect_sessions(mgr: &Manager) -> Vec<(Session, String)> {
    let Ok(list) = mgr.GetSessions() else { return Vec::new() };
    let mut out = Vec::new();
    let count = list.Size().unwrap_or(0);
    for i in 0..count {
        if let Ok(s) = list.GetAt(i) {
            if let Ok(src) = s.SourceAppUserModelId() {
                out.push((s, src.to_string()));
            }
        }
    }
    out
}

fn pick_session(
    mgr: &Manager,
    pinned_source: &mut Option<String>,
) -> Option<(Session, String, usize, usize)> {
    let sessions = collect_sessions(mgr);
    if sessions.is_empty() {
        *pinned_source = None;
        return None;
    }
    if let Some(pin) = pinned_source.as_deref() {
        if let Some(idx) = sessions.iter().position(|(_, src)| src == pin) {
            let (s, src) = &sessions[idx];
            return Some((s.clone(), src.clone(), sessions.len(), idx));
        }
        *pinned_source = None;
    }
    if let Ok(cur) = mgr.GetCurrentSession() {
        if let Ok(cur_src) = cur.SourceAppUserModelId() {
            let cur_src = cur_src.to_string();
            let idx = sessions.iter().position(|(_, src)| src == &cur_src).unwrap_or(0);
            return Some((cur, cur_src, sessions.len(), idx));
        }
    }
    let (s, src) = &sessions[0];
    Some((s.clone(), src.clone(), sessions.len(), 0))
}

fn read(
    session: &Session,
    source: &str,
    session_count: usize,
    session_index: usize,
    cache: &mut Cache,
) -> windows::core::Result<Value> {
    let props = session.TryGetMediaPropertiesAsync()?.join()?;
    let title = props.Title()?.to_string();
    let artist = props.Artist()?.to_string();

    let key = format!("{source}\u{1}{title}\u{1}{artist}");
    if key != cache.key {
        cache.key = key;
        cache.art = art(&props);
        cache.art_retries = 0;
    } else if cache.art.is_none() && cache.art_retries < 5 {
        cache.art_retries += 1;
        cache.art = art(&props);
    }

    let playing = session.GetPlaybackInfo()?.PlaybackStatus()? == Status::Playing;
    let tl = session.GetTimelineProperties()?;
    let start = tl.StartTime()?.Duration;
    let pos = (tl.Position()?.Duration - start) as f64 / 1e7;
    let dur = (tl.EndTime()?.Duration - start) as f64 / 1e7;
    let updated = tl
        .LastUpdatedTime()
        .ok()
        .map(|t| unix_ms_from_filetime(t.UniversalTime))
        .unwrap_or(0);
    let app = friendly(source);

    Ok(json!({
        "active": !title.is_empty(),
        "playing": playing,
        "title": title,
        "artist": artist,
        "pos": pos.max(0.0),
        "dur": dur.max(0.0),
        "updated": updated,
        "source": source,
        "app": app,
        "sessions": session_count,
        "sessionIndex": session_index,
    }))
}

pub fn spawn(app: AppHandle) {
    let (tx, rx) = mpsc::channel::<MediaCmd>();
    let _ = CMD_TX.set(tx.clone());

    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mgr = loop {
            match manager() {
                Ok(m) => break m,
                Err(e) => {
                    crate::log_err(Some(&app), &format!("GSMTC manager failed: {e}"));
                    std::thread::sleep(Duration::from_secs(3));
                }
            }
        };

        let tx_cur = tx.clone();
        let _cur_tok = mgr.CurrentSessionChanged(&TypedEventHandler::<Manager, CurrentSessionChangedEventArgs>::new(
            move |_, _| {
                let _ = tx_cur.send(MediaCmd::Refresh);
                Ok(())
            },
        ));

        let mut cache = Cache { key: String::new(), art: None, art_retries: 0 };
        let mut last: Option<Value> = None;
        let mut last_art: Option<Option<String>> = None;
        let mut pinned_source: Option<String> = None;
        let mut tracked: Option<TrackedSession> = None;

        let mut timeout = Duration::from_secs(2);
        let _ = tx.send(MediaCmd::Refresh);

        loop {
            // Умный таймаут: пока ждём обложку — 1 с, во время игры — 3 с, в простое/паузе — 30 с.
            let first = rx.recv_timeout(timeout).unwrap_or(MediaCmd::Refresh);
            let mut cmds = vec![first];
            while let Ok(more) = rx.try_recv() {
                cmds.push(more);
            }

            let mut need_refresh = false;
            for cmd in cmds {
                match cmd {
                    MediaCmd::Refresh => need_refresh = true,
                    MediaCmd::CycleSession => {
                        let sessions = collect_sessions(&mgr);
                        if sessions.len() > 1 {
                            let cur_src = tracked.as_ref().map(|t| t.source.as_str());
                            let idx = cur_src
                                .and_then(|cs| sessions.iter().position(|(_, s)| s == cs))
                                .unwrap_or(0);
                            let next_idx = (idx + 1) % sessions.len();
                            pinned_source = Some(sessions[next_idx].1.clone());
                        }
                        need_refresh = true;
                    }
                    MediaCmd::Action(what) => {
                        if let Some((session, _, _, _)) = pick_session(&mgr, &mut pinned_source) {
                            let _ = match what.as_str() {
                                "toggle" => session.TryTogglePlayPauseAsync().and_then(|op| op.join()),
                                "next" => session.TrySkipNextAsync().and_then(|op| op.join()),
                                "prev" => session.TrySkipPreviousAsync().and_then(|op| op.join()),
                                _ => Ok(false),
                            };
                        }
                        need_refresh = true;
                    }
                    MediaCmd::Seek(seconds) => {
                        if let Some((session, _, _, _)) = pick_session(&mgr, &mut pinned_source) {
                            if let Ok(tl) = session.GetTimelineProperties() {
                                if let Ok(start) = tl.StartTime() {
                                    let ticks = start.Duration + (seconds.max(0.0) * 1e7) as i64;
                                    let _ = session.TryChangePlaybackPositionAsync(ticks).and_then(|op| op.join());
                                }
                            }
                        }
                        need_refresh = true;
                    }
                }
            }

            if !need_refresh {
                continue;
            }

            let picked = pick_session(&mgr, &mut pinned_source);
            let v = match picked {
                Some((session, src, count, idx)) => {
                    if tracked.as_ref().map(|t| t.source.as_str()) != Some(src.as_str()) {
                        tracked = Some(TrackedSession::attach(session.clone(), src.clone(), &tx));
                    }
                    read(&session, &src, count, idx, &mut cache).unwrap_or_else(|_| {
                        cache.art = None;
                        cache.key.clear();
                        json!({ "active": false })
                    })
                }
                None => {
                    tracked = None;
                    cache.art = None;
                    cache.key.clear();
                    json!({ "active": false })
                }
            };

            let active = v.get("active").and_then(|b| b.as_bool()).unwrap_or(false);
            let playing = v.get("playing").and_then(|b| b.as_bool()).unwrap_or(false);
            let waiting_art = active && cache.art.is_none() && cache.art_retries < 5;
            timeout = if waiting_art {
                Duration::from_secs(1)
            } else if active && playing {
                Duration::from_secs(3)
            } else {
                Duration::from_secs(30)
            };

            if last_art.as_ref() != Some(&cache.art) {
                publish(&app, "art", json!(cache.art));
                last_art = Some(cache.art.clone());
            }
            if last.as_ref() != Some(&v) {
                publish(&app, "media", v.clone());
                last = Some(v);
            }
        }
    });
}

/// Кнопки управления: `toggle`, `next`, `prev`.
pub fn action(what: &str) {
    if let Some(tx) = CMD_TX.get() {
        let _ = tx.send(MediaCmd::Action(what.to_string()));
    }
}

/// Перемотка: `seconds` от начала трека.
pub fn seek(seconds: f64) {
    if let Some(tx) = CMD_TX.get() {
        let _ = tx.send(MediaCmd::Seek(seconds));
    }
}

/// Переключение между активными медиа-сессиями.
pub fn cycle_session() {
    if let Some(tx) = CMD_TX.get() {
        let _ = tx.send(MediaCmd::CycleSession);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_zero_and_before_epoch_return_zero() {
        assert_eq!(unix_ms_from_filetime(0), 0);
        assert_eq!(unix_ms_from_filetime(EPOCH_DIFF), 0);
        assert_eq!(unix_ms_from_filetime(EPOCH_DIFF + 1_234 * 10_000), 1_234);
    }

    #[test]
    fn mime_sanitization_blocks_non_image_or_css_breaking_chars() {
        assert_eq!(sanitize_image_mime(""), "image/png");
        assert_eq!(sanitize_image_mime("image/jpeg"), "image/jpeg");
        assert_eq!(sanitize_image_mime("image/svg+xml"), "image/svg+xml");
        assert_eq!(sanitize_image_mime("image/png\" )"), "image/png");
        assert_eq!(sanitize_image_mime("text/html"), "image/png");
    }

    #[test]
    fn fit_128_preserves_aspect_ratio() {
        assert_eq!(fit_128(100, 80), (100, 80));
        assert_eq!(fit_128(512, 256), (128, 64));
        assert_eq!(fit_128(300, 600), (64, 128));
        assert_eq!(fit_128(1000, 1), (128, 1));
    }

    #[test]
    fn friendly_app_names() {
        assert_eq!(friendly("Spotify.exe"), "Spotify");
        assert_eq!(friendly("C:\\Program Files\\Google\\Chrome\\chrome.exe"), "Chrome");
        assert_eq!(friendly("SomeApp!App.exe"), "App");
    }
}
