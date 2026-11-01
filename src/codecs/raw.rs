//! Camera RAW files, shown through the JPEG previews cameras embed in them.
//!
//! Demosaicing is slow and never matches the camera's own rendering; every
//! RAW container carries one or more finished JPEGs (often full size) that do.
//! This module finds them all, reads only their headers, then decodes the
//! smallest one that is still big enough for the request.

use std::{
    collections::HashSet,
    fs::File,
    io::{Cursor, Read, Seek, SeekFrom},
    path::Path,
};

use anyhow::{Context, Result, anyhow, bail};
use image::{
    DynamicImage, ImageDecoder, RgbImage, codecs::jpeg::JpegDecoder, metadata::Orientation,
};

use super::{Pixels, oriented};

pub const EXTENSIONS: &[&str] = &[
    "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "dng", "raf", "orf", "rw2", "pef", "srw",
    "raw", "rwl", "3fr", "iiq", "x3f",
];

/// Above this the bytes can't be a preview; guards against corrupt offsets.
const MAX_PREVIEW_BYTES: u64 = 256 << 20;
/// Previews whose aspect differs more than this from the largest are letterboxed.
const ASPECT_TOLERANCE: f32 = 0.03;

pub fn decode(path: &Path, bounds: (u32, u32)) -> Result<Pixels> {
    let mut file = Source::open(path)?;
    let found = scan(&mut file)?;
    let largest = found
        .previews
        .iter()
        .max_by_key(|p| p.width as u64 * p.height as u64)
        .ok_or_else(|| anyhow!("this RAW file has no embedded preview to show"))?;

    // The container's orientation; previews without one (Fujifilm) carry it in their own EXIF.
    let orientation = found.orientation.unwrap_or(Orientation::NoTransforms);
    let full = oriented((largest.width, largest.height), orientation);
    let needed = crate::decode::fit_within(full, bounds);
    let aspect = |p: &Preview| p.width as f32 / p.height.max(1) as f32;
    let chosen = found
        .previews
        .iter()
        .filter(|p| (aspect(p) / aspect(largest) - 1.0).abs() <= ASPECT_TOLERANCE)
        .filter(|p| {
            let (w, h) = oriented((p.width, p.height), orientation);
            w >= needed.0 && h >= needed.1
        })
        .min_by_key(|p| p.width as u64 * p.height as u64)
        .unwrap_or(largest);

    let (mut image, embedded) = file.load(chosen)?;
    let orientation = found
        .orientation
        .or(embedded)
        .unwrap_or(Orientation::NoTransforms);
    image.apply_orientation(orientation);
    Ok(Pixels {
        rgba: image.into_rgba8(),
        source: oriented((largest.width, largest.height), orientation),
        alpha: false,
    })
}

#[derive(Debug)]
struct Preview {
    width: u32,
    height: u32,
    data: Data,
}

#[derive(Debug)]
enum Data {
    Jpeg {
        offset: u64,
        len: u64,
    },
    /// Uncompressed 8-bit RGB, possibly split into strips.
    Rgb {
        strips: Vec<(u64, u64)>,
    },
}

#[derive(Default)]
struct Found {
    previews: Vec<Preview>,
    orientation: Option<Orientation>,
}

/// Positioned reads on the file; previews are a small part of a large file.
struct Source {
    file: File,
    len: u64,
}

impl Source {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }

    fn read(&mut self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let end = offset.checked_add(len).filter(|&end| end <= self.len);
        if end.is_none() || len > MAX_PREVIEW_BYTES {
            bail!("RAW structure points outside the file");
        }
        let mut buf = vec![0; len as usize];
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn array<const N: usize>(&mut self, offset: u64) -> Result<[u8; N]> {
        let mut buf = [0; N];
        if offset
            .checked_add(N as u64)
            .is_none_or(|end| end > self.len)
        {
            bail!("RAW structure points outside the file");
        }
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Adds a JPEG candidate if `offset` holds a baseline/progressive JPEG
    /// (rejecting the lossless JPEG some cameras store raw data in).
    fn add_jpeg(&mut self, found: &mut Found, offset: u64, len: u64) {
        if len < 128 || len > MAX_PREVIEW_BYTES {
            return;
        }
        if let Some((width, height)) = self.jpeg_size(offset, len)
            && !found
                .previews
                .iter()
                .any(|p| matches!(p.data, Data::Jpeg { offset: o, .. } if o == offset))
        {
            found.previews.push(Preview {
                width,
                height,
                data: Data::Jpeg { offset, len },
            });
        }
    }

    /// Walks JPEG markers up to the frame header.
    fn jpeg_size(&mut self, start: u64, len: u64) -> Option<(u32, u32)> {
        let end = start.checked_add(len)?.min(self.len);
        if self.array::<2>(start).ok()? != [0xFF, 0xD8] {
            return None;
        }
        let mut pos = start + 2;
        for _ in 0..256 {
            if pos + 4 > end {
                return None;
            }
            let [ff, marker, hi, lo] = self.array::<4>(pos).ok()?;
            if ff != 0xFF {
                return None;
            }
            match marker {
                // Fill byte before a marker.
                0xFF => pos += 1,
                // Baseline, extended sequential, progressive.
                0xC0..=0xC2 => {
                    let [_, _, _, _, _, h1, h0, w1, w0] = self.array::<9>(pos).ok()?;
                    let (w, h) = (u16::from_be_bytes([w1, w0]), u16::from_be_bytes([h1, h0]));
                    return (w > 0 && h > 0).then_some((w.into(), h.into()));
                }
                // Lossless, arithmetic, hierarchical: not something `image` decodes.
                0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF | 0xD8 | 0xD9 | 0xDA => return None,
                _ => pos += 2 + u16::from_be_bytes([hi, lo]) as u64,
            }
        }
        None
    }

    /// Decodes a preview; also returns the orientation in its own EXIF, if any.
    fn load(&mut self, preview: &Preview) -> Result<(DynamicImage, Option<Orientation>)> {
        match &preview.data {
            Data::Jpeg { offset, len } => {
                let bytes = self.read(*offset, (*len).min(self.len.saturating_sub(*offset)))?;
                let mut decoder = JpegDecoder::new(Cursor::new(bytes))?;
                let orientation = decoder
                    .orientation()
                    .ok()
                    .filter(|o| *o != Orientation::NoTransforms);
                let image = DynamicImage::from_decoder(decoder).context("embedded preview")?;
                Ok((image, orientation))
            }
            Data::Rgb { strips } => {
                let needed = preview.width as usize * preview.height as usize * 3;
                let mut pixels = Vec::with_capacity(needed);
                for &(offset, len) in strips {
                    pixels.extend(self.read(offset, len)?);
                }
                pixels.truncate(needed);
                let image = RgbImage::from_raw(preview.width, preview.height, pixels)
                    .ok_or_else(|| anyhow!("embedded preview is truncated"))?;
                Ok((DynamicImage::ImageRgb8(image), None))
            }
        }
    }
}

fn scan(src: &mut Source) -> Result<Found> {
    let head = src
        .array::<16>(0)
        .map_err(|_| anyhow!("file too short to be a RAW photo"))?;
    let mut found = Found::default();
    if &head[..15] == b"FUJIFILMCCD-RAW" {
        // The header points at a full-quality JPEG (with its own EXIF orientation).
        let [o3, o2, o1, o0, l3, l2, l1, l0] = src.array::<8>(84)?;
        let offset = u32::from_be_bytes([o3, o2, o1, o0]) as u64;
        let len = u32::from_be_bytes([l3, l2, l1, l0]) as u64;
        src.add_jpeg(&mut found, offset, len);
    } else if &head[4..8] == b"ftyp" && &head[8..12] == b"crx " {
        cr3(src, &mut found)?;
    } else if &head[..4] == b"FOVb" {
        x3f(src, &mut found)?;
    } else if let Some(tiff) = Tiff::at(src, 0) {
        let mut walker = Walker {
            src,
            found: &mut found,
            seen: HashSet::new(),
        };
        walker.chain(tiff, tiff.first_ifd, 0);
    } else {
        bail!("not a RAW format Vitreus recognizes");
    }
    Ok(found)
}

// ---------------------------------------------------------------------------
// TIFF-based containers: CR2, NEF, NRW, ARW, SRF, SR2, DNG, ORF, RW2, PEF, SRW,
// RAW, RWL, 3FR, IIQ.

const IMAGE_WIDTH: u16 = 0x100;
const IMAGE_LENGTH: u16 = 0x101;
const BITS_PER_SAMPLE: u16 = 0x102;
const COMPRESSION: u16 = 0x103;
const PHOTOMETRIC: u16 = 0x106;
const STRIP_OFFSETS: u16 = 0x111;
const ORIENTATION: u16 = 0x112;
const SAMPLES_PER_PIXEL: u16 = 0x115;
const STRIP_BYTE_COUNTS: u16 = 0x117;
const PLANAR_CONFIG: u16 = 0x11C;
const SUB_IFDS: u16 = 0x14A;
const JPEG_OFFSET: u16 = 0x201;
const JPEG_LENGTH: u16 = 0x202;
const EXIF_IFD: u16 = 0x8769;
const MAKER_NOTE: u16 = 0x927C;
/// Panasonic RW2/RWL/RAW: the camera's full JPEG, stored inline.
const PANASONIC_JPEG: u16 = 0x2E;

/// One TIFF structure: where its offsets count from and its byte order.
#[derive(Clone, Copy)]
struct Tiff {
    base: u64,
    big_endian: bool,
    first_ifd: u32,
}

impl Tiff {
    /// Parses a TIFF header (including the ORF/RW2 magic variants) at `base`.
    fn at(src: &mut Source, base: u64) -> Option<Tiff> {
        let header = src.array::<8>(base).ok()?;
        let big_endian = match &header[..2] {
            b"II" => false,
            b"MM" => true,
            _ => return None,
        };
        let mut tiff = Tiff {
            base,
            big_endian,
            first_ifd: 0,
        };
        // 42 = TIFF, ORF "RO"/"RS", Panasonic 0x55.
        if !matches!(tiff.u16(&header[2..4]), 42 | 0x4F52 | 0x5352 | 0x55) {
            return None;
        }
        tiff.first_ifd = tiff.u32(&header[4..8]);
        Some(tiff)
    }

    fn u16(&self, b: &[u8]) -> u16 {
        let b = [b[0], b[1]];
        if self.big_endian {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        }
    }

    fn u32(&self, b: &[u8]) -> u32 {
        let b = [b[0], b[1], b[2], b[3]];
        if self.big_endian {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        }
    }
}

struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    value: [u8; 4],
}

struct Ifd {
    tiff: Tiff,
    entries: Vec<Entry>,
    next: u32,
}

impl Ifd {
    fn read(src: &mut Source, tiff: Tiff, offset: u32) -> Option<Ifd> {
        let at = tiff.base + offset as u64;
        let count = tiff.u16(&src.array::<2>(at).ok()?) as u64;
        if count == 0 || count > 1000 {
            return None;
        }
        let bytes = src.read(at + 2, count * 12 + 4).ok()?;
        let entries = bytes
            .chunks_exact(12)
            .map(|e| Entry {
                tag: tiff.u16(&e[0..2]),
                kind: tiff.u16(&e[2..4]),
                count: tiff.u32(&e[4..8]),
                value: [e[8], e[9], e[10], e[11]],
            })
            .collect();
        let next = tiff.u32(&bytes[count as usize * 12..]);
        Some(Ifd {
            tiff,
            entries,
            next,
        })
    }

    fn entry(&self, tag: u16) -> Option<&Entry> {
        self.entries.iter().find(|e| e.tag == tag)
    }

    /// Integer values of a BYTE/SHORT/LONG/IFD entry.
    fn values(&self, src: &mut Source, tag: u16) -> Option<Vec<u32>> {
        let e = self.entry(tag)?;
        let size = match e.kind {
            1 | 7 => 1,
            3 => 2,
            4 | 13 => 4,
            _ => return None,
        };
        let count = e.count.min(4096) as u64;
        let total = count * size;
        let bytes = if total <= 4 {
            e.value[..total as usize].to_vec()
        } else {
            src.read(self.tiff.base + self.tiff.u32(&e.value) as u64, total)
                .ok()?
        };
        Some(
            bytes
                .chunks_exact(size as usize)
                .map(|b| match size {
                    1 => b[0] as u32,
                    2 => self.tiff.u16(b) as u32,
                    _ => self.tiff.u32(b),
                })
                .collect(),
        )
    }

    fn value(&self, src: &mut Source, tag: u16) -> Option<u32> {
        self.values(src, tag)?.first().copied()
    }

    /// Absolute file offset and length of an entry's out-of-line data.
    fn blob(&self, tag: u16) -> Option<(u64, u64)> {
        let e = self.entry(tag)?;
        (e.count > 4).then(|| {
            (
                self.tiff.base + self.tiff.u32(&e.value) as u64,
                e.count as u64,
            )
        })
    }

    /// Offset of a sub-IFD, whether stored as a pointer or as an inline blob.
    fn pointer(&self, src: &mut Source, tag: u16) -> Option<u32> {
        let e = self.entry(tag)?;
        if e.kind == 7 {
            (e.count > 4).then(|| self.tiff.u32(&e.value))
        } else {
            self.value(src, tag)
        }
    }
}

struct Walker<'a> {
    src: &'a mut Source,
    found: &'a mut Found,
    seen: HashSet<u64>,
}

impl Walker<'_> {
    /// Visits an IFD chain plus everything hanging off it.
    fn chain(&mut self, tiff: Tiff, mut offset: u32, depth: u32) {
        for index in 0..16 {
            if offset == 0 || depth > 4 || !self.seen.insert(tiff.base + offset as u64) {
                return;
            }
            let Some(ifd) = Ifd::read(self.src, tiff, offset) else {
                return;
            };
            if depth == 0 && index == 0 && self.found.orientation.is_none() {
                self.found.orientation = ifd
                    .value(self.src, ORIENTATION)
                    .and_then(|v| Orientation::from_exif(v as u8));
            }
            self.previews(&ifd);
            for sub in ifd.values(self.src, SUB_IFDS).unwrap_or_default() {
                self.chain(tiff, sub, depth + 1);
            }
            if let Some(exif) = ifd.pointer(self.src, EXIF_IFD)
                && self.seen.insert(tiff.base + exif as u64)
                && let Some(exif) = Ifd::read(self.src, tiff, exif)
                && let Some((at, len)) = exif.blob(MAKER_NOTE)
            {
                self.maker_note(tiff, at, len);
            }
            offset = ifd.next;
        }
    }

    fn previews(&mut self, ifd: &Ifd) {
        let src = &mut *self.src;
        let base = ifd.tiff.base;
        if let (Some(offset), Some(len)) =
            (ifd.value(src, JPEG_OFFSET), ifd.value(src, JPEG_LENGTH))
        {
            src.add_jpeg(self.found, base + offset as u64, len as u64);
        }
        if let Some((offset, len)) = ifd.blob(PANASONIC_JPEG) {
            src.add_jpeg(self.found, offset, len);
        }

        let photometric = ifd.value(src, PHOTOMETRIC);
        // Colour filter array / linear raw: sensor data, not a picture.
        if matches!(photometric, Some(32803 | 34892)) {
            return;
        }
        let (Some(offsets), Some(counts)) = (
            ifd.values(src, STRIP_OFFSETS),
            ifd.values(src, STRIP_BYTE_COUNTS),
        ) else {
            return;
        };
        if offsets.len() != counts.len() || offsets.is_empty() {
            return;
        }
        match ifd.value(src, COMPRESSION) {
            // Old-style and new-style JPEG in one strip (CR2 IFD0, DNG previews).
            Some(6 | 7) if offsets.len() == 1 => {
                src.add_jpeg(self.found, base + offsets[0] as u64, counts[0] as u64);
            }
            Some(1) | None if photometric == Some(2) => {
                let (Some(width), Some(height)) =
                    (ifd.value(src, IMAGE_WIDTH), ifd.value(src, IMAGE_LENGTH))
                else {
                    return;
                };
                let bits = ifd.values(src, BITS_PER_SAMPLE).unwrap_or_default();
                let samples = ifd.value(src, SAMPLES_PER_PIXEL).unwrap_or(1);
                let planar = ifd.value(src, PLANAR_CONFIG).unwrap_or(1);
                let total: u64 = counts.iter().map(|&c| c as u64).sum();
                if samples == 3
                    && planar == 1
                    && bits.iter().all(|&b| b == 8)
                    && width > 0
                    && height > 0
                    && total >= width as u64 * height as u64 * 3
                    && total <= MAX_PREVIEW_BYTES
                {
                    let strips = offsets
                        .iter()
                        .zip(&counts)
                        .map(|(&o, &c)| (base + o as u64, c as u64))
                        .collect();
                    self.found.previews.push(Preview {
                        width,
                        height,
                        data: Data::Rgb { strips },
                    });
                }
            }
            _ => {}
        }
    }

    /// Makernotes that hold previews the main IFDs don't point at.
    fn maker_note(&mut self, tiff: Tiff, at: u64, len: u64) {
        let Ok(head) = self.src.array::<16>(at) else {
            return;
        };
        let order = |b: &[u8]| match b {
            b"MM" => Some(true),
            b"II" => Some(false),
            _ => None,
        };
        let own = |big_endian| Tiff {
            base: at,
            big_endian,
            first_ifd: 0,
        };
        // Where the note's offsets count from, and where its IFD sits relative to that.
        let (note, ifd_at, tags): (Tiff, u64, &[PreviewTags]) =
            if head.starts_with(b"OLYMPUS\0") || head.starts_with(b"OM SYSTEM\0") {
                let header = if head.starts_with(b"OLYMPUS\0") {
                    8
                } else {
                    12
                };
                let Some(big) = order(&head[header..header + 2]) else {
                    return;
                };
                (
                    own(big),
                    header as u64 + 4,
                    &[PreviewTags::Sub(0x2020, 0x101, 0x102)],
                )
            } else if head.starts_with(b"OLYMP\0") || head.starts_with(b"EPSON\0") {
                let tags = &[
                    PreviewTags::Sub(0x2020, 0x101, 0x102),
                    PreviewTags::Direct(0x88, 0x89),
                ];
                (tiff, at + 8 - tiff.base, tags)
            } else if head.starts_with(b"Nikon\0") {
                // A complete TIFF header follows the 10-byte signature.
                let Some(inner) = Tiff::at(self.src, at + 10) else {
                    return;
                };
                (
                    inner,
                    inner.first_ifd as u64,
                    &[PreviewTags::Sub(0x11, JPEG_OFFSET, JPEG_LENGTH)],
                )
            } else if head.starts_with(b"AOC\0") {
                let big = order(&head[4..6]).unwrap_or(tiff.big_endian);
                (
                    Tiff {
                        big_endian: big,
                        ..tiff
                    },
                    at + 6 - tiff.base,
                    &[PreviewTags::Direct(0x05, 0x04)],
                )
            } else if head.starts_with(b"PENTAX \0") {
                let Some(big) = order(&head[8..10]) else {
                    return;
                };
                (own(big), 10, &[PreviewTags::Direct(0x05, 0x04)])
            } else {
                return;
            };
        let ifd_abs = note.base + ifd_at;
        if ifd_abs < at || ifd_abs >= at + len || !self.seen.insert(ifd_abs) {
            return;
        }
        let Some(ifd) = Ifd::read(self.src, note, ifd_at as u32) else {
            return;
        };
        for &tags in tags {
            let inner;
            let (holder, start, length) = match tags {
                PreviewTags::Direct(start, length) => (&ifd, start, length),
                PreviewTags::Sub(pointer, start, length) => {
                    let Some(sub) = ifd
                        .pointer(self.src, pointer)
                        .and_then(|o| Ifd::read(self.src, note, o))
                    else {
                        continue;
                    };
                    inner = sub;
                    (&inner, start, length)
                }
            };
            if let (Some(offset), Some(len)) = (
                holder.value(self.src, start),
                holder.value(self.src, length),
            ) {
                self.src
                    .add_jpeg(self.found, note.base + offset as u64, len as u64);
            }
        }
    }
}

/// Where a makernote keeps its preview: (start tag, length tag) in the note's
/// own IFD, or behind a pointer to a sub-IFD.
#[derive(Clone, Copy)]
enum PreviewTags {
    Direct(u16, u16),
    Sub(u16, u16, u16),
}

// ---------------------------------------------------------------------------
// Canon CR3 (ISO base media file format).

const CANON_UUID: [u8; 16] = [
    0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48,
];
const PREVIEW_UUID: [u8; 16] = [
    0xea, 0xf4, 0x2b, 0x5e, 0x1c, 0x98, 0x4b, 0x88, 0xb9, 0xfb, 0xb7, 0xdc, 0x40, 0x6e, 0x4d, 0x16,
];

/// A box's type and payload range (after the size/type/uuid header).
struct Bmff {
    kind: [u8; 4],
    uuid: Option<[u8; 16]>,
    start: u64,
    end: u64,
}

fn boxes(src: &mut Source, mut pos: u64, end: u64) -> Vec<Bmff> {
    let mut out = Vec::new();
    while pos + 8 <= end && out.len() < 256 {
        let Ok(head) = src.array::<8>(pos) else { break };
        let kind = [head[4], head[5], head[6], head[7]];
        let (size, mut header) = match u32::from_be_bytes([head[0], head[1], head[2], head[3]]) {
            0 => (end - pos, 8),
            1 => match src.array::<8>(pos + 8) {
                Ok(large) => (u64::from_be_bytes(large), 16),
                Err(_) => break,
            },
            size => (size as u64, 8),
        };
        if size < header || pos + size > end {
            break;
        }
        let mut uuid = None;
        if &kind == b"uuid" {
            uuid = src.array::<16>(pos + header).ok();
            header += 16;
        }
        out.push(Bmff {
            kind,
            uuid,
            start: pos + header,
            end: pos + size,
        });
        pos += size;
    }
    out
}

fn child(src: &mut Source, parent: &Bmff, kind: &[u8; 4]) -> Option<Bmff> {
    boxes(src, parent.start, parent.end)
        .into_iter()
        .find(|b| &b.kind == kind)
}

fn cr3(src: &mut Source, found: &mut Found) -> Result<()> {
    let len = src.len;
    for top in boxes(src, 0, len) {
        match (&top.kind, top.uuid) {
            (b"moov", _) => {
                for b in boxes(src, top.start, top.end) {
                    if b.uuid == Some(CANON_UUID) {
                        for meta in boxes(src, b.start, b.end) {
                            match &meta.kind {
                                // IFD0 of the photo's EXIF: orientation lives here.
                                b"CMT1" => {
                                    if let Some(tiff) = Tiff::at(src, meta.start) {
                                        let mut walker = Walker {
                                            src,
                                            found,
                                            seen: HashSet::new(),
                                        };
                                        walker.chain(tiff, tiff.first_ifd, 0);
                                    }
                                }
                                b"THMB" => jpeg_in_box(src, found, &meta),
                                _ => {}
                            }
                        }
                    } else if &b.kind == b"trak" {
                        track_jpeg(src, found, &b);
                    }
                }
            }
            (b"uuid", Some(PREVIEW_UUID)) => {
                // 8 bytes of unknown data precede the PRVW box.
                for b in boxes(src, top.start + 8, top.end) {
                    if &b.kind == b"PRVW" {
                        jpeg_in_box(src, found, &b);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// THMB / PRVW: a short header, then the JPEG up to the end of the box.
fn jpeg_in_box(src: &mut Source, found: &mut Found, b: &Bmff) {
    let Ok(head) = src.read(b.start, 64.min(b.end - b.start)) else {
        return;
    };
    if let Some(i) = head.windows(3).position(|w| w == [0xFF, 0xD8, 0xFF]) {
        let offset = b.start + i as u64;
        src.add_jpeg(found, offset, b.end - offset);
    }
}

/// The first CR3 track is the full-size JPEG; the others are raw data.
fn track_jpeg(src: &mut Source, found: &mut Found, trak: &Bmff) {
    let Some(stbl) = child(src, trak, b"mdia")
        .and_then(|b| child(src, &b, b"minf"))
        .and_then(|b| child(src, &b, b"stbl"))
    else {
        return;
    };
    let offset = if let Some(co64) = child(src, &stbl, b"co64") {
        src.array::<8>(co64.start + 8).ok().map(u64::from_be_bytes)
    } else if let Some(stco) = child(src, &stbl, b"stco") {
        src.array::<4>(stco.start + 8)
            .ok()
            .map(|b| u32::from_be_bytes(b) as u64)
    } else {
        None
    };
    let len = child(src, &stbl, b"stsz").and_then(|stsz| {
        let fixed = u32::from_be_bytes(src.array::<4>(stsz.start + 4).ok()?);
        let first = if fixed != 0 {
            fixed
        } else {
            u32::from_be_bytes(src.array::<4>(stsz.start + 12).ok()?)
        };
        Some(first as u64)
    });
    if let (Some(offset), Some(len)) = (offset, len) {
        src.add_jpeg(found, offset, len);
    }
}

// ---------------------------------------------------------------------------
// Sigma X3F.

fn x3f(src: &mut Source, found: &mut Found) -> Result<()> {
    let directory = u32::from_le_bytes(src.array::<4>(src.len - 4)?) as u64;
    let head = src.array::<12>(directory)?;
    if &head[..4] != b"SECd" {
        bail!("damaged X3F directory");
    }
    let count = u32::from_le_bytes([head[8], head[9], head[10], head[11]]).min(256) as u64;
    let entries = src.read(directory + 12, count * 12)?;
    for e in entries.chunks_exact(12) {
        let le = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64;
        let (offset, len) = (le(&e[0..4]), le(&e[4..8]));
        if !matches!(&e[8..12], b"IMA2" | b"IMAG") || len < 28 {
            continue;
        }
        let section = src.array::<28>(offset)?;
        // Data format 18: JPEG.
        if &section[..4] == b"SECi" && le(&section[12..16]) == 18 {
            src.add_jpeg(found, offset + 28, len - 28);
        }
    }
    Ok(())
}
