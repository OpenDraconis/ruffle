use std::collections::HashMap;
use std::ffi::{CString, c_char};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, Weak};

use ruffle_render::bitmap::{BitmapHandle, BitmapHandleImpl};
use ruffle_render_wgpu::Texture;
use ruffle_render_wgpu::backend::ExternalImages;
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::wgpu;

type Open = unsafe extern "C" fn(*const c_char, *mut usize) -> *mut u8;
type Free = unsafe extern "C" fn(*mut u8);
type Resolve = unsafe extern "C" fn(*const c_char, *mut c_char, usize, *mut u32, *mut u32) -> bool;

static OPENER: Mutex<Option<(Open, Free)>> = Mutex::new(None);
static RESOLVER: Mutex<Option<(Resolve, String)>> = Mutex::new(None);
struct Cached(Weak<dyn BitmapHandleImpl>);

unsafe impl Send for Cached {}

static CACHE: LazyLock<Mutex<HashMap<String, Cached>>> = LazyLock::new(Default::default);

#[unsafe(no_mangle)]
pub extern "C" fn dv2r_set_file_opener(open: Open, free: Free) {
    *OPENER.lock().unwrap() = Some((open, free));
}

#[unsafe(no_mangle)]
pub extern "C" fn dv2r_set_image_resolver(resolve: Resolve, empty: *const c_char) {
    let empty = unsafe { std::ffi::CStr::from_ptr(empty) }.to_string_lossy().into_owned();
    *RESOLVER.lock().unwrap() = Some((resolve, empty));
}

// CUIImageLoader::ConvertPath @6e5a00 decomp
fn resolve(url: &str) -> Option<(String, u32, u32)> {
    let resolve = RESOLVER.lock().unwrap().as_ref()?.0;
    let url = CString::new(url).ok()?;
    let mut path = vec![0u8; 1024];
    let (mut width, mut height) = (0, 0);
    if !unsafe {
        resolve(
            url.as_ptr(),
            path.as_mut_ptr() as *mut c_char,
            path.len(),
            &mut width,
            &mut height,
        )
    } {
        return None;
    }
    let end = path.iter().position(|b| *b == 0)?;
    Some((String::from_utf8_lossy(&path[..end]).into_owned(), width, height))
}

// FxPlayerNDLMemoryOpener, GFxLoadStates::BuildURL decomp
fn read(url: &str) -> Option<Vec<u8>> {
    let path = url::Url::parse(url).ok()?.to_file_path().ok()?;
    read_path(&path.to_string_lossy())
}

fn read_path(path: &str) -> Option<Vec<u8>> {
    let path = CString::new(path).ok()?;
    let (open, free) = (*OPENER.lock().unwrap())?;
    let mut length = 0;
    let data = unsafe { open(path.as_ptr(), &mut length) };
    if data.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data, length) }.to_vec();
    unsafe { free(data) };
    Some(bytes)
}

fn word(dds: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(dds[at..at + 4].try_into().unwrap())
}

// GRendererD3D9Impl::InitTexture @e3c100 decomp
fn texture(descriptors: &Descriptors, dds: &[u8]) -> Option<wgpu::Texture> {
    if dds.len() < 128 || &dds[..4] != b"DDS " {
        return None;
    }
    let height = word(dds, 12);
    let width = word(dds, 16);
    let levels = word(dds, 28).max(1);
    let compressed = word(dds, 80) & 4 != 0;
    let (format, block_bytes) = if compressed {
        match &dds[84..88] {
            b"DXT1" => (wgpu::TextureFormat::Bc1RgbaUnorm, 8),
            b"DXT3" => (wgpu::TextureFormat::Bc2RgbaUnorm, 16),
            b"DXT5" => (wgpu::TextureFormat::Bc3RgbaUnorm, 16),
            _ => return None,
        }
    } else if word(dds, 88) == 32 && word(dds, 92) == 0xff0000 && word(dds, 100) == 0xff {
        (wgpu::TextureFormat::Bgra8Unorm, 4)
    } else {
        return None;
    };
    let opaque = !compressed && word(dds, 104) == 0;
    let texture = descriptors.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("GFx image"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut at = 128;
    for level in 0..levels {
        let (w, h) = ((width >> level).max(1), (height >> level).max(1));
        let (columns, rows, row_bytes) = if compressed {
            let (c, r) = (w.div_ceil(4), h.div_ceil(4));
            (c * 4, r * 4, c * block_bytes)
        } else {
            (w, h, w * 4)
        };
        let size = (row_bytes * if compressed { rows / 4 } else { rows }) as usize;
        let mut data = dds.get(at..at + size)?.to_vec();
        at += size;
        if opaque {
            data.iter_mut().skip(3).step_by(4).for_each(|a| *a = 255);
        }
        descriptors.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: columns,
                height: rows,
                depth_or_array_layers: 1,
            },
        );
    }
    Some(texture)
}

pub struct Images(pub Arc<Descriptors>, pub OnceLock<Option<wgpu::Texture>>);

struct Pending(BitmapHandle);

unsafe impl Send for Pending {}

impl Images {
    // GNDLNiPixelInfoBase::GNDLNiPixelInfoBase @114f200, GNDLNiPixelInfoBase::GetTexture @114f010 decomp
    fn runtime(&self, url: &str) -> Option<(BitmapHandle, u32, u32)> {
        let (path, width, height) = resolve(url)?;
        let mut cache = CACHE.lock().unwrap();
        if let Some(handle) = cache.get(&path).and_then(|c| c.0.upgrade()) {
            return Some((BitmapHandle(handle), width, height));
        }
        let empty = self.1.get_or_init(|| {
            let path = RESOLVER.lock().unwrap().as_ref()?.1.clone();
            let texture = read_path(&path).and_then(|dds| texture(&self.0, &dds));
            if texture.is_none() {
                eprintln!("[dv2ruffle] no empty image {path}");
            }
            texture
        });
        let handle = Texture::external(empty.clone()?, true);
        cache.insert(path.clone(), Cached(Arc::downgrade(&handle.0)));
        let descriptors = self.0.clone();
        let pending = Pending(handle.clone());
        std::thread::spawn(move || {
            let pending = pending;
            let Some(loaded) = read_path(&path).and_then(|dds| texture(&descriptors, &dds)) else {
                eprintln!("[dv2ruffle] no image {path}");
                return;
            };
            ruffle_render_wgpu::as_texture(&pending.0)
                .forward_to(Texture::external(loaded, true));
        });
        Some((handle, width, height))
    }
}

impl ExternalImages for Images {
    // GFxImageFileResourceCreator::CreateResource @ed1780, CScaleFormTextureManager::Register @107f6f0 decomp
    fn image(&self, url: &str) -> Option<(BitmapHandle, u32, u32)> {
        if url.starts_with("img://") {
            return self.runtime(url);
        }
        let mut cache = CACHE.lock().unwrap();
        if let Some(handle) = cache.get(url).and_then(|c| c.0.upgrade()) {
            return Some((BitmapHandle(handle), 0, 0));
        }
        let Some(texture) = read(url).and_then(|dds| texture(&self.0, &dds)) else {
            eprintln!("[dv2ruffle] no image {url}");
            return None;
        };
        let (width, height) = (texture.width(), texture.height());
        let handle = Texture::external(texture, true);
        cache.insert(url.to_string(), Cached(Arc::downgrade(&handle.0)));
        Some((handle, width, height))
    }
}
