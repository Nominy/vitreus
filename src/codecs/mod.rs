//! Formats the `image` crate can't read: camera RAW, JPEG XL, Photoshop and
//! HEIF/AVIF (through the operating system's decoder).

pub mod heif;
pub mod jxl;
pub mod psd;
pub mod raw;

use std::path::Path;

use anyhow::Result;
use image::{RgbaImage, metadata::Orientation};

/// Pixels in display orientation, not yet fitted to any bounds.
pub struct Pixels {
    pub rgba: RgbaImage,
    /// Size of the photo itself after orientation; `rgba` may be smaller
    /// (embedded previews, OS-side downscaling).
    pub source: (u32, u32),
    pub alpha: bool,
}

/// Decodes `path` if one of these codecs owns its extension. `bounds` is a
/// hint (device pixels the result will be fitted into) so codecs with several
/// resolutions on offer can skip the needlessly large ones.
pub fn decode(path: &Path, bounds: (u32, u32)) -> Option<Result<Pixels>> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let ext = ext.as_str();
    Some(if raw::EXTENSIONS.contains(&ext) {
        raw::decode(path, bounds)
    } else if heif::EXTENSIONS.contains(&ext) {
        heif::decode(path, bounds)
    } else if jxl::EXTENSIONS.contains(&ext) {
        jxl::decode(path)
    } else if psd::EXTENSIONS.contains(&ext) {
        psd::decode(path)
    } else {
        return None;
    })
}

/// Every extension handled here; `catalog::EXTENSIONS` must list them all.
#[cfg(test)]
pub fn extensions() -> impl Iterator<Item = &'static str> {
    [
        raw::EXTENSIONS,
        heif::EXTENSIONS,
        jxl::EXTENSIONS,
        psd::EXTENSIONS,
    ]
    .into_iter()
    .flatten()
    .copied()
}

/// `(width, height)` after applying `orientation`.
pub fn oriented((w, h): (u32, u32), orientation: Orientation) -> (u32, u32) {
    match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (h, w),
        _ => (w, h),
    }
}
