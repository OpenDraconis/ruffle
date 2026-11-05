use std::ffi::{CStr, CString, c_char};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use dv2ruffle::{Def, dv2r_add_fonts_from_movie, dv2r_advance, dv2r_close, dv2r_free, dv2r_invoke, dv2r_load, dv2r_render, dv2r_set_file_opener, font_parses, open_shared};
use ruffle_render_wgpu::backend::{create_wgpu_instance, request_adapter_and_device};
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::target::TextureTarget;
use ruffle_render_wgpu::wgpu;

const BURST: usize = 100;
const ROUNDS: usize = 4;
const OPENS: usize = 40;
const RETAINED_LIMIT_BYTES: usize = 64 * 1024 * 1024;
const OPEN_LIMIT_MS: f64 = 20.0;

fn gui() -> PathBuf {
    std::env::var_os("DV2_GUI").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap()).join("dv2-export/gui/GUI/Win32")
    })
}

fn rss() -> usize {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
    statm.split_whitespace().nth(1).unwrap().parse::<usize>().unwrap() * 4096
}

fn gpu() -> Arc<Descriptors> {
    let backends = wgpu::Backends::VULKAN;
    let instance = create_wgpu_instance(backends, wgpu::BackendOptions::default(), None);
    let (adapter, device, queue) = futures::executor::block_on(request_adapter_and_device(
        backends,
        &instance,
        None,
        wgpu::PowerPreference::HighPerformance,
    ))
    .unwrap();
    Arc::new(Descriptors::new(instance, adapter, device, queue))
}

fn path(name: &str) -> CString {
    CString::new(gui().join(name).to_str().unwrap()).unwrap()
}

unsafe extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn free(p: *mut u8);
}

unsafe extern "C" fn opener(path: *const c_char, length: *mut usize) -> *mut u8 {
    let Ok(bytes) = std::fs::read(unsafe { CStr::from_ptr(path) }.to_str().unwrap()) else {
        return std::ptr::null_mut();
    };
    let data = unsafe { malloc(bytes.len()) };
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
        *length = bytes.len();
    }
    data
}

fn setup() -> (Arc<Descriptors>, *mut Def) {
    dv2r_set_file_opener(opener, free);
    let fonts = path("fonts_en.gfx");
    assert!(unsafe { dv2r_add_fonts_from_movie(fonts.as_ptr()) });
    let def = unsafe { dv2r_load(path("hitDamage.gfx").as_ptr()) };
    assert!(!def.is_null());
    (gpu(), def)
}

fn frame(m: *mut dv2ruffle::Movie, (width, height): (u32, u32)) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let name = CString::new("setHit").unwrap();
    let args = CString::new("shit 25\x1fn800\x1fn600\x1fn16777215").unwrap();
    unsafe {
        dv2r_advance(m, 0.05, 1);
        dv2r_invoke(m, name.as_ptr(), args.as_ptr(), std::ptr::null_mut(), 0);
        for _ in 0..10 {
            dv2r_advance(m, 0.05, 1);
        }
        assert!(dv2r_render(m, pixels.as_mut_ptr(), pixels.len()));
    }
    pixels
}

fn open(descriptors: &Arc<Descriptors>, def: *const Def) -> *mut dv2ruffle::Movie {
    let m = open_shared(unsafe { &*def }, descriptors.clone(), TextureTarget::new(&descriptors.device, unsafe { &*def }.size()).unwrap());
    assert!(!m.is_null());
    m
}

fn burst(descriptors: &Arc<Descriptors>, def: *const Def) {
    let movies: Vec<_> = (0..BURST)
        .map(|_| {
            let m = open(descriptors, def);
            frame(m, unsafe { &*def }.size());
            m
        })
        .collect();
    for m in movies {
        unsafe { dv2r_close(m) };
    }
}

#[test]
fn closed_movies_return_their_memory() {
    let (descriptors, def) = setup();
    let warm = open(&descriptors, def);
    frame(warm, unsafe { &*def }.size());
    unsafe { dv2r_close(warm) };
    let before = rss();
    for _ in 0..ROUNDS {
        burst(&descriptors, def);
    }
    let retained = rss().saturating_sub(before);
    unsafe { dv2r_free(def) };
    assert!(
        retained < RETAINED_LIMIT_BYTES,
        "{} MiB still resident after {ROUNDS} bursts of {BURST} opens and closes",
        retained >> 20
    );
}

#[test]
fn fonts_are_parsed_once_and_an_open_is_fast() {
    let (descriptors, def) = setup();
    let first = font_parses();
    unsafe { dv2r_close(open(&descriptors, def)) };
    let start = Instant::now();
    let movies: Vec<_> = (0..OPENS)
        .map(|_| open(&descriptors, def))
        .collect();
    let mean = start.elapsed().as_secs_f64() * 1000.0 / OPENS as f64;
    for m in movies {
        unsafe { dv2r_close(m) };
    }
    unsafe { dv2r_free(def) };
    let parsed = font_parses() - first;
    assert!(parsed <= 1, "fonts_en.gfx parsed {parsed} times for {OPENS} opens");
    assert!(mean < OPEN_LIMIT_MS, "{mean:.1} ms per open, limit {OPEN_LIMIT_MS}");
}

#[test]
fn a_hit_text_is_drawn() {
    let (descriptors, def) = setup();
    let m = open(&descriptors, def);
    let pixels = frame(m, unsafe { &*def }.size());
    unsafe { dv2r_close(m) };
    unsafe { dv2r_free(def) };
    let lit = pixels.chunks(4).filter(|p| p[3] != 0).count();
    assert!(lit > 100, "{lit} pixels drawn");
}

#[test]
fn every_movie_opens_with_the_shared_fonts() {
    dv2r_set_file_opener(opener, free);
    let fonts = path("fonts_en.gfx");
    assert!(unsafe { dv2r_add_fonts_from_movie(fonts.as_ptr()) });
    let descriptors = gpu();
    let mut names: Vec<_> = std::fs::read_dir(gui())
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".gfx") && !n.starts_with("fonts_"))
        .collect();
    names.sort();
    for name in names {
        let def = unsafe { dv2r_load(path(&name).as_ptr()) };
        assert!(!def.is_null(), "{name}");
        let m = open(&descriptors, def);
        let mut pixels = vec![0u8; (unsafe { &*def }.size().0 * unsafe { &*def }.size().1 * 4) as usize];
        unsafe {
            for _ in 0..5 {
                dv2r_advance(m, 0.05, 1);
            }
            assert!(dv2r_render(m, pixels.as_mut_ptr(), pixels.len()), "{name}");
            dv2r_close(m);
            dv2r_free(def);
        }
    }
}
