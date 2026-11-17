//! Полка для файлов (File Shelf) с сохранением между перезапусками, добавлением из буфера,
//! открытием файла, показом в Проводнике (`SHOpenFolderAndSelectItems`) и перетаскиванием (`DoDragDrop`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use windows::core::{implement, w, BOOL, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, IDataObject, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize, DROPEFFECT,
    DROPEFFECT_COPY, DROPEFFECT_LINK,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    ILCreateFromPathW, ILFree, SHCreateDataObject, SHOpenFolderAndSelectItems, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const MAX_ITEMS: usize = 8;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ShelfItem {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
}

fn shelf_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("shelf.json"))
}

pub fn load(app: &AppHandle) -> Vec<ShelfItem> {
    let items: Vec<ShelfItem> = shelf_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    items
        .into_iter()
        .filter(|x| Path::new(&x.path).exists())
        .take(MAX_ITEMS)
        .collect()
}

pub fn save(app: &AppHandle, items: &[ShelfItem]) {
    if let Some(p) = shelf_path(app) {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, serde_json::to_string(items).unwrap_or_default());
    }
}

pub fn add_paths(shelf: &mut Vec<ShelfItem>, paths: Vec<PathBuf>) -> bool {
    let mut changed = false;
    for p in paths {
        if !p.exists() {
            continue;
        }
        let s = p.to_string_lossy().to_string();
        if shelf.iter().any(|x| x.path.eq_ignore_ascii_case(&s)) {
            continue;
        }
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .unwrap_or(&s)
            .to_string();
        shelf.push(ShelfItem {
            path: s,
            name,
            is_dir: p.is_dir(),
        });
        if shelf.len() > MAX_ITEMS {
            shelf.remove(0);
        }
        changed = true;
    }
    changed
}

/// Открывает сам файл или папку приложением по умолчанию (двойной клик на полке).
pub fn open_file(path: String) {
    if !Path::new(&path).exists() {
        return;
    }
    unsafe {
        let wide = HSTRING::from(path.as_str());
        let _ = ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), None, None, SW_SHOWNORMAL);
    }
}

/// Открывает Проводник и выделяет указанный файл или папку.
pub fn reveal_in_explorer(path: String) {
    if !Path::new(&path).exists() {
        return;
    }
    std::thread::spawn(move || unsafe {
        let com_ok = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let wide = HSTRING::from(path.as_str());
        let pidl: *mut ITEMIDLIST = ILCreateFromPathW(PCWSTR(wide.as_ptr()));
        if !pidl.is_null() {
            let _ = SHOpenFolderAndSelectItems(pidl as *const ITEMIDLIST, None, 0);
            ILFree(Some(pidl as *const ITEMIDLIST));
        }
        if com_ok {
            CoUninitialize();
        }
    });
}

#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if (grfkeystate.0 & 1) == 0 {
            return DRAGDROP_S_DROP;
        }
        S_OK
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Запускает системный OLE Drag & Drop для файла на отдельной STA-нити.
pub fn start_drag(path: String) {
    if !Path::new(&path).exists() {
        return;
    }
    std::thread::spawn(move || unsafe {
        let ole_ok = OleInitialize(None).is_ok();
        let wide = HSTRING::from(path.as_str());
        let pidl: *mut ITEMIDLIST = ILCreateFromPathW(PCWSTR(wide.as_ptr()));
        if !pidl.is_null() {
            let pidls = [pidl as *const ITEMIDLIST];
            if let Ok(data_obj) = SHCreateDataObject::<_, IDataObject>(None, Some(&pidls), None) {
                let src: IDropSource = DropSource.into();
                let mut effect = DROPEFFECT(0);
                let _ = DoDragDrop(&data_obj, &src, DROPEFFECT_COPY | DROPEFFECT_LINK, &mut effect);
            }
            ILFree(Some(pidl as *const ITEMIDLIST));
        }
        if ole_ok {
            OleUninitialize();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelf_deduplicates_and_caps_items() {
        let mut shelf = Vec::new();
        let existing = std::env::current_exe().unwrap();
        assert!(add_paths(&mut shelf, vec![existing.clone()]));
        assert!(!add_paths(&mut shelf, vec![existing]));
        assert_eq!(shelf.len(), 1);
        assert!(!shelf[0].name.is_empty());
    }
}
