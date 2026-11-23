//! Общие утилиты WIC (Windows Imaging Component) и RAII-обёртка для COM.
//!
//! Используются модулями `media` (даунскейл обложек 128×128) и `clipboard`
//! (декодирование скриншотов в PNG, миниатюры и конвертация в `CF_DIB`).

use base64::Engine;
use windows::Win32::Foundation::{HGLOBAL, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA,
    IWICBitmapSource, IWICImagingFactory, WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant,
    WICConvertBitmapSource, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, GetHGlobalFromStream};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

/// RAII-гард для `CoInitializeEx` / `CoUninitialize`.
/// Вызывает `CoUninitialize` только если инициализация в текущем потоке прошла успешно.
pub struct ComGuard(bool);

impl ComGuard {
    pub fn init_mta() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // S_OK (0) и S_FALSE (1) требуют парного CoUninitialize; RPC_E_CHANGED_MODE — нет.
        Self(hr.is_ok() && hr != RPC_E_CHANGED_MODE)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

unsafe fn encode_source_png(
    factory: &IWICImagingFactory,
    src: &IWICBitmapSource,
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
    let out_stream = CreateStreamOnHGlobal(HGLOBAL::default(), true).ok()?;
    let encoder = factory
        .CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())
        .ok()?;
    encoder.Initialize(&out_stream, WICBitmapEncoderNoCache).ok()?;
    let mut out_frame = None;
    let mut props = None;
    encoder.CreateNewFrame(&mut out_frame, &mut props).ok()?;
    let out_frame = out_frame?;
    out_frame.Initialize(props.as_ref()).ok()?;
    out_frame.SetSize(w, h).ok()?;
    out_frame.WriteSource(src, std::ptr::null()).ok()?;
    out_frame.Commit().ok()?;
    encoder.Commit().ok()?;

    let hg = GetHGlobalFromStream(&out_stream).ok()?;
    let size = GlobalSize(hg);
    if size == 0 {
        return None;
    }
    let ptr = GlobalLock(hg) as *const u8;
    if ptr.is_null() {
        return None;
    }
    let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
    let _ = GlobalUnlock(hg);
    Some(bytes)
}

/// Уменьшает изображение до `max_px` по большей стороне и кодирует в PNG.
/// Возвращает `None`, если картинка уже укладывается в `max_px` и `max_bytes`.
pub fn downscale_png(raw: &[u8], max_px: u32, max_bytes: usize) -> Option<Vec<u8>> {
    let _com = ComGuard::init_mta();
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let in_stream = factory.CreateStream().ok()?;
        in_stream.InitializeFromMemory(raw).ok()?;
        let decoder = factory
            .CreateDecoderFromStream(&in_stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        if w <= max_px && h <= max_px && raw.len() <= max_bytes {
            return None;
        }
        let max_dim = w.max(h);
        if max_dim == 0 {
            return None;
        }
        let (nw, nh) = if max_dim <= max_px {
            (w, h)
        } else {
            let scale = max_px as f64 / max_dim as f64;
            (
                ((w as f64 * scale).round() as u32).max(1),
                ((h as f64 * scale).round() as u32).max(1),
            )
        };
        let scaler = factory.CreateBitmapScaler().ok()?;
        scaler
            .Initialize(&frame, nw, nh, WICBitmapInterpolationModeFant)
            .ok()?;
        let src: IWICBitmapSource = windows::core::Interface::cast(&scaler).ok()?;
        encode_source_png(&factory, &src, nw, nh)
    }
}

/// Декодирует изображение (PNG или BMP) через WIC и возвращает `(полный_PNG, миниатюра_data_url, w, h)`.
pub fn decode_to_png_and_thumb(
    raw: &[u8],
    already_png: bool,
    thumb_px: u32,
) -> Option<(Vec<u8>, String, u32, u32)> {
    let _com = ComGuard::init_mta();
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let in_stream = factory.CreateStream().ok()?;
        in_stream.InitializeFromMemory(raw).ok()?;
        let decoder = factory
            .CreateDecoderFromStream(&in_stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 || w > 8192 || h > 8192 {
            return None;
        }
        let full_png = if already_png {
            raw.to_vec()
        } else {
            let src: IWICBitmapSource = windows::core::Interface::cast(&frame).ok()?;
            encode_source_png(&factory, &src, w, h)?
        };
        if full_png.len() > 8 * 1024 * 1024 {
            return None;
        }

        let max_dim = w.max(h);
        let thumb_png = if max_dim <= thumb_px {
            full_png.clone()
        } else {
            let scale = thumb_px as f64 / max_dim as f64;
            let tw = ((w as f64 * scale).round() as u32).max(1);
            let th = ((h as f64 * scale).round() as u32).max(1);
            let scaler = factory.CreateBitmapScaler().ok()?;
            scaler
                .Initialize(&frame, tw, th, WICBitmapInterpolationModeFant)
                .ok()?;
            let src: IWICBitmapSource = windows::core::Interface::cast(&scaler).ok()?;
            encode_source_png(&factory, &src, tw, th)?
        };
        let preview = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&thumb_png)
        );
        Some((full_png, preview, w, h))
    }
}

/// Конвертирует PNG в 32-битный `CF_DIB` (`BITMAPINFOHEADER` + BGRA top-down) для Win32-приложений.
pub fn png_to_dib(png: &[u8]) -> Option<Vec<u8>> {
    let _com = ComGuard::init_mta();
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let in_stream = factory.CreateStream().ok()?;
        in_stream.InitializeFromMemory(png).ok()?;
        let decoder = factory
            .CreateDecoderFromStream(&in_stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        let bgra = WICConvertBitmapSource(&GUID_WICPixelFormat32bppBGRA, &frame).ok()?;
        let stride = w.checked_mul(4)?;
        let img_bytes = stride.checked_mul(h)? as usize;
        let mut top_down = vec![0u8; img_bytes];
        bgra.CopyPixels(std::ptr::null(), stride, &mut top_down).ok()?;

        let mut dib = Vec::with_capacity(40 + img_bytes);
        dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
        dib.extend_from_slice(&(w as i32).to_le_bytes()); // biWidth
        dib.extend_from_slice(&(-(h as i32)).to_le_bytes()); // top-down biHeight
        dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
        dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        dib.extend_from_slice(&(img_bytes as u32).to_le_bytes());
        dib.extend_from_slice(&0i32.to_le_bytes());
        dib.extend_from_slice(&0i32.to_le_bytes());
        dib.extend_from_slice(&0u32.to_le_bytes());
        dib.extend_from_slice(&0u32.to_le_bytes());
        dib.extend_from_slice(&top_down);
        Some(dib)
    }
}
