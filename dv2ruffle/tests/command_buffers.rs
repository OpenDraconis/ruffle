use ruffle_render_wgpu::wgpu;

const ENCODERS: usize = 48;
const PASSES: usize = 300;
const HELD_LIMIT: usize = 64 * 1024 * 1024;

#[repr(C)]
#[derive(Default)]
struct MallInfo2 {
    arena: usize,
    ordblks: usize,
    smblks: usize,
    hblks: usize,
    hblkhd: usize,
    usmblks: usize,
    fsmblks: usize,
    uordblks: usize,
    fordblks: usize,
    keepcost: usize,
}

unsafe extern "C" {
    fn mallinfo2() -> MallInfo2;
}

fn heap_in_use() -> usize {
    let info = unsafe { mallinfo2() };
    info.uordblks + info.hblkhd
}

fn round(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView, passes: usize) {
    let buffers: Vec<_> = (0..ENCODERS)
        .map(|_| {
            let mut encoder = device.create_command_encoder(&Default::default());
            for _ in 0..passes {
                encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
            }
            encoder.finish()
        })
        .collect();
    queue.submit(buffers);
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
}

#[test]
fn encoders_give_command_buffers_back_after_a_frame_with_many_passes() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter =
        futures::executor::block_on(instance.request_adapter(&Default::default())).unwrap();
    let (device, queue) =
        futures::executor::block_on(adapter.request_device(&Default::default())).unwrap();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());

    round(&device, &queue, &view, 1);
    let before = heap_in_use();
    round(&device, &queue, &view, PASSES);
    round(&device, &queue, &view, 1);
    let held = heap_in_use().saturating_sub(before);

    assert!(
        held < HELD_LIMIT,
        "{} MiB still held after the heavy frame",
        held / (1024 * 1024)
    );
}
