//! Проверка обновлений через GitHub Releases API (`WinHttp`, без ослабления CSP в WebView).

use serde::Serialize;
use serde_json::Value;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
    WINHTTP_FLAG_SECURE,
};

const REPO_HOST: &str = "api.github.com";
const REPO_PATH: &str = "/repos/raiyaxkrash/dynamic-island/releases/latest";
const RELEASES_PREFIX: &str = "https://github.com/raiyaxkrash/dynamic-island/";

#[derive(Serialize, Clone, Debug, Default)]
pub struct UpdateInfo {
    pub available: bool,
    pub version: String,
    pub current: String,
    pub url: String,
}

fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let clean = s.trim().trim_start_matches('v').split('-').next()?;
    let mut it = clean.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next().unwrap_or("0").parse().ok()?;
    let pat = it.next().unwrap_or("0").parse().ok()?;
    Some((maj, min, pat))
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
    Some(UpdateInfo {
        available: latest_ver > cur_ver,
        version: tag.trim_start_matches('v').to_string(),
        current: current.to_string(),
        url,
    })
}

fn fetch_latest_json() -> Option<String> {
    unsafe {
        let session = WinHttpOpen(
            w!("DynamicIslandUpdater/0.1"),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return None;
        }
        let host = HSTRING::from(REPO_HOST);
        let conn = WinHttpConnect(session, PCWSTR(host.as_ptr()), 443, 0);
        if conn.is_null() {
            let _ = WinHttpCloseHandle(session);
            return None;
        }
        let path = HSTRING::from(REPO_PATH);
        let req = WinHttpOpenRequest(
            conn,
            w!("GET"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if req.is_null() {
            let _ = WinHttpCloseHandle(conn);
            let _ = WinHttpCloseHandle(session);
            return None;
        }

        let headers: Vec<u16> = "Accept: application/vnd.github+json\r\n"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let ok = WinHttpSendRequest(req, Some(&headers[..headers.len() - 1]), None, 0, 0, 0).is_ok()
            && WinHttpReceiveResponse(req, std::ptr::null_mut()).is_ok();

        let mut out = Vec::new();
        if ok {
            loop {
                let mut avail = 0u32;
                if WinHttpQueryDataAvailable(req, &mut avail).is_err() || avail == 0 {
                    break;
                }
                let to_read = avail.min(16384);
                let mut chunk = vec![0u8; to_read as usize];
                let mut read = 0u32;
                if WinHttpReadData(req, chunk.as_mut_ptr() as *mut _, to_read, &mut read).is_err()
                    || read == 0
                {
                    break;
                }
                out.extend_from_slice(&chunk[..read as usize]);
                if out.len() > 256 * 1024 {
                    break;
                }
            }
        }

        let _ = WinHttpCloseHandle(req);
        let _ = WinHttpCloseHandle(conn);
        let _ = WinHttpCloseHandle(session);
        String::from_utf8(out).ok()
    }
}

pub fn check(current: &str) -> UpdateInfo {
    fetch_latest_json()
        .and_then(|json| parse_latest_release(&json, current))
        .unwrap_or_else(|| UpdateInfo {
            available: false,
            version: current.to_string(),
            current: current.to_string(),
            url: format!("{RELEASES_PREFIX}releases/latest"),
        })
}

pub fn allowed_release_url(url: &str) -> bool {
    url.starts_with(RELEASES_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_semver_and_validates_url() {
        let body = r#"{"tag_name":"v0.2.0","html_url":"https://github.com/raiyaxkrash/dynamic-island/releases/tag/v0.2.0"}"#;
        let info = parse_latest_release(body, "0.1.0").unwrap();
        assert!(info.available);
        assert_eq!(info.version, "0.2.0");

        let same = parse_latest_release(r#"{"tag_name":"v0.1.0","html_url":"https://evil.example"}"#, "0.1.0").unwrap();
        assert!(!same.available);
        assert!(same.url.starts_with(RELEASES_PREFIX));
    }
}
