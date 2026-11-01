//! Windows: Windows Imaging Component, backed by the Store codecs
//! ("HEIF Image Extensions" + "HEVC Video Extensions", "AV1 Video Extension").
//!
//! WIC's HEIF decoder applies the `irot`/`imir` transforms itself (frames come
//! out display-oriented and report orientation 1), so no rotation happens here.

use std::path::Path;

use anyhow::{Result, anyhow};
use image::RgbaImage;
use windows::{
    Win32::{
        Foundation::GENERIC_READ,
        Graphics::Imaging::{
            CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICBitmapSource,
            IWICImagingFactory, IWICPalette, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
            WICDecodeMetadataCacheOnDemand,
        },
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
    },
    core::{HRESULT, HSTRING, Interface},
};

use crate::codecs::Pixels;

/// Same cap as the other decoders' allocation limit.
const MAX_BYTES: u64 = 4 << 30;
/// Thumbnails whose aspect differs more than this from the photo are letterboxed.
const ASPECT_TOLERANCE: f32 = 0.03;

/// What WIC reports when the Store codec (or the video codec under it) is absent.
const CODEC_MISSING: [u32; 4] = [
    0x8898_2F50, // WINCODEC_ERR_COMPONENTNOTFOUND
    0x8898_2F8B, // WINCODEC_ERR_COMPONENTINITIALIZEFAILURE
    0x8004_0154, // REGDB_E_CLASSNOTREG
    0xC00D_5212, // MF_E_TOPO_CODEC_NOT_FOUND
];

/// COM, entered once per decode thread and left when the thread ends.
struct Apartment(bool);

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

thread_local! {
    // S_OK / S_FALSE need balancing; RPC_E_CHANGED_MODE (already STA) is still usable.
    static APARTMENT: Apartment = Apartment(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok());
}

pub fn decode(path: &Path, bounds: (u32, u32)) -> Result<Pixels> {
    APARTMENT.with(|_| {});
    let (rgba, source) = unsafe { decode_frame(path, bounds) }.map_err(|e| explain(path, e))?;
    let alpha = rgba.pixels().any(|p| p[3] != 255);
    Ok(Pixels {
        rgba,
        source,
        alpha,
    })
}

/// Returns the pixels plus the size of the full frame.
unsafe fn decode_frame(
    path: &Path,
    bounds: (u32, u32),
) -> windows::core::Result<(RgbaImage, (u32, u32))> {
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let decoder = factory.CreateDecoderFromFilename(
            &HSTRING::from(path.as_os_str()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )?;
        let frame = decoder.GetFrame(0)?;
        let source = size(&frame)?;

        // HEIC files carry a ~320 px thumbnail; using it skips the HEVC decode
        // (over a second for a phone photo) whenever it is big enough.
        let needed = crate::decode::fit_within(source, bounds);
        let aspect = |(w, h): (u32, u32)| w as f32 / h.max(1) as f32;
        let thumbnail = frame.GetThumbnail().ok().filter(|thumb| {
            size(thumb).is_ok_and(|(w, h)| {
                w >= needed.0
                    && h >= needed.1
                    && (aspect((w, h)) / aspect(source) - 1.0).abs() <= ASPECT_TOLERANCE
            })
        });
        let chosen: IWICBitmapSource = match thumbnail {
            Some(thumb) => thumb,
            None => frame.cast()?,
        };

        let converter = factory.CreateFormatConverter()?;
        converter.Initialize(
            &chosen,
            &GUID_WICPixelFormat32bppRGBA,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let (width, height) = size(&converter)?;
        let bytes = width as u64 * height as u64 * 4;
        if bytes == 0 || bytes > MAX_BYTES {
            return Err(windows::core::Error::new(
                HRESULT(0x8007_000E_u32 as i32), // E_OUTOFMEMORY
                format!("image too large ({width} × {height})"),
            ));
        }
        let mut pixels = vec![0u8; bytes as usize];
        converter.CopyPixels(std::ptr::null(), width * 4, &mut pixels)?;
        let rgba =
            RgbaImage::from_raw(width, height, pixels).expect("buffer sized from dimensions");
        Ok((rgba, source))
    }
}

fn size(source: &IWICBitmapSource) -> windows::core::Result<(u32, u32)> {
    let (mut width, mut height) = (0, 0);
    unsafe { source.GetSize(&mut width, &mut height)? };
    Ok((width, height))
}

fn explain(path: &Path, error: windows::core::Error) -> anyhow::Error {
    if !CODEC_MISSING.contains(&(error.code().0 as u32)) {
        return anyhow!("Windows couldn't decode this image: {}", error.message());
    }
    let avif = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("avif"));
    if avif {
        anyhow!(
            "Windows needs the free “AV1 Video Extension” from the Microsoft Store to open AVIF images"
        )
    } else {
        anyhow!(
            "Windows needs the free “HEIF Image Extensions” and “HEVC Video Extensions” from the Microsoft Store to open HEIC images"
        )
    }
}
