//! HEIC/HEIF/AVIF through the operating system's own decoder, so no patent-
//! encumbered codec ships with Vitreus.

use std::path::Path;

use anyhow::Result;

use super::Pixels;

pub const EXTENSIONS: &[&str] = &["heic", "heif", "hif", "avif"];

#[cfg(target_os = "macos")]
mod imageio;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod libheif;
#[cfg(windows)]
mod wic;

/// Already oriented. `bounds` lets backends that can decode smaller do so.
pub fn decode(path: &Path, bounds: (u32, u32)) -> Result<Pixels> {
    #[cfg(windows)]
    return wic::decode(path, bounds);
    #[cfg(target_os = "macos")]
    return imageio::decode(path, bounds);
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    return libheif::decode(path, bounds);
    #[cfg(not(any(
        windows,
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd"
    )))]
    {
        let _ = (path, bounds);
        anyhow::bail!("HEIC/AVIF isn't supported on this platform")
    }
}
