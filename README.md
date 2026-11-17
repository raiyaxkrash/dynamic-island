<div align="center">

# Dynamic Island для Windows

**Лёгкая, нативная «пилюля» сверху экрана в стиле iPhone для Windows 10 и 11.**  
Работает поверх всех окон, пропускает клики сквозь себя в свёрнутом виде и построена на **Tauri 2 + Rust + Win32/WinRT** с пружинной физикой на 60 FPS.

[![Windows](https://img.shields.io/badge/Windows-10%20%2F%2011-0078D6?style=flat-square&logo=windows)](https://github.com/raiyaxkrash/dynamic-island/releases/latest)
[![Tauri 2](https://img.shields.io/badge/Tauri-2.x-FFC131?style=flat-square&logo=tauri&logoColor=black)](https://tauri.app/)
[![Rust](https://img.shields.io/badge/Rust-Win32%20%2F%20WinRT-DEA584?style=flat-square&logo=rust&logoColor=black)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-green?style=flat-square)](LICENSE)
[![Release](https://img.shields.io/github/v/release/raiyaxkrash/dynamic-island?style=flat-square&label=Скачать)](https://github.com/raiyaxkrash/dynamic-island/releases/latest)

<h3>

[⬇️ Скачать установщик (`Dynamic-Island-0.2.0-x64-setup.exe`)](https://github.com/raiyaxkrash/dynamic-island/releases/latest)

</h3>

> **Примечание о Windows SmartScreen:** установщик пока не подписан сертификатом (Authenticode), поэтому при первом запуске Windows может показать предупреждение «Система Windows защитила ваш компьютер» — нажмите **Подробнее → Выполнить в любом случае** (либо соберите из исходников командами ниже).

</div>

---

## Интерфейс

<div align="center">

| Плеер с адаптивным эквалайзером | Домашний экран, помодоро и система |
| :---: | :---: |
| ![Плеер Dynamic Island](assets/screenshots/music.png) | ![Домашний экран и помодоро](assets/screenshots/home.png) |

| Переключатель вывода звука | Полка файлов (Drag & Drop) |
| :---: | :---: |
| ![Вывод звука](assets/screenshots/audio.png) | ![Полка файлов](assets/screenshots/shelf.png) |

| Свёрнутый режим и точки приватности | Подключение Bluetooth-наушников |
| :---: | :---: |
| ![Свёрнутый режим](assets/screenshots/mini.png) | ![Bluetooth-устройство](assets/screenshots/device.png) |

</div>

<details>
<summary><b>Окно настроек</b></summary>

<div align="center">
<br>
<img src="assets/screenshots/settings.png" alt="Окно настроек Dynamic Island" width="380">
</div>

</details>

---

## Возможности

- **Музыка и медиа (Windows GSMTC).** Работает со Spotify, Яндекс Музыкой, YouTube и музыкой в браузерах, «Медиаплеером» и любыми приложениями с системным медиа-контролем. Обложка масштабируется на стороне Rust (WIC `128×128`), цвет эквалайзера подстраивается под её оттенки, а между несколькими активными плеерами можно переключаться прямо из карточки трека.
- **Переключатель вывода звука в 1 клик.** Клик по чипу наушников/динамиков на домашнем экране открывает список активных аудиоустройств и переключает вывод по умолчанию без открытия системных параметров.
- **Полка файлов (Drag & Drop).** Перетащите файлы на раскрытый остров, чтобы держать их под рукой, а затем вытяните обратно в Проводник, браузер или мессенджер (через нативный Windows OLE `DoDragDrop`).
- **Таймер, свой интервал и помодоро.** Пресеты `5 мин`, `15 мин`, `Помодоро 25`, свой таймер (`1–180 мин`), `+1 мин` на лету, перерывы и 30-дневная статистика в настройках (сегодня, за 7 дней и серия дней).
- **Микрофон и камера.** Оранжевая и зелёная точки справа от острова при обращении программ к микрофону или камере; при наведении отображается конкретное приложение, а клик открывает параметры конфиденциальности Windows.
- **Буфер обмена.** Краткая подсказка при копировании текста, изображения или файлов. Контент с флагами приватности менеджеров паролей (`ExcludeClipboardContentFromMonitorProcessing`, `CanIncludeInClipboardHistory = 0`) не читается.
- **Наушники и заряд Bluetooth.** При подключении показывает название и тип устройства и, если Windows сообщает уровень батареи, кольцо заряда; текущий заряд виден и на домашнем экране.
- **Раскладка и Caps Lock.** Показывает смену языка (`РУ`, `EN` и др., включая `Win + Space`) и состояние `Caps Lock`.
- **Локализация (RU / EN) и гибкое расположение.** Автовыбор или ручное переключение языка интерфейса, выбор монитора (Основной / За курсором с учётом смешанного DPI), выравнивание (Слева / По центру / Справа), отступ сверху (`0 / 6 / 14 px`) и настраиваемые горячие клавиши.
- **Проверка обновлений и событийный бэкенд.** Проверка свежих релизов на GitHub из окна настроек (`WinHttp`), а все системные наблюдатели (`GSMTC`, `Core Audio`, `RegisterPowerSettingNotification`, `AddClipboardFormatListener`, `RegNotifyChangeKeyValue`) работают по событиям Windows.

---

## Горячие клавиши и жесты

| Действие | Управление |
| --- | --- |
| Раскрыть / свернуть остров | `Ctrl + Alt + I` *(настраивается в параметрах или наведение курсора)* |
| Запустить Помодоро 25 / пауза таймера | `Ctrl + Alt + T` *(настраивается в параметрах)* |
| Изменить громкость | Колёсико мыши над раскрытым островом *(опционально — и над свёрнутым)* |
| Следующий / предыдущий трек | Горизонтальный свайп двумя пальцами по тачпаду |
| Пауза / воспроизведение | Двойной клик по обложке или названию трека |
| Настройка своего таймера | Колёсико мыши или кнопки `−` / `+` в режиме таймера |

---

## Сборка из исходников

Требуются **Node.js 20+** и **Rust** (stable toolchain на Windows).

```bash
npm install
npm run dev      # запуск в режиме разработки
npm run build    # релизная сборка и NSIS-установщик в src-tauri/target/release/bundle/nsis
```

Проверка фронтенда и тестов Rust:

```bash
node --check ui/i18n.js
node --check ui/island.js
cargo test --manifest-path src-tauri/Cargo.toml
```

---

## Архитектура проекта

- `src-tauri/src/lib.rs` — инициализация Tauri, трей, окна, команды IPC, размещение и ротируемое логирование (`island.log`).
- `src-tauri/src/media.rs` — событийный GSMTC, переключение сессий и даунскейл обложек через Windows WIC.
- `src-tauri/src/audio.rs` — Core Audio (`IMMNotificationClient`, `IAudioEndpointVolumeCallback`), переключение устройства вывода (`IPolicyConfig`) и регулировка громкости.
- `src-tauri/src/shelf.rs` — полка файлов и нативное перетаскивание наружу через OLE `SHCreateDataObject` / `DoDragDrop`.
- `src-tauri/src/devices.rs` — чтение заряда Bluetooth-устройств через `SetupAPI`.
- `src-tauri/src/hover.rs` — прозрачность для кликов, сжатие в полоску над окнами, многомониторность и полноэкранный режим.
- `src-tauri/src/clipboard.rs` — `AddClipboardFormatListener` с проверкой приватности.
- `src-tauri/src/privacy.rs` — `RegNotifyChangeKeyValue` для микрофона и камеры.
- `src-tauri/src/power.rs` — `RegisterPowerSettingNotification` для батареи и питания.
- `src-tauri/src/keyboard.rs` — отслеживание раскладки и `Caps Lock`.
- `src-tauri/src/hotkeys.rs` — настраиваемые глобальные горячие клавиши `RegisterHotKey`.
- `src-tauri/src/settings.rs` — настройки, автозапуск в реестре и 30-дневная история помодоро.
- `src-tauri/src/updater.rs` — проверка обновлений через GitHub Releases API (`WinHttp`).
- `src-tauri/src/sysinfo.rs` — загрузка ЦП/ОЗУ и полуночный сброс счётчика.
- `src-tauri/src/winmsg.rs` — общий помощник скрытых `HWND_MESSAGE`-окон.
- `ui/` — интерфейс острова (`index.html`, `island.css`, `island.js`), локализация (`i18n.js`) и окно настроек (`settings.html`).
