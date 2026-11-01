//! The facts shown in the info panel (`I`).

use std::{fs::File, io::BufReader, path::Path};

use exif::{Exif, In, Tag};

/// Reads EXIF leniently: plenty of real files have small structural errors
/// (odd IFD links, bad counts) but perfectly usable tags.
pub fn read_exif(path: &Path) -> Option<Exif> {
    let file = File::open(path).ok()?;
    exif::Reader::new()
        .continue_on_error(true)
        .read_from_container(&mut BufReader::new(file))
        .or_else(|e| e.distill_partial_result(|_| {}))
        .ok()
}

pub fn describe(path: &Path, dimensions: Option<(u32, u32)>) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    rows.push(("File", crate::catalog::file_name(path)));
    if let Some(parent) = path.parent() {
        rows.push(("Folder", parent.display().to_string()));
    }
    if let Some((w, h)) = dimensions {
        let megapixels = w as f64 * h as f64 / 1e6;
        rows.push(("Dimensions", format!("{w} × {h}  ({megapixels:.1} MP)")));
    }
    if let Ok(meta) = std::fs::metadata(path) {
        rows.push(("Size", human_size(meta.len())));
    }

    let Some(exif) = read_exif(path) else {
        return rows;
    };
    let text = |tag| {
        exif.get_field(tag, In::PRIMARY).map(|f| {
            f.display_value()
                .with_unit(&exif)
                .to_string()
                .trim_matches('"')
                .trim()
                .to_owned()
        })
    };
    if let Some(taken) = text(Tag::DateTimeOriginal).or_else(|| text(Tag::DateTime)) {
        rows.push(("Taken", taken));
    }
    let camera = [text(Tag::Make), text(Tag::Model)]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if !camera.is_empty() {
        rows.push(("Camera", dedupe_make(&camera)));
    }
    let fields = [
        ("Lens", Tag::LensModel),
        ("Exposure", Tag::ExposureTime),
        ("Aperture", Tag::FNumber),
        ("ISO", Tag::PhotographicSensitivity),
        ("Focal length", Tag::FocalLength),
        ("Flash", Tag::Flash),
    ];
    for (label, tag) in fields {
        if let Some(value) = text(tag).filter(|v| !v.is_empty()) {
            rows.push((label, value));
        }
    }
    rows
}

/// "Canon" + "Canon EOS R5" → "Canon EOS R5".
fn dedupe_make(parts: &[String]) -> String {
    match parts {
        [make, model] if model.to_lowercase().starts_with(&make.to_lowercase()) => model.clone(),
        _ => parts.join(" "),
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
