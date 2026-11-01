//! Turns files into GPU-ready BGRA frames at the size the viewer needs.

use std::{
    fs::File,
    io::BufReader,
    path::Path,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU8, Ordering},
    },
};

use anyhow::{Result, anyhow};
use fast_image_resize as fr;
use gpui::RenderImage;
use image::{
    AnimationDecoder, Delay, DynamicImage, Frame, ImageBuffer, ImageDecoder, ImageFormat,
    ImageReader, Limits, RgbaImage,
    codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder},
    imageops,
    metadata::Orientation,
};
use resvg::{tiny_skia, usvg};

use crate::{codecs, settings::Scaling};

pub struct Decoded {
    pub image: Arc<RenderImage>,
    /// Size of the photo itself after EXIF orientation. The pixels in `image`
    /// may be fewer (previews, thumbnails) and may be user-rotated.
    pub width: u32,
    pub height: u32,
    /// Nothing was thrown away: zooming in can't get any sharper.
    pub full_res: bool,
    /// Quarter turns clockwise already applied to `image`.
    pub rotation: u8,
    /// No transparent pixels, so a drop shadow behind it is invisible.
    pub opaque: bool,
}

impl Decoded {
    /// Image pixels per photo pixel (1.0 when `full_res`).
    pub fn pixel_ratio(&self) -> f32 {
        let size = self.image.size(0);
        let longest = size.width.0.max(size.height.0) as f32;
        longest / self.width.max(self.height).max(1) as f32
    }
}

const THUMB_SIZE: u32 = 192;
/// Largest texture every gpui backend accepts.
const MAX_TEXTURE: u32 = 16384;
/// Vectors are rasterized once, sharp enough for any sensible zoom.
const MAX_SVG: u32 = 4096;
const MAX_ANIMATION_BYTES: usize = 768 << 20;

pub fn thumbnail(path: &Path) -> Result<Decoded> {
    if let Some(thumb) = exif_thumbnail(path) {
        return Ok(thumb);
    }
    if has_extension(path, codecs::psd::EXTENSIONS)
        && let Some(thumb) = codecs::psd::thumbnail(path)
    {
        return Ok(finish(
            thumb.rgba,
            thumb.source,
            (THUMB_SIZE, THUMB_SIZE),
            false,
        ));
    }
    decode(path, (THUMB_SIZE, THUMB_SIZE), false)
}

/// Fits within `bounds` device pixels (never upscaled).
pub fn preview(path: &Path, bounds: (u32, u32)) -> Result<Decoded> {
    decode(path, bounds, true)
}

pub fn full(path: &Path) -> Result<Decoded> {
    decode(path, (MAX_TEXTURE, MAX_TEXTURE), true)
}

fn decode(path: &Path, bounds: (u32, u32), animate: bool) -> Result<Decoded> {
    if has_extension(path, &["svg"]) {
        return svg(path, bounds);
    }
    if let Some(pixels) = codecs::decode(path, bounds) {
        let pixels = pixels?;
        return Ok(finish(pixels.rgba, pixels.source, bounds, pixels.alpha));
    }
    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    if animate && let Some(animation) = animation(path, reader.format(), bounds)? {
        return Ok(animation);
    }
    reader.limits(limits());
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let alpha = image.color().has_alpha();
    let (width, height) = (image.width(), image.height());
    Ok(finish(image.into_rgba8(), (width, height), bounds, alpha))
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    // Big panoramas are the point of a photo viewer; the default 512 MiB cap is too low.
    limits.max_alloc = Some(4 << 30);
    limits
}

/// Downscales into `bounds`, converts to BGRA and wraps for gpui.
fn finish(rgba: RgbaImage, source: (u32, u32), bounds: (u32, u32), alpha: bool) -> Decoded {
    let (width, height) = rgba.dimensions();
    let target = fit_within((width, height), bounds);
    let full_res = (width, height) == source && target == source;
    let rgba = if target == (width, height) {
        rgba
    } else {
        resize(rgba, target, alpha)
    };
    Decoded {
        image: Arc::new(RenderImage::new(vec![Frame::new(to_bgra(rgba))])),
        width: source.0,
        height: source.1,
        full_res,
        rotation: 0,
        opaque: !alpha,
    }
}

pub(crate) fn fit_within((w, h): (u32, u32), (max_w, max_h): (u32, u32)) -> (u32, u32) {
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    let scale = (max_w as f64 / w as f64).min(max_h as f64 / h as f64);
    (
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
    )
}

/// Resampling used for downscaled decodes; follows the Scaling setting.
static SCALING: AtomicU8 = AtomicU8::new(0);

pub fn set_scaling(scaling: Scaling) {
    SCALING.store(scaling as u8, Ordering::Relaxed);
}

fn resize(rgba: RgbaImage, (w, h): (u32, u32), alpha: bool) -> RgbaImage {
    let (sw, sh) = rgba.dimensions();
    let src = fr::images::Image::from_vec_u8(sw, sh, rgba.into_raw(), fr::PixelType::U8x4)
        .expect("RGBA buffer matches its dimensions");
    let mut dst = fr::images::Image::new(w, h, fr::PixelType::U8x4);
    let algorithm = match SCALING.load(Ordering::Relaxed) {
        x if x == Scaling::Sharp as u8 => fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3),
        x if x == Scaling::Pixel as u8 => fr::ResizeAlg::Nearest,
        _ => fr::ResizeAlg::Convolution(fr::FilterType::CatmullRom),
    };
    let options = fr::ResizeOptions::new().resize_alg(algorithm).use_alpha(alpha);
    fr::Resizer::new()
        .resize(&src, &mut dst, &options)
        .expect("same pixel type on both sides");
    RgbaImage::from_raw(w, h, dst.into_vec()).expect("resizer output matches its dimensions")
}

/// gpui textures are BGRA.
fn to_bgra(mut rgba: RgbaImage) -> RgbaImage {
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    rgba
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

/// Animated GIF / WebP / APNG. `None` for still images.
fn animation(
    path: &Path,
    format: Option<ImageFormat>,
    bounds: (u32, u32),
) -> Result<Option<Decoded>> {
    let open = || -> Result<BufReader<File>> { Ok(BufReader::new(File::open(path)?)) };
    let frames = match format {
        Some(ImageFormat::Gif) => GifDecoder::new(open()?)?.into_frames(),
        Some(ImageFormat::WebP) => {
            let decoder = WebPDecoder::new(open()?)?;
            if !decoder.has_animation() {
                return Ok(None);
            }
            decoder.into_frames()
        }
        Some(ImageFormat::Png) => {
            let decoder = PngDecoder::new(open()?)?;
            if !decoder.is_apng()? {
                return Ok(None);
            }
            decoder.apng()?.into_frames()
        }
        _ => return Ok(None),
    };

    let mut out = Vec::new();
    let mut source = (0, 0);
    let mut bytes = 0usize;
    for frame in frames {
        let frame = frame?;
        let delay = frame.delay();
        let rgba = frame.into_buffer();
        source = rgba.dimensions();
        let target = fit_within(source, bounds);
        let rgba = if target == source {
            rgba
        } else {
            resize(rgba, target, true)
        };
        bytes += rgba.len();
        out.push(Frame::from_parts(to_bgra(rgba), 0, 0, sane_delay(delay)));
        if bytes > MAX_ANIMATION_BYTES {
            break;
        }
    }
    if out.len() < 2 {
        // Single-frame GIF/WebP: let the still path handle it (orientation, limits).
        return Ok(None);
    }
    let full_res = out[0].buffer().dimensions() == source;
    Ok(Some(Decoded {
        image: Arc::new(RenderImage::new(out)),
        width: source.0,
        height: source.1,
        full_res,
        rotation: 0,
        opaque: false,
    }))
}

/// Browsers treat tiny GIF delays as 100 ms; so do we.
fn sane_delay(delay: Delay) -> Delay {
    let (num, den) = delay.numer_denom_ms();
    if den == 0 || num / den.max(1) < 20 {
        Delay::from_numer_denom_ms(100, 1)
    } else {
        delay
    }
}

/// Cameras embed a ~160 px JPEG in the EXIF block; reading it is ~100x faster
/// than decoding the photo, which is what makes fly-through instant.
fn exif_thumbnail(path: &Path) -> Option<Decoded> {
    if !has_extension(path, &["jpg", "jpeg", "jpe", "jfif"]) {
        return None;
    }
    let exif = crate::info::read_exif(path)?;
    let uint = |tag, ifd| exif.get_field(tag, ifd)?.value.get_uint(0);
    let offset = uint(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL)? as usize;
    let len = uint(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL)? as usize;
    let bytes = exif.buf().get(offset..offset.checked_add(len)?)?;
    let mut thumb = image::load_from_memory_with_format(bytes, ImageFormat::Jpeg).ok()?;

    let orientation = uint(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|v| Orientation::from_exif(v as u8))
        .unwrap_or(Orientation::NoTransforms);
    thumb.apply_orientation(orientation);

    let dimensions = ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    let source = codecs::oriented(dimensions, orientation);
    // Many cameras letterbox the embedded thumbnail to 4:3; a stretched stand-in looks broken.
    let thumb_aspect = thumb.width() as f32 / thumb.height() as f32;
    let source_aspect = source.0 as f32 / source.1 as f32;
    if (thumb_aspect / source_aspect - 1.0).abs() > 0.03 {
        return None;
    }
    Some(finish(
        thumb.into_rgba8(),
        source,
        (THUMB_SIZE, THUMB_SIZE),
        false,
    ))
}

fn svg(path: &Path, bounds: (u32, u32)) -> Result<Decoded> {
    static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    });
    let fontdb = FONTS.clone();
    let options = usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        fontdb,
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(&std::fs::read(path)?, &options)?;
    let size = tree.size();
    // Rasterize into a square box so the result stays sharp when the user rotates it.
    let side = bounds.0.max(bounds.1).min(MAX_SVG) as f32;
    let scale = (side / size.width()).min(side / size.height());
    let (pw, ph) = (
        ((size.width() * scale).round() as u32).max(1),
        ((size.height() * scale).round() as u32).max(1),
    );
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph).ok_or_else(|| anyhow!("SVG has no area"))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia is premultiplied RGBA; gpui wants straight BGRA.
    let mut data = pixmap.take();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
        px.swap(0, 2);
    }
    let bgra = RgbaImage::from_raw(pw, ph, data).expect("pixmap matches its dimensions");
    Ok(Decoded {
        image: Arc::new(RenderImage::new(vec![Frame::new(bgra)])),
        // Vectors have no native size worth honouring: report the raster size
        // so they fit the screen like a photo instead of showing at icon size.
        width: pw,
        height: ph,
        full_res: true,
        rotation: 0,
        opaque: false,
    })
}

/// Rotates every frame by `quarter_turns` clockwise.
pub fn rotate(image: &RenderImage, quarter_turns: u8) -> Arc<RenderImage> {
    let frames: Vec<Frame> = (0..image.frame_count())
        .map(|i| {
            let size = image.size(i);
            let bytes = image.as_bytes(i).unwrap_or_default().to_vec();
            let buffer: RgbaImage =
                ImageBuffer::from_raw(size.width.0 as u32, size.height.0 as u32, bytes)
                    .expect("frame bytes match frame size");
            let rotated = match quarter_turns % 4 {
                1 => imageops::rotate90(&buffer),
                2 => imageops::rotate180(&buffer),
                3 => imageops::rotate270(&buffer),
                _ => buffer,
            };
            Frame::from_parts(rotated, 0, 0, image.delay(i))
        })
        .collect();
    Arc::new(RenderImage::new(frames))
}
