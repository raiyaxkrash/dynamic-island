//! Проверка и 1-клик установка обновлений через GitHub Releases API (`WinHttp`, без ослабления CSP в WebView).

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const REPO_HOST: &str = "api.github.com";
const REPO_PATH: &str = "/repos/raiyaxkrash/dynamic-island/releases/latest";
const RELEASES_PREFIX: &str = "https://github.com/raiyaxkrash/dynamic-island/";
const MAX_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct UpdateInfo {
    pub available: bool,
    pub version: String,
    pub current: String,
    pub url: String,
    pub download_url: Option<String>,
    pub asset_name: Option<String>,
    pub error: Option<String>,
}

fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let clean = s.trim().trim_start_matches('v').split('-').next()?;
    let mut it = clean.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next().unwrap_or("0").parse().ok()?;
    let pat = it.next().unwrap_or("0").parse().ok()?;
    Some((maj, min, pat))
}

pub fn safe_asset_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.to_ascii_lowercase().ends_with(".exe")
        && !name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
        && !name.contains("..")
}

fn pick_installer_asset(v: &Value) -> (Option<String>, Option<String>) {
    let Some(assets) = v.get("assets").and_then(|a| a.as_array()) else {
        return (None, None);
    };
    let mut best: Option<(u8, String, String)> = None;
    for a in assets {
        let Some(name) = a.get("name").and_then(|n| n.as_str()) else { continue };
        let Some(dl) = a.get("browser_download_url").and_then(|u| u.as_str()) else { continue };
        if !safe_asset_name(name) || !allowed_release_url(dl) {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        let prio = if lower.ends_with("-setup.exe") {
            2
        } else if lower.ends_with(".exe") {
            1
        } else {
            0
        };
        if prio > 0 && best.as_ref().is_none_or(|(p, _, _)| prio > *p) {
            best = Some((prio, dl.to_string(), name.to_string()));
        }
    }
    match best {
        Some((_, dl, name)) => (Some(dl), Some(name)),
        None => (None, None),
    }
}

pub fn parse_latest_release(body: &str, current: &str) -> Option<UpdateInfo> {
    let v: Value = serde_json::from_str(body).ok()?;
    let tag = v.get("tag_name")?.as_str()?;
    let html_url = v.get("html_url").and_then(|u| u.as_str()).unwrap_or("");
    let url = if html_url.starts_with(RELEASES_PREFIX) {
        html_url.to_string()
    } else {
        format!("{RELEASES_PREFIX}releases/latest")
    };
    let latest_ver = parse_semver(tag)?;
    let cur_ver = parse_semver(current)?;
    let (download_url, asset_name) = pick_installer_asset(&v);
    Some(UpdateInfo {
        available: latest_ver > cur_ver,
        version: tag.trim_start_matches('v').to_string(),
        current: current.to_string(),
        url,
        download_url,
        asset_name,
        error: None,
    })
}

struct WinHttpHandle(*mut std::ffi::c_void);

impl WinHttpHandle {
    fn new(raw: *mut std::ffi::c_void) -> Option<Self> {
        if raw.is_null() { None } else { Some(Self(raw)) }
    }
}

impl Drop for WinHttpHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

unsafe fn open_session() -> Option<WinHttpHandle> {
    let ua = HSTRING::from(concat!("DynamicIslandUpdater/", env!("CARGO_PKG_VERSION")));
    WinHttpHandle::new(WinHttpOpen(
        PCWSTR(ua.as_ptr()),
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
        PCWSTR::null(),
        PCWSTR::null(),
        0,
    ))
    .or_else(|| {
        WinHttpHandle::new(WinHttpOpen(
            PCWSTR(ua.as_ptr()),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))
    })
}

fn parse_https_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    if host.is_empty() || host.contains(['@', ' ', ':']) {
        return None;
    }
    Some((host.to_string(), path.to_string()))
}

fn fetch_latest_json() -> Result<String, &'static str> {
    unsafe {
        let session = open_session().ok_or("network")?;
        let _ = WinHttpSetTimeouts(session.0, 4000, 4000, 5000, 5000);
        let host = HSTRING::from(REPO_HOST);
        let conn = WinHttpHandle::new(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0))
            .ok_or("network")?;
        let path = HSTRING::from(REPO_PATH);
        let req = WinHttpHandle::new(WinHttpOpenRequest(
            conn.0,
            w!("GET"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))
        .ok_or("network")?;

        let headers: Vec<u16> = "Accept: application/vnd.github+json\r\n"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let ok = WinHttpSendRequest(req.0, Some(&headers[..headers.len() - 1]), None, 0, 0, 0).is_ok()
            && WinHttpReceiveResponse(req.0, std::ptr::null_mut()).is_ok();

        if !ok {
            return Err("network");
        }

        let mut status: u32 = 0;
        let mut status_len = std::mem::size_of::<u32>() as u32;
        let status_ok = WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut status_len,
            std::ptr::null_mut(),
        )
        .is_ok();

        if status_ok && (status == 403 || status == 429) {
            return Err("rate_limit");
        }
        if !status_ok || status != 200 {
            return Err("network");
        }

        let mut out = Vec::new();
        loop {
            let mut avail = 0u32;
            if WinHttpQueryDataAvailable(req.0, &mut avail).is_err() || avail == 0 {
                break;
            }
            let to_read = avail.min(16384);
            let mut chunk = vec![0u8; to_read as usize];
            let mut read = 0u32;
            if WinHttpReadData(req.0, chunk.as_mut_ptr() as *mut _, to_read, &mut read).is_err()
                || read == 0
            {
                break;
            }
            out.extend_from_slice(&chunk[..read as usize]);
            if out.len() > 256 * 1024 {
                break;
            }
        }

        String::from_utf8(out).map_err(|_| "parse")
    }
}

/// Скачивает установщик с GitHub Releases в `%TEMP%\DynamicIslandUpdate\` и запускает его.
pub fn download_and_install(
    url: &str,
    filename: &str,
    mut on_progress: impl FnMut(u8),
) -> Result<(), &'static str> {
    if !allowed_release_url(url) || !safe_asset_name(filename) {
        return Err("invalid_url");
    }
    let (host_s, path_s) = parse_https_url(url).ok_or("invalid_url")?;

    let dir: PathBuf = std::env::temp_dir().join("DynamicIslandUpdate");
    std::fs::create_dir_all(&dir).map_err(|_| "io")?;
    let part_path = dir.join(format!("{filename}.part"));
    let final_path = dir.join(filename);
    let _ = std::fs::remove_file(&part_path);

    unsafe {
        let session = open_session().ok_or("network")?;
        let _ = WinHttpSetTimeouts(session.0, 8000, 10000, 15000, 30000);
        let host = HSTRING::from(host_s.as_str());
        let conn = WinHttpHandle::new(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0))
            .ok_or("network")?;
        let path = HSTRING::from(path_s.as_str());
        let req = WinHttpHandle::new(WinHttpOpenRequest(
            conn.0,
            w!("GET"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))
        .ok_or("network")?;

        let ok = WinHttpSendRequest(req.0, None, None, 0, 0, 0).is_ok()
            && WinHttpReceiveResponse(req.0, std::ptr::null_mut()).is_ok();
        if !ok {
            return Err("network");
        }

        let mut status: u32 = 0;
        let mut status_len = std::mem::size_of::<u32>() as u32;
        let status_ok = WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut status_len,
            std::ptr::null_mut(),
        )
        .is_ok();
        if !status_ok || status != 200 {
            return Err("network");
        }

        let mut content_len: u32 = 0;
        let mut cl_size = std::mem::size_of::<u32>() as u32;
        let total = if WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut content_len as *mut u32 as *mut _),
            &mut cl_size,
            std::ptr::null_mut(),
        )
        .is_ok()
        {
            content_len as u64
        } else {
            0
        };
        if total > MAX_DOWNLOAD_BYTES {
            return Err("too_large");
        }

        let mut file = std::fs::File::create(&part_path).map_err(|_| "io")?;
        let mut downloaded: u64 = 0;
        let mut magic = [0u8; 2];
        let mut magic_len = 0usize;
        let mut last_pct: u8 = 0;
        on_progress(1);

        loop {
            let mut avail = 0u32;
            if WinHttpQueryDataAvailable(req.0, &mut avail).is_err() || avail == 0 {
                break;
            }
            let to_read = avail.min(65536);
            let mut chunk = vec![0u8; to_read as usize];
            let mut read = 0u32;
            if WinHttpReadData(req.0, chunk.as_mut_ptr() as *mut _, to_read, &mut read).is_err()
                || read == 0
            {
                break;
            }
            let slice = &chunk[..read as usize];
            if magic_len < 2 {
                let take = (2 - magic_len).min(slice.len());
                magic[magic_len..magic_len + take].copy_from_slice(&slice[..take]);
                magic_len += take;
            }
            downloaded += slice.len() as u64;
            if downloaded > MAX_DOWNLOAD_BYTES {
                let _ = std::fs::remove_file(&part_path);
                return Err("too_large");
            }
            file.write_all(slice).map_err(|_| "io")?;
            if let Some(raw_pct) = (downloaded * 100).checked_div(total) {
                let pct = raw_pct.min(99) as u8;
                if pct != last_pct {
                    last_pct = pct;
                    on_progress(pct);
                }
            }
        }
        drop(file);

        if downloaded < 64 * 1024 || &magic != b"MZ" {
            let _ = std::fs::remove_file(&part_path);
            return Err("invalid_file");
        }
        let _ = std::fs::remove_file(&final_path);
        std::fs::rename(&part_path, &final_path).map_err(|_| "io")?;
        on_progress(100);

        let wide = HSTRING::from(final_path.to_string_lossy().as_ref());
        let res = ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), None, None, SW_SHOWNORMAL);
        if (res.0 as usize) <= 32 {
            return Err("launch");
        }
        Ok(())
    }
}

pub fn check(current: &str) -> UpdateInfo {
    match fetch_latest_json() {
        Ok(json) => parse_latest_release(&json, current).unwrap_or_else(|| UpdateInfo {
            available: false,
            version: current.to_string(),
            current: current.to_string(),
            url: format!("{RELEASES_PREFIX}releases/latest"),
            download_url: None,
            asset_name: None,
            error: Some("parse".to_string()),
        }),
        Err(code) => UpdateInfo {
            available: false,
            version: current.to_string(),
            current: current.to_string(),
            url: format!("{RELEASES_PREFIX}releases/latest"),
            download_url: None,
            asset_name: None,
            error: Some(code.to_string()),
        },
    }
}

pub fn allowed_release_url(url: &str) -> bool {
    url.starts_with(RELEASES_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_semver_and_validates_url() {
        let body = r#"{
            "tag_name":"v0.7.0",
            "html_url":"https://github.com/raiyaxkrash/dynamic-island/releases/tag/v0.7.0",
            "assets":[
                {"name":"dynamic-island-0.7.0-x64-portable.exe","browser_download_url":"https://github.com/raiyaxkrash/dynamic-island/releases/download/v0.7.0/dynamic-island-0.7.0-x64-portable.exe"},
                {"name":"Dynamic-Island-0.7.0-x64-setup.exe","browser_download_url":"https://github.com/raiyaxkrash/dynamic-island/releases/download/v0.7.0/Dynamic-Island-0.7.0-x64-setup.exe"}
            ]
        }"#;
        let info = parse_latest_release(body, "0.6.1").unwrap();
        assert!(info.available);
        assert_eq!(info.version, "0.7.0");
        assert_eq!(info.asset_name.as_deref(), Some("Dynamic-Island-0.7.0-x64-setup.exe"));
        assert!(info.download_url.as_deref().unwrap().ends_with("-setup.exe"));
        assert!(info.error.is_none());

        let same = parse_latest_release(
            r#"{"tag_name":"v0.1.0","html_url":"https://evil.example"}"#,
            "0.1.0",
        )
        .unwrap();
        assert!(!same.available);
        assert!(same.url.starts_with(RELEASES_PREFIX));
        assert!(same.download_url.is_none());
    }

    #[test]
    fn validates_asset_names_and_urls() {
        assert!(safe_asset_name("Dynamic-Island-0.7.0-x64-setup.exe"));
        assert!(!safe_asset_name("..\\evil.exe"));
        assert!(!safe_asset_name("script.bat"));
        assert_eq!(
            parse_https_url("https://github.com/a/b"),
            Some(("github.com".to_string(), "/a/b".to_string()))
        );
        assert_eq!(parse_https_url("http://github.com/a/b"), None);
    }
}
