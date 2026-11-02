use std::sync::Arc;

use ruffle_render::backend::RenderBackend;
use ruffle_render::commands::{CommandHandler, CommandList, RenderBlendMode};
use ruffle_render::matrix::Matrix;
use ruffle_render_wgpu::backend::WgpuRenderBackend;
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::target::TextureTarget;
use ruffle_render_wgpu::wgpu;
use swf::{BlendMode, Color};

const SIZE: u32 = 32;
const GREY: Color = Color {
    r: 128,
    g: 128,
    b: 128,
    a: 255,
};

fn backend() -> WgpuRenderBackend<TextureTarget> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter =
        futures::executor::block_on(instance.request_adapter(&Default::default())).unwrap();
    let (device, queue) =
        futures::executor::block_on(adapter.request_device(&Default::default())).unwrap();
    let target = TextureTarget::new(&device, (SIZE, SIZE)).unwrap();
    let descriptors = Arc::new(Descriptors::new(instance, adapter, device, queue));
    WgpuRenderBackend::new(descriptors, target).unwrap()
}

fn rect(list: &mut CommandList, color: Color) {
    list.draw_rect(color, Matrix::scale(SIZE as f32, SIZE as f32));
}

fn multiplied(color: Color) -> CommandList {
    let mut inner = CommandList::new();
    rect(&mut inner, color);
    let mut list = CommandList::new();
    list.blend(inner, RenderBlendMode::Builtin(BlendMode::Multiply));
    list
}

fn pixel(backend: &mut WgpuRenderBackend<TextureTarget>, commands: CommandList) -> [u8; 4] {
    backend.submit_frame(GREY, commands, vec![]);
    let [r, g, b, a] = backend
        .capture_frame()
        .unwrap()
        .get_pixel(SIZE / 2, SIZE / 2)
        .0;
    let stored = |c: u8| (c as u32 * a as u32 / 255) as u8;
    [stored(r), stored(g), stored(b), a]
}

fn assert_near(actual: [u8; 4], expected: [u8; 4]) {
    for (a, e) in actual.iter().zip(expected) {
        assert!(
            (*a as i32 - e as i32).abs() <= 2,
            "{actual:?} != {expected:?}"
        );
    }
}

// GRendererD3D9Impl::ApplyBlendMode @e3d160, GRendererD3D9Impl::ApplyColor @e3d3e0 decomp
#[test]
fn multiply_writes_what_gfx_writes() {
    let mut backend = backend();
    let opaque = pixel(
        &mut backend,
        multiplied(Color {
            r: 255,
            g: 128,
            b: 0,
            a: 255,
        }),
    );
    assert_near(opaque, [128, 64, 0, 255]);
    let half = pixel(
        &mut backend,
        multiplied(Color {
            r: 255,
            g: 0,
            b: 0,
            a: 128,
        }),
    );
    assert_near(half, [128, 64, 64, 128]);
}

// GRendererD3D9Impl::PushBlendMode @e401e0 decomp
#[test]
fn multiply_draws_on_the_target_without_another_texture() {
    let mut backend = backend();
    let textures = |backend: &WgpuRenderBackend<TextureTarget>| {
        backend
            .descriptors()
            .device
            .get_internal_counters()
            .hal
            .texture_memory
            .read()
    };
    let mut normal = CommandList::new();
    for _ in 0..50 {
        rect(&mut normal, GREY);
    }
    backend.submit_frame(GREY, normal, vec![]);
    let before = textures(&backend);
    let mut list = CommandList::new();
    for _ in 0..50 {
        let mut inner = CommandList::new();
        rect(&mut inner, GREY);
        list.blend(inner, RenderBlendMode::Builtin(BlendMode::Multiply));
    }
    backend.submit_frame(GREY, list, vec![]);
    assert_eq!(textures(&backend), before);
}
