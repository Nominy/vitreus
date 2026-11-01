//! Photoshop PSD/PSB: the flattened composite Photoshop stores next to the
//! layers ("Maximize Compatibility"). Layers themselves are not composited.

use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use anyhow::{Result, anyhow, bail};
use image::{ImageFormat, RgbaImage};

use super::Pixels;

pub const EXTENSIONS: &[&str] = &["psd", "psb"];

/// Same cap as the other decoders' allocation limit.
const MAX_BYTES: u64 = 4 << 30;
const THUMBNAIL_RESOURCE: u16 = 1036;
const VERSION_RESOURCE: u16 = 1057;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Bitmap,
    Gray,
    Indexed,
    Rgb,
    Cmyk,
    Lab,
}

struct Header {
    big: bool,
    channels: usize,
    width: u32,
    height: u32,
    depth: u16,
    mode: Mode,
}

struct Document {
    header: Header,
    palette: Vec<u8>,
    resources: Vec<u8>,
    reader: BufReader<File>,
}

fn open(path: &Path) -> Result<Document> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut h = [0u8; 26];
    reader.read_exact(&mut h)?;
    if &h[..4] != b"8BPS" {
        bail!("not a Photoshop file");
    }
    let be16 = |b: &[u8]| u16::from_be_bytes([b[0], b[1]]);
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let big = match be16(&h[4..6]) {
        1 => false,
        2 => true,
        v => bail!("unknown Photoshop version {v}"),
    };
    let mode = match be16(&h[24..26]) {
        0 => Mode::Bitmap,
        // Duotone stores its gray ramp; showing that is what other viewers do.
        1 | 8 => Mode::Gray,
        2 => Mode::Indexed,
        3 => Mode::Rgb,
        4 => Mode::Cmyk,
        9 => Mode::Lab,
        _ => bail!("multichannel Photoshop documents aren't supported"),
    };
    let header = Header {
        big,
        channels: be16(&h[12..14]) as usize,
        height: be32(&h[14..18]),
        width: be32(&h[18..22]),
        depth: be16(&h[22..24]),
        mode,
    };
    if !matches!(header.depth, 1 | 8 | 16 | 32) || header.width == 0 || header.height == 0 {
        bail!("damaged Photoshop header");
    }
    if header.width as u64 * header.height as u64 * 4 > MAX_BYTES {
        bail!("image too large ({} × {})", header.width, header.height);
    }
    let palette = section(&mut reader, 4)?;
    let resources = section(&mut reader, 4)?;
    Ok(Document {
        header,
        palette,
        resources,
        reader,
    })
}

/// Reads a length-prefixed section.
fn section(reader: &mut BufReader<File>, len_bytes: usize) -> Result<Vec<u8>> {
    let len = read_len(reader, len_bytes)?;
    if len > MAX_BYTES {
        bail!("damaged Photoshop section");
    }
    let mut data = vec![0; len as usize];
    reader.read_exact(&mut data)?;
    Ok(data)
}

fn read_len(reader: &mut impl Read, len_bytes: usize) -> Result<u64> {
    let mut b = [0u8; 8];
    reader.read_exact(&mut b[8 - len_bytes..])?;
    Ok(u64::from_be_bytes(b))
}

/// Image resource blocks: ("8BIM", id, padded Pascal name, padded data).
fn resource(resources: &[u8], wanted: u16) -> Option<&[u8]> {
    let mut pos = 0;
    while pos + 12 <= resources.len() {
        let block = &resources[pos..];
        if &block[..4] != b"8BIM" {
            return None;
        }
        let id = u16::from_be_bytes([block[4], block[5]]);
        let name_len = (1 + block[6] as usize + 1) & !1;
        let at = 6 + name_len;
        let size = u32::from_be_bytes(block.get(at..at + 4)?.try_into().ok()?) as usize;
        let data = block.get(at + 4..at + 4 + size)?;
        if id == wanted {
            return Some(data);
        }
        pos += at + 4 + ((size + 1) & !1);
    }
    None
}

/// Photoshop's ~160 px JPEG thumbnail; reading it skips the whole composite.
pub fn thumbnail(path: &Path) -> Option<Pixels> {
    let doc = open(path).ok()?;
    let data = resource(&doc.resources, THUMBNAIL_RESOURCE)?;
    // 28-byte header; format 1 is JFIF.
    if data.len() <= 28 || u32::from_be_bytes(data[..4].try_into().ok()?) != 1 {
        return None;
    }
    let thumb = image::load_from_memory_with_format(&data[28..], ImageFormat::Jpeg).ok()?;
    let source = (doc.header.width, doc.header.height);
    let aspect = |(w, h): (u32, u32)| w as f32 / h.max(1) as f32;
    if (aspect((thumb.width(), thumb.height())) / aspect(source) - 1.0).abs() > 0.03 {
        return None;
    }
    Some(Pixels {
        rgba: thumb.into_rgba8(),
        source,
        alpha: false,
    })
}

pub fn decode(path: &Path) -> Result<Pixels> {
    let Document {
        header: h,
        palette,
        resources,
        mut reader,
    } = open(path)?;
    // Version info: hasRealMergedData is false when saved without Maximize Compatibility.
    if let Some(version) = resource(&resources, VERSION_RESOURCE)
        && version.get(4) == Some(&0)
    {
        bail!("saved without “Maximize Compatibility”, so there is no flattened image to show");
    }

    // Layer and mask information; a negative layer count means the first
    // extra channel is the composite's transparency.
    let len_bytes = if h.big { 8 } else { 4 };
    let layers_len = read_len(&mut reader, len_bytes)?;
    let layers_start = reader.stream_position()?;
    let mut transparent = false;
    if layers_len >= len_bytes as u64 + 2 && read_len(&mut reader, len_bytes)? > 0 {
        let mut count = [0u8; 2];
        reader.read_exact(&mut count)?;
        transparent = i16::from_be_bytes(count) < 0;
    }
    reader.seek(SeekFrom::Start(layers_start + layers_len))?;

    let color = match h.mode {
        Mode::Rgb | Mode::Lab => 3,
        Mode::Cmyk => 4,
        Mode::Bitmap | Mode::Gray | Mode::Indexed => 1,
    };
    let alpha = transparent && h.channels > color && h.mode != Mode::Indexed;
    let used = color + alpha as usize;
    if h.channels < color {
        bail!("damaged Photoshop file: {} channels", h.channels);
    }

    let mut compression = [0u8; 2];
    reader.read_exact(&mut compression)?;
    let (w, ht) = (h.width as usize, h.height as usize);
    let row_bytes = (w * h.depth as usize).div_ceil(8);
    let mut planes = vec![Vec::new(); used];
    match u16::from_be_bytes(compression) {
        0 => {
            for plane in &mut planes {
                *plane = vec![0; row_bytes * ht];
                reader.read_exact(plane)?;
            }
        }
        1 => {
            // PackBits rows, preceded by every row's byte count.
            let count_bytes = if h.big { 4 } else { 2 };
            let mut counts = vec![0u8; h.channels * ht * count_bytes];
            reader.read_exact(&mut counts)?;
            let count = |i: usize| {
                let c = &counts[i * count_bytes..][..count_bytes];
                c.iter().fold(0usize, |acc, &b| acc << 8 | b as usize)
            };
            let mut packed = Vec::new();
            for (c, plane) in planes.iter_mut().enumerate() {
                *plane = vec![0; row_bytes * ht];
                for (y, row) in plane.chunks_exact_mut(row_bytes).enumerate() {
                    packed.resize(count(c * ht + y), 0);
                    reader.read_exact(&mut packed)?;
                    unpack_bits(&packed, row);
                }
            }
        }
        n => bail!("unsupported Photoshop compression {n}"),
    }

    let planes: Vec<Vec<u8>> = planes
        .into_iter()
        .enumerate()
        .map(|(i, plane)| {
            to_8bit(
                plane,
                h.depth,
                w,
                i < color && matches!(h.mode, Mode::Rgb | Mode::Gray),
            )
        })
        .collect();
    let mut rgba = vec![255u8; w * ht * 4];
    for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
        let v = |c: usize| planes[c][i];
        let [r, g, b] = match h.mode {
            Mode::Rgb => [v(0), v(1), v(2)],
            Mode::Gray => [v(0); 3],
            // 1 = black, already expanded to bytes.
            Mode::Bitmap => [255 - v(0); 3],
            Mode::Indexed => {
                let i = v(0) as usize;
                let p = |c: usize| palette.get(c * 256 + i).copied().unwrap_or(0);
                [p(0), p(1), p(2)]
            }
            // Stored inverted: 255 is no ink.
            Mode::Cmyk => {
                let k = v(3) as u32;
                [0, 1, 2].map(|c| (v(c) as u32 * k / 255) as u8)
            }
            Mode::Lab => lab_to_srgb(v(0), v(1), v(2)),
        };
        px[..3].copy_from_slice(&[r, g, b]);
        if alpha {
            let a = v(color);
            px[3] = a;
            // Photoshop mattes the composite against white; undo that.
            if a != 0 && a != 255 {
                for c in &mut px[..3] {
                    let matted = *c as i32 - (255 - a as i32);
                    *c = (matted.max(0) * 255 / a as i32).min(255) as u8;
                }
            }
        }
    }
    let rgba =
        RgbaImage::from_raw(h.width, h.height, rgba).ok_or_else(|| anyhow!("buffer size"))?;
    Ok(Pixels {
        rgba,
        source: (h.width, h.height),
        alpha,
    })
}

fn unpack_bits(mut src: &[u8], dst: &mut [u8]) {
    let mut out = 0;
    while let [header, rest @ ..] = src {
        let n = *header as i8;
        if n >= 0 {
            let len = (n as usize + 1).min(rest.len());
            let take = len.min(dst.len() - out);
            dst[out..out + take].copy_from_slice(&rest[..take]);
            out += take;
            src = &rest[len..];
        } else if n != -128 {
            let Some((&value, rest)) = rest.split_first() else {
                break;
            };
            let take = (1 - n as isize) as usize;
            let take = take.min(dst.len() - out);
            dst[out..out + take].fill(value);
            out += take;
            src = rest;
        } else {
            src = rest;
        }
    }
}

/// One channel to 8 bits per pixel. 32-bit documents are linear light.
fn to_8bit(plane: Vec<u8>, depth: u16, width: usize, gamma: bool) -> Vec<u8> {
    match depth {
        8 => plane,
        16 => plane.chunks_exact(2).map(|s| s[0]).collect(),
        32 => plane
            .chunks_exact(4)
            .map(|s| {
                let v = f32::from_be_bytes([s[0], s[1], s[2], s[3]]).clamp(0.0, 1.0);
                let v = if gamma { linear_to_srgb(v) } else { v };
                (v * 255.0 + 0.5) as u8
            })
            .collect(),
        // Bitmap: one bit per pixel, rows padded to whole bytes.
        _ => {
            let row_bytes = width.div_ceil(8);
            plane
                .chunks_exact(row_bytes)
                .flat_map(|row| {
                    (0..width).map(move |x| {
                        if row[x / 8] >> (7 - x % 8) & 1 == 1 {
                            255
                        } else {
                            0
                        }
                    })
                })
                .collect()
        }
    }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// CIELAB (D50, Photoshop's encoding) to sRGB.
fn lab_to_srgb(l: u8, a: u8, b: u8) -> [u8; 3] {
    let l = l as f32 * 100.0 / 255.0;
    let (a, b) = (a as f32 - 128.0, b as f32 - 128.0);
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let f = |t: f32| {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            3.0 * (6.0f32 / 29.0).powi(2) * (t - 4.0 / 29.0)
        }
    };
    let (x, y, z) = (0.9642 * f(fx), f(fy), 0.8249 * f(fz));
    // XYZ (D50) to linear sRGB, Bradford-adapted.
    let r = 3.1339 * x - 1.6170 * y - 0.4906 * z;
    let g = -0.9785 * x + 1.9160 * y + 0.0334 * z;
    let bl = 0.0720 * x - 0.2290 * y + 1.4057 * z;
    [r, g, bl].map(|c| (linear_to_srgb(c.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8)
}

#[cfg(test)]
mod tests {
    use super::unpack_bits;

    #[test]
    fn packbits_matches_apple_example_and_tolerates_overruns() {
        // Apple TN1023's sample stream.
        let packed = [
            0xFE, 0xAA, 0x02, 0x80, 0x00, 0x2A, 0xFD, 0xAA, 0x03, 0x80, 0x00, 0x2A, 0x22, 0xF7,
            0xAA,
        ];
        let mut row = [0u8; 24];
        unpack_bits(&packed, &mut row);
        assert_eq!(
            row,
            [
                0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0xAA, 0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0x22,
                0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA
            ]
        );
        // A corrupt run longer than the row must not panic.
        let mut short = [0u8; 2];
        unpack_bits(&[0x81, 7, 0x05, 1, 2, 3], &mut short);
        assert_eq!(short, [7, 7]);
    }
}
