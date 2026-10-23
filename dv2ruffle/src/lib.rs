mod fonts;
mod images;
mod unity;

use std::any::Any;
use std::collections::VecDeque;
use std::ffi::{CStr, c_char};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ruffle_core::backend::log::LogBackend;
use ruffle_core::backend::navigator::{NullExecutor, NullNavigatorBackend};
use ruffle_core::events::{
    KeyCode, MouseButton, MouseWheelDelta, PlayerEvent, TextControlCode,
};
use ruffle_core::external::{FsCommandProvider, Value};
use ruffle_core::limits::ExecutionLimit;
use ruffle_core::tag_utils::{SwfMovieData, movie_from_path};
use ruffle_core::{FloatDuration, Player, PlayerBuilder};
use ruffle_render_wgpu::backend::{
    WgpuRenderBackend, create_wgpu_instance, request_adapter_and_device,
};
use ruffle_render_wgpu::buffer_pool::TexturePool;
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::target::TextureTarget;
use ruffle_render_wgpu::wgpu;

type Queue = Arc<Mutex<VecDeque<String>>>;

struct Log(Queue);
impl LogBackend for Log {
    fn avm_trace(&self, m: &str) {
        self.0.lock().unwrap().push_back(format!("trace\x1f{m}"));
    }
    fn avm_warning(&self, m: &str) {
        self.0.lock().unwrap().push_back(format!("warning\x1f{m}"));
    }
}

struct Fs(Queue);
impl FsCommandProvider for Fs {
    fn on_fs_command(&self, c: &str, a: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .push_back(format!("fscommand\x1f{c}\x1f{a}"));
        true
    }
}

// CGFxPlayStream::CGFxPlayStream @107f430 decomp
static DESKTOP_ASPECT: AtomicU32 = AtomicU32::new(0x3f80_0000);

pub struct Movie {
    player: Arc<Mutex<Player>>,
    desktop_aspect: f32,
    executor: NullExecutor,
    events: Queue,
    width: u32,
    height: u32,
    pub last_render_ms: f32,
    result: Vec<u8>,
}

pub struct Def {
    path: PathBuf,
    movie: SwfMovieData,
}

// CMovieStreamLetter::Execute @107fab0, CScaleformHub::ProcessRequests @107de10 decomp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_load(path: *const c_char) -> *mut Def {
    let path = PathBuf::from(unsafe { text(path) });
    match movie_from_path(&path, None) {
        Ok(movie) => Box::into_raw(Box::new(Def { path, movie })),
        Err(_) => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_free(d: *mut Def) {
    if !d.is_null() {
        drop(unsafe { Box::from_raw(d) });
    }
}

fn build(
    def: &Def,
    width: u32,
    height: u32,
    renderer: Box<dyn ruffle_render::backend::RenderBackend>,
) -> *mut Movie {
    let path = &def.path;
    let movie = def.movie.clone();
    let executor = NullExecutor::new();
    let Ok(navigator) = NullNavigatorBackend::with_base_path(path.parent().unwrap(), &executor)
    else {
        return std::ptr::null_mut();
    };
    let events: Queue = Default::default();
    let player = PlayerBuilder::new()
        .with_boxed_renderer(renderer)
        .with_navigator(navigator)
        .with_log(Log(events.clone()))
        .with_fs_commands(Box::new(Fs(events.clone())))
        .with_movie(movie)
        .with_autoplay(true)
        .with_viewport_dimensions(width, height, 1.0)
        .build();
    player.lock().unwrap().set_window_mode("transparent");
    player.lock().unwrap().enable_gfx_view();
    fonts::install(&mut player.lock().unwrap());
    Box::into_raw(Box::new(Movie {
        player,
        desktop_aspect: f32::from_bits(DESKTOP_ASPECT.load(Ordering::Relaxed)),
        executor,
        events,
        width,
        height,
        last_render_ms: 0.0,
        result: Vec::new(),
    }))
}

static POOL: std::sync::OnceLock<Arc<Mutex<TexturePool>>> = std::sync::OnceLock::new();

// CGFxPlayStream::Notifyloaded @107e6c0 decomp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_open_on_unity(def: *const Def) -> *mut Movie {
    let def = unsafe { &*def };
    let Some(descriptors) = unity::gpu() else {
        return std::ptr::null_mut();
    };
    let Ok(mut renderer) = WgpuRenderBackend::new(descriptors.clone(), unity::UnityTarget { width: 1, height: 1, built: Mutex::new(None) }) else {
        return std::ptr::null_mut();
    };
    // CGFxPlayStreamManager::Initialize @10810d0 decomp
    renderer.disable_msaa();
    // CGFxPlayStreamManager::Initialize @10810d0 decomp
    renderer.set_shared_texture_pool(POOL.get_or_init(Default::default).clone());
    renderer.set_external_images(Arc::new(images::Images(descriptors, Default::default())));
    build(def, 1, 1, Box::new(renderer))
}

unsafe fn text(p: *const c_char) -> String {
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_open(path: *const c_char, width: u32, height: u32) -> *mut Movie {
    let path = PathBuf::from(unsafe { text(path) });
    let Ok(movie) = movie_from_path(&path, None) else {
        return std::ptr::null_mut();
    };
    let def = Def { path, movie };
    let backends = wgpu::Backends::VULKAN;
    let instance = create_wgpu_instance(backends, wgpu::BackendOptions::default(), None);
    let Ok((adapter, device, queue)) = futures::executor::block_on(request_adapter_and_device(
        backends,
        &instance,
        None,
        wgpu::PowerPreference::HighPerformance,
    )) else {
        return std::ptr::null_mut();
    };
    let descriptors = Arc::new(Descriptors::new(instance, adapter, device, queue));
    let Ok(target) = TextureTarget::new(&descriptors.device, (width, height)) else {
        return std::ptr::null_mut();
    };
    let Ok(mut renderer) = WgpuRenderBackend::new(descriptors.clone(), target) else {
        return std::ptr::null_mut();
    };
    renderer.set_external_images(Arc::new(images::Images(descriptors, Default::default())));
    build(&def, width, height, Box::new(renderer))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_close(m: *mut Movie) {
    if !m.is_null() {
        drop(unsafe { Box::from_raw(m) });
    }
}

// CGFxPlayStreamManager::SetDesktopAspectRatio @495200, CGFxPlayStreamManager::CreatePlayStream @1081430 decomp
#[unsafe(no_mangle)]
pub extern "C" fn dv2r_set_desktop_aspect_ratio(aspect: f32) {
    DESKTOP_ASPECT.store(aspect.to_bits(), Ordering::Relaxed);
}

// CGFxPlayStream::Render @107ed80, GFxMovieDefImpl::CreateInstance @ec7fe0 decomp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_set_view(m: *mut Movie, width: u32, height: u32) {
    let m = unsafe { &mut *m };
    m.width = width;
    m.height = height;
    m.player.lock().unwrap().set_gfx_view(width, height, m.desktop_aspect);
}

// CGFxPlayStream::Advance @107e430 decomp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_advance(m: *mut Movie, seconds: f64, catch_up: u32) {
    let m = unsafe { &mut *m };
    m.executor.run();
    let mut p = m.player.lock().unwrap();
    p.preload(&mut ExecutionLimit::none());
    p.advance(FloatDuration::from_secs(seconds), catch_up);
}

// CGFxPlayStream::Invoke @107e990 decomp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_invoke(
    m: *mut Movie,
    name: *const c_char,
    args: *const c_char,
    result: *mut u8,
    capacity: usize,
) -> i32 {
    let m = unsafe { &mut *m };
    let name = unsafe { text(name) };
    let args = unsafe { text(args) };
    let values = args
        .split('\x1f')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let (kind, v) = s.split_at(1);
            match kind {
                "n" => Value::Number(v.parse().unwrap_or(0.0)),
                "b" => Value::Bool(v == "1"),
                "u" => Value::Undefined,
                "l" => Value::Null,
                _ => Value::String(v.to_string()),
            }
        })
        .collect();
    let Some(text) = m.player.lock().unwrap().invoke(&name, values) else {
        return -1;
    };
    m.result = text.into_bytes();
    unsafe { dv2r_invoke_result(m, result, capacity) }
}

// CGFxPlayStream::Invoke @107e990 decomp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_invoke_result(m: *mut Movie, result: *mut u8, capacity: usize) -> i32 {
    let m = unsafe { &*m };
    let n = m.result.len().min(capacity);
    if !result.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(m.result.as_ptr(), result, n) };
    }
    m.result.len() as i32
}

// CGFxPlayStream::Advance @107e430 decomp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_frame_rate(m: *mut Movie) -> f64 {
    let m = unsafe { &mut *m };
    m.player.lock().unwrap().frame_rate()
}

// CGFxPlayStream::HitTest @107e320 decomp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_hit_test(m: *mut Movie, x: f64, y: f64) -> bool {
    let m = unsafe { &mut *m };
    m.player.lock().unwrap().hit_test(x, y)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_render(m: *mut Movie, rgba: *mut u8, len: usize) -> bool {
    let m = unsafe { &mut *m };
    let start = Instant::now();
    let mut p = m.player.lock().unwrap();
    p.set_gfx_view(m.width, m.height, m.desktop_aspect);
    p.render();
    let r = <dyn Any>::downcast_mut::<WgpuRenderBackend<TextureTarget>>(p.renderer_mut()).unwrap();
    let Some(image) = r.capture_frame() else {
        return false;
    };
    let raw = image.as_raw();
    if raw.len() != len {
        return false;
    }
    let row = (m.width * 4) as usize;
    let out = unsafe { std::slice::from_raw_parts_mut(rgba, len) };
    for y in 0..m.height as usize {
        let to = (m.height as usize - 1 - y) * row;
        out[to..to + row].copy_from_slice(&raw[y * row..y * row + row]);
    }
    m.last_render_ms = start.elapsed().as_secs_f32() * 1000.0;
    true
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_render_ms(m: *mut Movie) -> f32 {
    unsafe { &*m }.last_render_ms
}

// GFxMovieRoot::HandleEvent @f22530, GFxMovieRoot::OnMovieFocus @f20070 decomp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_focus_event(m: *mut Movie, focused: bool) {
    let m = unsafe { &mut *m };
    m.player.lock().unwrap().handle_event(if focused {
        PlayerEvent::FocusGained
    } else {
        PlayerEvent::FocusLost
    });
}

// GFxMovieRoot::HandleEvent @f22530, GFxInputEventsQueue::AddMouseWheel @f1d8e0, CUIObjectManager::Process @76db80 decomp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_mouse_event(
    m: *mut Movie,
    kind: i32,
    x: f64,
    y: f64,
    button: i32,
    scroll_delta: f32,
) {
    let m = unsafe { &mut *m };
    let mut p = m.player.lock().unwrap();
    let button = match button {
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        _ => MouseButton::Left,
    };
    p.handle_event(PlayerEvent::MouseMove { x, y });
    match kind {
        2 => {
            p.handle_event(PlayerEvent::MouseDown {
                x,
                y,
                button,
                index: None,
            });
        }
        3 => {
            p.handle_event(PlayerEvent::MouseUp { x, y, button });
        }
        4 => {
            p.handle_event(PlayerEvent::MouseWheel {
                delta: MouseWheelDelta::Lines(f64::from(scroll_delta as i32)),
            });
        }
        _ => {}
    }
}

// GFxTextKeyMap::InitWindowsKeyMap @ede990 decomp
fn text_control(key_code: u32) -> Option<TextControlCode> {
    Some(match key_code {
        0x26 => TextControlCode::LineUp,
        0x28 => TextControlCode::LineDown,
        0x25 => TextControlCode::MoveLeft,
        0x27 => TextControlCode::MoveRight,
        0x21 => TextControlCode::PageUp,
        0x22 => TextControlCode::PageDown,
        0x24 => TextControlCode::LineHome,
        0x23 => TextControlCode::LineEnd,
        0x08 => TextControlCode::Backspace,
        0x2e => TextControlCode::Delete,
        0x0d => TextControlCode::Enter,
        _ => return None,
    })
}

// GFxMovieRoot::HandleEvent @f22530, GFxEditTextCharacter::OnKeyEvent @ee7910, CUIObjectManager::ApplyUnicodeKey @76d910 decomp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_key_event(m: *mut Movie, kind: i32, key_code: u32, wchar: u32) {
    let m = unsafe { &mut *m };
    let mut p = m.player.lock().unwrap();
    let key_char = char::from_u32(wchar).filter(|_| wchar != 0);
    let key_code = KeyCode::from_code(key_code);
    if kind == 6 {
        p.handle_event(PlayerEvent::KeyCodeUp { key_code, key_char });
        return;
    }
    p.handle_event(PlayerEvent::KeyCodeDown { key_code, key_char });
    if let Some(code) = text_control(key_code.value()) {
        p.handle_event(PlayerEvent::TextControl { code });
    }
    if let Some(codepoint) = key_char.filter(|_| wchar > 0x1f && wchar != 0x7f) {
        p.handle_event(PlayerEvent::TextInput { codepoint });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_poll(m: *mut Movie, buffer: *mut u8, len: usize) -> i32 {
    let m = unsafe { &mut *m };
    let Some(event) = m.events.lock().unwrap().pop_front() else {
        return 0;
    };
    let bytes = event.as_bytes();
    let n = bytes.len().min(len);
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, n) };
    n as i32
}
