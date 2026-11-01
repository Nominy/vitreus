//! Linux/BSD: libheif, loaded at runtime so Vitreus still starts without it.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    os::unix::ffi::OsStrExt,
    path::Path,
    ptr,
    sync::LazyLock,
};

use anyhow::{Result, anyhow, bail};
use image::RgbaImage;
use libloading::Library;

use crate::codecs::Pixels;

const MISSING: &str = "HEIC/AVIF needs libheif (e.g. sudo apt install libheif1)";
const COLORSPACE_RGB: c_int = 1;
const CHROMA_INTERLEAVED_RGBA: c_int = 11;
const CHANNEL_INTERLEAVED: c_int = 10;
/// Same cap as the other decoders' allocation limit.
const MAX_BYTES: u64 = 4 << 30;

#[repr(C)]
struct HeifError {
    code: c_int,
    subcode: c_int,
    message: *const c_char,
}

type Context = *mut c_void;
type Handle = *mut c_void;
type Image = *mut c_void;

struct Api {
    context_alloc: unsafe extern "C" fn() -> Context,
    context_free: unsafe extern "C" fn(Context),
    read_from_file: unsafe extern "C" fn(Context, *const c_char, *const c_void) -> HeifError,
    primary_image_handle: unsafe extern "C" fn(Context, *mut Handle) -> HeifError,
    handle_release: unsafe extern "C" fn(Handle),
    has_alpha: unsafe extern "C" fn(Handle) -> c_int,
    decode_image:
        unsafe extern "C" fn(Handle, *mut Image, c_int, c_int, *const c_void) -> HeifError,
    plane: unsafe extern "C" fn(Image, c_int, *mut c_int) -> *const u8,
    width: unsafe extern "C" fn(Image, c_int) -> c_int,
    height: unsafe extern "C" fn(Image, c_int) -> c_int,
    image_release: unsafe extern "C" fn(Image),
    _library: Library,
}

fn api() -> Result<&'static Api> {
    static API: LazyLock<Result<Api, String>> = LazyLock::new(|| unsafe { load() });
    API.as_ref().map_err(|e| anyhow!("{e}"))
}

unsafe fn load() -> Result<Api, String> {
    let library = ["libheif.so.1", "libheif.so"]
        .into_iter()
        .find_map(|name| unsafe { Library::new(name) }.ok())
        .ok_or_else(|| MISSING.to_owned())?;
    macro_rules! symbol {
        ($name:literal) => {
            *unsafe { library.get(concat!($name, "\0").as_bytes()) }
                .map_err(|e| format!("libheif is too old ({e}); {MISSING}"))?
        };
    }
    // libheif ≥ 1.13 loads its codec plugins here; older versions don't need it.
    if let Ok(init) =
        unsafe { library.get::<unsafe extern "C" fn(*mut c_void) -> HeifError>(b"heif_init\0") }
    {
        unsafe { init(ptr::null_mut()) };
    }
    Ok(Api {
        context_alloc: symbol!("heif_context_alloc"),
        context_free: symbol!("heif_context_free"),
        read_from_file: symbol!("heif_context_read_from_file"),
        primary_image_handle: symbol!("heif_context_get_primary_image_handle"),
        handle_release: symbol!("heif_image_handle_release"),
        has_alpha: symbol!("heif_image_handle_has_alpha_channel"),
        decode_image: symbol!("heif_decode_image"),
        plane: symbol!("heif_image_get_plane_readonly"),
        width: symbol!("heif_image_get_width"),
        height: symbol!("heif_image_get_height"),
        image_release: symbol!("heif_image_release"),
        _library: library,
    })
}

fn check(error: HeifError) -> Result<()> {
    if error.code == 0 {
        return Ok(());
    }
    let message = if error.message.is_null() {
        "unknown error".into()
    } else {
        unsafe { CStr::from_ptr(error.message) }.to_string_lossy()
    };
    bail!("libheif: {message}")
}

/// Runs a release function when dropped.
struct Release<T: Copy>(T, unsafe extern "C" fn(T));

impl<T: Copy> Drop for Release<T> {
    fn drop(&mut self) {
        unsafe { (self.1)(self.0) };
    }
}

/// Decodes the primary image with libheif's default transformations
/// (rotation, mirroring, cropping) applied.
pub fn decode(path: &Path, _bounds: (u32, u32)) -> Result<Pixels> {
    let api = api()?;
    let path = CString::new(path.as_os_str().as_bytes())?;
    unsafe {
        let context = (api.context_alloc)();
        if context.is_null() {
            bail!("libheif: out of memory");
        }
        let context = Release(context, api.context_free);
        check((api.read_from_file)(context.0, path.as_ptr(), ptr::null()))?;
        let mut handle = ptr::null_mut();
        check((api.primary_image_handle)(context.0, &mut handle))?;
        let handle = Release(handle, api.handle_release);
        let alpha = (api.has_alpha)(handle.0) != 0;
        let mut image = ptr::null_mut();
        check((api.decode_image)(
            handle.0,
            &mut image,
            COLORSPACE_RGB,
            CHROMA_INTERLEAVED_RGBA,
            ptr::null(),
        ))?;
        let image = Release(image, api.image_release);

        let width = (api.width)(image.0, CHANNEL_INTERLEAVED);
        let height = (api.height)(image.0, CHANNEL_INTERLEAVED);
        let mut stride = 0;
        let data = (api.plane)(image.0, CHANNEL_INTERLEAVED, &mut stride);
        if data.is_null() || width <= 0 || height <= 0 || stride < width * 4 {
            bail!("libheif returned no pixels");
        }
        let (width, height, stride) = (width as usize, height as usize, stride as usize);
        if (width * height * 4) as u64 > MAX_BYTES {
            bail!("image too large ({width} × {height})");
        }
        let mut pixels = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            pixels.extend_from_slice(std::slice::from_raw_parts(data.add(y * stride), width * 4));
        }
        let rgba = RgbaImage::from_raw(width as u32, height as u32, pixels)
            .expect("buffer sized from dimensions");
        let source = rgba.dimensions();
        Ok(Pixels {
            rgba,
            source,
            alpha,
        })
    }
}
