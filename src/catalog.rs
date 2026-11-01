//! Folder listing: which files are photos and in what order they are shown.

use std::{
    cmp::Ordering,
    ffi::OsStr,
    path::{Path, PathBuf},
};

/// Extensions the decoders understand (lowercase, no dot). Also the list of
/// file types Vitreus registers itself for.
pub const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "gif", "webp", "bmp", "dib", "tif", "tiff", "ico",
    "tga", "qoi", "pnm", "pbm", "pgm", "ppm", "pam", "hdr", "exr", "dds", "ff", "svg",
    // Through `codecs`.
    "heic", "heif", "hif", "avif", "jxl", "psd", "psb",
    // Camera RAW (embedded previews).
    "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "dng", "raf", "orf", "rw2", "pef", "srw",
    "raw", "rwl", "3fr", "iiq", "x3f",
];

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

/// All supported images directly inside `dir`, in natural file-name order.
pub fn scan(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| !t.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| is_supported(path))
        .collect();
    files.sort_by(|a, b| natural_cmp(&file_name(a), &file_name(b)));
    files
}

/// Resolves what the user asked to open into a folder listing plus the
/// index to start at. A directory opens at its first image.
pub fn listing_for(path: &Path) -> (Vec<PathBuf>, usize) {
    if path.is_dir() {
        return (scan(path), 0);
    }
    let mut files = path.parent().map(scan).unwrap_or_default();
    let index = match files.iter().position(|p| p.file_name() == path.file_name()) {
        Some(index) => index,
        None => {
            // Opened explicitly with an extension we don't list; still show it.
            files.push(path.to_path_buf());
            files.sort_by(|a, b| natural_cmp(&file_name(a), &file_name(b)));
            files.iter().position(|p| p == path).unwrap_or(0)
        }
    };
    (files, index)
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Explorer/Finder-style ordering: case-insensitive, digit runs compared by value
/// ("img2" < "img10").
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let da = take_digits(&mut a);
                let db = take_digits(&mut b);
                let (ta, tb) = (da.trim_start_matches('0'), db.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

fn take_digits(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut digits = String::new();
    while let Some(c) = chars.peek().copied().filter(char::is_ascii_digit) {
        digits.push(c);
        chars.next();
    }
    digits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_matches_file_managers() {
        let mut names = vec![
            "IMG_10.jpg",
            "img_2.jpg",
            "IMG_1.jpg",
            "b.png",
            "A.png",
            "img_003.jpg",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            [
                "A.png",
                "b.png",
                "IMG_1.jpg",
                "img_2.jpg",
                "img_003.jpg",
                "IMG_10.jpg"
            ]
        );
    }

    #[test]
    fn every_codec_extension_is_listed_and_registered() {
        for ext in crate::codecs::extensions() {
            assert!(
                EXTENSIONS.contains(&ext),
                "{ext} missing from catalog::EXTENSIONS"
            );
        }
    }
}
