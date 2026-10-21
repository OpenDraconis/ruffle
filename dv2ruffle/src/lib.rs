use std::any::Any;
use std::collections::VecDeque;
use std::ffi::{CStr, c_char};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ruffle_core::backend::log::LogBackend;
use ruffle_core::backend::navigator::{NullExecutor, NullNavigatorBackend};
use ruffle_core::events::{MouseButton, PlayerEvent};
use ruffle_core::external::{FsCommandProvider, Value};
use ruffle_core::limits::ExecutionLimit;
use ruffle_core::tag_utils::movie_from_path;
use ruffle_core::{FloatDuration, Player, PlayerBuilder};
use ruffle_render_wgpu::backend::{
    WgpuRenderBackend, create_wgpu_instance, request_adapter_and_device,
};
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

pub struct Movie {
    player: Arc<Mutex<Player>>,
    executor: NullExecutor,
    events: Queue,
    width: u32,
    height: u32,
    pub last_render_ms: f32,
}

unsafe fn text(p: *const c_char) -> String {
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_open(path: *const c_char, width: u32, height: u32) -> *mut Movie {
    let path = PathBuf::from(unsafe { text(path) });
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
    let Ok(movie) = movie_from_path(&path, None) else {
        return std::ptr::null_mut();
    };
    let Ok(target) = TextureTarget::new(&descriptors.device, (width, height)) else {
        return std::ptr::null_mut();
    };
    let Ok(renderer) = WgpuRenderBackend::new(descriptors, target) else {
        return std::ptr::null_mut();
    };
    let executor = NullExecutor::new();
    let Ok(navigator) = NullNavigatorBackend::with_base_path(path.parent().unwrap(), &executor)
    else {
        return std::ptr::null_mut();
    };
    let events: Queue = Default::default();
    let player = PlayerBuilder::new()
        .with_renderer(renderer)
        .with_navigator(navigator)
        .with_log(Log(events.clone()))
        .with_fs_commands(Box::new(Fs(events.clone())))
        .with_movie(movie)
        .with_autoplay(true)
        .with_viewport_dimensions(width, height, 1.0)
        .build();
    player.lock().unwrap().set_window_mode("transparent");
    Box::into_raw(Box::new(Movie {
        player,
        executor,
        events,
        width,
        height,
        last_render_ms: 0.0,
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_close(m: *mut Movie) {
    if !m.is_null() {
        drop(unsafe { Box::from_raw(m) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_advance(m: *mut Movie, seconds: f64) {
    let m = unsafe { &mut *m };
    m.executor.run();
    let mut p = m.player.lock().unwrap();
    p.preload(&mut ExecutionLimit::none());
    p.tick(FloatDuration::from_secs(seconds));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_invoke(m: *mut Movie, name: *const c_char, args: *const c_char) {
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
                _ => Value::String(v.to_string()),
            }
        })
        .collect();
    m.player.lock().unwrap().invoke(&name, values);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_render(m: *mut Movie, rgba: *mut u8, len: usize) -> bool {
    let m = unsafe { &mut *m };
    let start = Instant::now();
    let mut p = m.player.lock().unwrap();
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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_mouse(m: *mut Movie, x: f64, y: f64, button: i32) {
    let m = unsafe { &mut *m };
    let event = match button {
        1 => PlayerEvent::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            index: None,
        },
        2 => PlayerEvent::MouseUp {
            x,
            y,
            button: MouseButton::Left,
        },
        _ => PlayerEvent::MouseMove { x, y },
    };
    m.player.lock().unwrap().handle_event(event);
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
