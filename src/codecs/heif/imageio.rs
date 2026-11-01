//! macOS: ImageIO, which decodes HEIC/HEIF/AVIF natively (AVIF since macOS 13)
//! and applies the orientation itself.

use std::{ffi::c_void, path::Path, ptr};

use anyhow::{Result, anyhow};
use core_foundation::{
    base::{CFRelease, CFType, CFTypeRef, TCFType},
    boolean::CFBoolean,
    dictionary::{CFDictionary, CFDictionaryRef},
    number::CFNumber,
    string::{CFString, CFStringRef},
    url::{CFURL, CFURLRef},
};
use core_graphics::{
    base::{kCGBitmapByteOrder32Big, kCGImageAlphaPremultipliedLast},
    color_space::{CGColorSpace, kCGColorSpaceSRGB},
    context::CGContext,
    geometry::{CGPoint, CGRect, CGSize},
    image::CGImage,
    sys,
};
use foreign_types::ForeignType;
use image::{RgbaImage, metadata::Orientation};

use crate::codecs::{Pixels, oriented};

type CGImageSourceRef = *const c_void;

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    fn CGImageSourceCreateWithURL(url: CFURLRef, options: CFDictionaryRef) -> CGImageSourceRef;
    fn CGImageSourceCopyPropertiesAtIndex(
        source: CGImageSourceRef,
        index: usize,
        options: CFDictionaryRef,
    ) -> CFDictionaryRef;
    fn CGImageSourceCreateThumbnailAtIndex(
        source: CGImageSourceRef,
        index: usize,
        options: CFDictionaryRef,
    ) -> sys::CGImageRef;

    static kCGImageSourceCreateThumbnailFromImageAlways: CFStringRef;
    static kCGImageSourceCreateThumbnailWithTransform: CFStringRef;
    static kCGImageSourceThumbnailMaxPixelSize: CFStringRef;
    static kCGImageSourceShouldCacheImmediately: CFStringRef;
    static kCGImagePropertyPixelWidth: CFStringRef;
    static kCGImagePropertyPixelHeight: CFStringRef;
    static kCGImagePropertyOrientation: CFStringRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGImageGetAlphaInfo(image: sys::CGImageRef) -> u32;
}

/// Same cap as the other decoders' allocation limit.
const MAX_BYTES: u64 = 4 << 30;

/// Owns a CGImageSource.
struct Source(CGImageSourceRef);

impl Drop for Source {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0 as CFTypeRef) };
    }
}

pub fn decode(path: &Path, bounds: (u32, u32)) -> Result<Pixels> {
    let url = CFURL::from_path(path, false).ok_or_else(|| anyhow!("invalid path"))?;
    let source = unsafe { CGImageSourceCreateWithURL(url.as_concrete_TypeRef(), ptr::null()) };
    if source.is_null() {
        return Err(anyhow!("macOS can't read this file"));
    }
    let source = Source(source);

    let key = |k: CFStringRef| unsafe { CFString::wrap_under_get_rule(k) };
    let props = unsafe { CGImageSourceCopyPropertiesAtIndex(source.0, 0, ptr::null()) };
    let (size, orientation) = if props.is_null() {
        ((0, 0), Orientation::NoTransforms)
    } else {
        let props: CFDictionary<CFString, CFType> =
            unsafe { CFDictionary::wrap_under_create_rule(props) };
        let number = |k: CFStringRef| {
            props
                .find(key(k))
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|n| n.to_i64())
        };
        let width = unsafe { number(kCGImagePropertyPixelWidth) }.unwrap_or(0) as u32;
        let height = unsafe { number(kCGImagePropertyPixelHeight) }.unwrap_or(0) as u32;
        let orientation = unsafe { number(kCGImagePropertyOrientation) }
            .and_then(|o| Orientation::from_exif(o as u8))
            .unwrap_or(Orientation::NoTransforms);
        ((width, height), orientation)
    };
    let full = oriented(size, orientation);
    let wanted = if full.0 > 0 && full.1 > 0 {
        crate::decode::fit_within(full, bounds)
    } else {
        bounds
    };

    // A "thumbnail" at the wanted size: ImageIO decodes no more than needed
    // and applies the orientation (HEIF irot/imir included).
    let options = CFDictionary::from_CFType_pairs(&[
        (
            key(unsafe { kCGImageSourceCreateThumbnailFromImageAlways }),
            CFBoolean::true_value().as_CFType(),
        ),
        (
            key(unsafe { kCGImageSourceCreateThumbnailWithTransform }),
            CFBoolean::true_value().as_CFType(),
        ),
        (
            key(unsafe { kCGImageSourceShouldCacheImmediately }),
            CFBoolean::true_value().as_CFType(),
        ),
        (
            key(unsafe { kCGImageSourceThumbnailMaxPixelSize }),
            CFNumber::from(wanted.0.max(wanted.1) as i64).as_CFType(),
        ),
    ]);
    let image =
        unsafe { CGImageSourceCreateThumbnailAtIndex(source.0, 0, options.as_concrete_TypeRef()) };
    if image.is_null() {
        return Err(anyhow!("macOS couldn't decode this image"));
    }
    let image = unsafe { CGImage::from_ptr(image) };
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 || (width * height * 4) as u64 > MAX_BYTES {
        return Err(anyhow!("image too large ({width} × {height})"));
    }
    // kCGImageAlphaNone, NoneSkipLast, NoneSkipFirst.
    let alpha = !matches!(
        unsafe { CGImageGetAlphaInfo(image.as_ptr()) } & 0x1F,
        0 | 5 | 6
    );

    let mut pixels = vec![0u8; width * height * 4];
    let space = CGColorSpace::create_with_name(unsafe { kCGColorSpaceSRGB })
        .unwrap_or_else(CGColorSpace::create_device_rgb);
    let context = CGContext::create_bitmap_context(
        Some(pixels.as_mut_ptr().cast()),
        width,
        height,
        8,
        width * 4,
        &space,
        kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big,
    );
    let rect = CGRect::new(
        &CGPoint::new(0.0, 0.0),
        &CGSize::new(width as f64, height as f64),
    );
    context.draw_image(rect, &image);
    drop(context);
    if alpha {
        // Bitmap contexts only do premultiplied alpha.
        for p in pixels.chunks_exact_mut(4) {
            let a = p[3] as u32;
            if a != 0 && a != 255 {
                for c in &mut p[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
    }
    let rgba = RgbaImage::from_raw(width as u32, height as u32, pixels)
        .expect("buffer sized from dimensions");
    let source = if full.0 > 0 && full.1 > 0 {
        full
    } else {
        rgba.dimensions()
    };
    Ok(Pixels {
        rgba,
        source,
        alpha,
    })
}
