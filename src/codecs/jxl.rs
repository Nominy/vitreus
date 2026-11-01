//! JPEG XL via jxl-oxide. Animations show their first frame.

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use image::RgbaImage;
use jxl_oxide::{JxlImage, PixelFormat};

use super::Pixels;

pub const EXTENSIONS: &[&str] = &["jxl"];

pub fn decode(path: &Path) -> Result<Pixels> {
    let image = JxlImage::builder().open(path).map_err(|e| anyhow!(e))?;
    let format = image.pixel_format();
    if format.has_black() {
        bail!("CMYK JPEG XL images aren't supported");
    }
    let render = image.render_frame(0).map_err(|e| anyhow!(e))?;
    // Orientation is applied by the stream.
    let mut stream = render.stream();
    let (width, height) = (stream.width(), stream.height());
    let channels = stream.channels() as usize;
    let mut samples = vec![0u8; width as usize * height as usize * channels];
    stream.write_to_buffer(&mut samples);

    let rgba = match channels {
        4 => samples,
        3 => samples
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        2 => samples
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        1 => samples.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        n => bail!("unexpected JPEG XL channel count {n}"),
    };
    let rgba = RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| anyhow!("JPEG XL decoder returned a short buffer"))?;
    Ok(Pixels {
        rgba,
        source: (width, height),
        alpha: matches!(format, PixelFormat::Graya | PixelFormat::Rgba),
    })
}
