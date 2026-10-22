use std::ffi::{CStr, c_char, c_void};
use std::mem::{offset_of, size_of};
use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use ash::vk;
use ash::vk::TaggedStructure;
use ruffle_render_wgpu::backend::device_features_and_limits;
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::target::{RenderTarget, RenderTargetFrame};
use ruffle_render_wgpu::wgpu;
use wgpu::hal;
use wgpu::hal::api::Vulkan;

use crate::Movie;

#[repr(C)]
pub struct IUnityInterfaces {
    get_interface: *const c_void,
    register_interface: *const c_void,
    get_interface_split: unsafe extern "C" fn(u64, u64) -> *mut c_void,
    register_interface_split: *const c_void,
}

#[repr(C)]
struct IUnityGraphics {
    get_renderer: unsafe extern "C" fn() -> i32,
    register_device_event_callback: unsafe extern "C" fn(unsafe extern "C" fn(i32)),
    unregister_device_event_callback: unsafe extern "C" fn(unsafe extern "C" fn(i32)),
    reserve_event_id_range: unsafe extern "C" fn(i32) -> i32,
}

#[repr(C)]
struct UnityVulkanInstance {
    pipeline_cache: vk::PipelineCache,
    instance: vk::Instance,
    physical_device: vk::PhysicalDevice,
    device: vk::Device,
    graphics_queue: vk::Queue,
    get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    queue_family_index: u32,
    reserved: [*mut c_void; 8],
}

#[repr(C)]
struct UnityVulkanMemory {
    memory: vk::DeviceMemory,
    offset: vk::DeviceSize,
    size: vk::DeviceSize,
    mapped: *mut c_void,
    flags: vk::MemoryPropertyFlags,
    memory_type_index: u32,
    reserved: [*mut c_void; 4],
}

#[repr(C)]
struct UnityVulkanImage {
    memory: UnityVulkanMemory,
    image: vk::Image,
    layout: vk::ImageLayout,
    aspect: vk::ImageAspectFlags,
    usage: vk::ImageUsageFlags,
    format: vk::Format,
    extent: vk::Extent3D,
    tiling: vk::ImageTiling,
    image_type: vk::ImageType,
    samples: vk::SampleCountFlags,
    layers: i32,
    mip_count: i32,
    reserved: [*mut c_void; 4],
}

#[repr(C)]
struct UnityVulkanPluginEventConfig {
    render_pass_precondition: i32,
    graphics_queue_access: i32,
    flags: u32,
}

type InitCallback = unsafe extern "C" fn(
    vk::PFN_vkGetInstanceProcAddr,
    *mut c_void,
) -> vk::PFN_vkGetInstanceProcAddr;

#[repr(C)]
struct IUnityGraphicsVulkanV2 {
    intercept_initialization: unsafe extern "C" fn(InitCallback, *mut c_void) -> bool,
    intercept_vulkan_api: *const c_void,
    configure_event: unsafe extern "C" fn(i32, *const UnityVulkanPluginEventConfig),
    instance: unsafe extern "C" fn() -> UnityVulkanInstance,
    command_recording_state: *const c_void,
    access_texture: *const c_void,
    access_render_buffer_texture: unsafe extern "C" fn(
        *mut c_void,
        *const vk::ImageSubresource,
        vk::ImageLayout,
        vk::PipelineStageFlags,
        vk::AccessFlags,
        i32,
        *mut UnityVulkanImage,
    ) -> bool,
    access_render_buffer_resolve_texture: *const c_void,
    access_buffer: *const c_void,
    ensure_outside_render_pass: *const c_void,
    ensure_inside_render_pass: *const c_void,
    access_queue: *const c_void,
    configure_swapchain: *const c_void,
    access_texture_by_id: *const c_void,
    add_intercept_initialization: unsafe extern "C" fn(InitCallback, *mut c_void, i32) -> bool,
    remove_intercept_initialization: *const c_void,
}

const GRAPHICS_GUID: (u64, u64) = (0x7CBA0A9CA4DDB544, 0x8C5AD4926EB17B11);
const VULKAN_V2_GUID: (u64, u64) = (0x329334C09DCA4787, 0xB347DD92A0097FFC);
const RENDERER_VULKAN: i32 = 21;
const DEVICE_EVENT_INITIALIZE: i32 = 0;
const DEVICE_EVENT_SHUTDOWN: i32 = 1;
const RENDER_PASS_ENSURE_OUTSIDE: i32 = 2;
const QUEUE_ACCESS_ALLOW: i32 = 1;
const EVENT_ENSURE_PREVIOUS_FRAME_SUBMISSION: u32 = 1;
const EVENT_FLUSH_COMMAND_BUFFERS: u32 = 2;
const EVENT_MODIFIES_COMMAND_BUFFERS_STATE: u32 = 8;
const ACCESS_PIPELINE_BARRIER: i32 = 1;

struct Hook {
    get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    instance: vk::Instance,
    api_version: u32,
    instance_extensions: Vec<&'static CStr>,
    prepared: Option<Prepared>,
}

struct Prepared {
    instance: hal::vulkan::Instance,
    adapter: hal::ExposedAdapter<Vulkan>,
    extensions: Vec<&'static CStr>,
    features: wgpu::Features,
    limits: wgpu::Limits,
}

unsafe impl Send for Hook {}

static GRAPHICS: AtomicPtr<IUnityGraphics> = AtomicPtr::new(std::ptr::null_mut());
static VULKAN: AtomicPtr<IUnityGraphicsVulkanV2> = AtomicPtr::new(std::ptr::null_mut());
static EVENT: AtomicI32 = AtomicI32::new(-1);
static HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static GPU: OnceLock<Arc<Descriptors>> = OnceLock::new();
static FRAME: Mutex<Option<wgpu::Texture>> = Mutex::new(None);

fn leak(name: &CStr) -> &'static CStr {
    Box::leak(name.to_owned().into_boxed_c_str())
}

unsafe fn names(count: u32, pointers: *const *const c_char) -> Vec<&'static CStr> {
    (0..count as usize)
        .map(|i| leak(unsafe { CStr::from_ptr(*pointers.add(i)) }))
        .collect()
}

fn entry(get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr) -> ash::Entry {
    unsafe {
        ash::Entry::from_static_fn(ash::StaticFn {
            get_instance_proc_addr,
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn UnityPluginLoad(interfaces: *mut IUnityInterfaces) {
    let interfaces = unsafe { &*interfaces };
    let graphics = unsafe { (interfaces.get_interface_split)(GRAPHICS_GUID.0, GRAPHICS_GUID.1) }
        as *mut IUnityGraphics;
    let vulkan = unsafe { (interfaces.get_interface_split)(VULKAN_V2_GUID.0, VULKAN_V2_GUID.1) }
        as *mut IUnityGraphicsVulkanV2;
    GRAPHICS.store(graphics, Ordering::SeqCst);
    VULKAN.store(vulkan, Ordering::SeqCst);
    if vulkan.is_null() {
        eprintln!("[dv2ruffle] no IUnityGraphicsVulkanV2");
    } else if !unsafe {
        ((*vulkan).add_intercept_initialization)(intercept, std::ptr::null_mut(), 0)
    } {
        eprintln!("[dv2ruffle] Vulkan init intercept refused");
    }
    if !graphics.is_null() {
        unsafe { ((*graphics).register_device_event_callback)(device_event) };
        unsafe { device_event(DEVICE_EVENT_INITIALIZE) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn UnityPluginUnload() {
    let graphics = GRAPHICS.load(Ordering::SeqCst);
    if !graphics.is_null() {
        unsafe { ((*graphics).unregister_device_event_callback)(device_event) };
    }
}

unsafe extern "C" fn intercept(
    get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    _: *mut c_void,
) -> vk::PFN_vkGetInstanceProcAddr {
    *HOOK.lock().unwrap() = Some(Hook {
        get_instance_proc_addr,
        instance: vk::Instance::null(),
        api_version: vk::API_VERSION_1_0,
        instance_extensions: Vec::new(),
        prepared: None,
    });
    hooked_get_instance_proc_addr
}

unsafe extern "system" fn hooked_get_instance_proc_addr(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    let wanted = unsafe { CStr::from_ptr(name) };
    if wanted == c"vkCreateInstance" {
        return unsafe { std::mem::transmute::<*const (), vk::PFN_vkVoidFunction>(create_instance as *const ()) };
    }
    if wanted == c"vkCreateDevice" {
        return unsafe { std::mem::transmute::<*const (), vk::PFN_vkVoidFunction>(create_device as *const ()) };
    }
    if wanted == c"vkGetInstanceProcAddr" {
        return unsafe {
            std::mem::transmute::<*const (), vk::PFN_vkVoidFunction>(
                hooked_get_instance_proc_addr as *const (),
            )
        };
    }
    let real = HOOK.lock().unwrap().as_ref().unwrap().get_instance_proc_addr;
    unsafe { real(instance, name) }
}

unsafe extern "system" fn create_instance(
    info: *const vk::InstanceCreateInfo,
    allocator: *const vk::AllocationCallbacks,
    out: *mut vk::Instance,
) -> vk::Result {
    let real_get = HOOK.lock().unwrap().as_ref().unwrap().get_instance_proc_addr;
    let entry = entry(real_get);
    let info = unsafe { *info };
    let mut application = if info.p_application_info.is_null() {
        vk::ApplicationInfo::default()
    } else {
        unsafe { *info.p_application_info }
    };
    let loader = unsafe { entry.try_enumerate_instance_version() }
        .ok()
        .flatten()
        .unwrap_or(vk::API_VERSION_1_0);
    let api_version = application
        .api_version
        .max(loader.min(vk::API_VERSION_1_3))
        .max(vk::API_VERSION_1_0);
    application.api_version = api_version;
    let mut extensions =
        unsafe { names(info.enabled_extension_count, info.pp_enabled_extension_names) };
    if let Ok(desired) = hal::vulkan::Instance::desired_extensions(
        &entry,
        api_version,
        wgpu::InstanceFlags::empty(),
    ) {
        for name in desired {
            if !extensions.contains(&name) {
                extensions.push(name);
            }
        }
    }
    let pointers: Vec<*const c_char> = extensions.iter().map(|n| n.as_ptr()).collect();
    let mut merged = info;
    merged.enabled_extension_count = pointers.len() as u32;
    merged.pp_enabled_extension_names = pointers.as_ptr();
    merged.p_application_info = &application;
    let real: vk::PFN_vkCreateInstance = unsafe {
        std::mem::transmute(real_get(vk::Instance::null(), c"vkCreateInstance".as_ptr()))
    };
    let result = unsafe { real(&merged, allocator, out) };
    eprintln!(
        "[dv2ruffle] vkCreateInstance {result:?}, api {}.{}, extensions {extensions:?}",
        vk::api_version_major(api_version),
        vk::api_version_minor(api_version)
    );
    if result == vk::Result::SUCCESS {
        let mut hook = HOOK.lock().unwrap();
        let hook = hook.as_mut().unwrap();
        hook.instance = unsafe { *out };
        hook.api_version = api_version;
        hook.instance_extensions = extensions;
    }
    result
}

unsafe fn chain(mut next: *const c_void) -> Vec<*mut vk::BaseOutStructure<'static>> {
    let mut all = Vec::new();
    while !next.is_null() {
        let item = next as *mut vk::BaseOutStructure<'static>;
        all.push(item);
        next = unsafe { (*item).p_next } as *const c_void;
    }
    all
}

unsafe fn or_words(into: *mut c_void, at: usize, from: *const c_void, count: usize) {
    for i in 0..count {
        unsafe {
            let to = (into as *mut u8).add(at + 4 * i) as *mut vk::Bool32;
            let value = *((from as *const u8).add(size_of::<vk::BaseOutStructure>() + 4 * i)
                as *const vk::Bool32);
            *to |= value;
        }
    }
}

fn words<T>() -> usize {
    (size_of::<T>() - size_of::<vk::BaseOutStructure>()) / 4
}

macro_rules! own {
    ($($t:ty),*) => {
        fn own_size(ty: vk::StructureType) -> Option<usize> {
            $(if ty == <$t>::STRUCTURE_TYPE { return Some(words::<$t>()); })*
            None
        }
    };
}

own!(
    vk::PhysicalDevice16BitStorageFeatures<'static>,
    vk::PhysicalDeviceAccelerationStructureFeaturesKHR<'static>,
    vk::PhysicalDeviceBufferDeviceAddressFeatures<'static>,
    vk::PhysicalDeviceCooperativeMatrixFeaturesKHR<'static>,
    vk::PhysicalDeviceDescriptorIndexingFeatures<'static>,
    vk::PhysicalDeviceFragmentShaderBarycentricFeaturesKHR<'static>,
    vk::PhysicalDeviceImageRobustnessFeatures<'static>,
    vk::PhysicalDeviceMaintenance4Features<'static>,
    vk::PhysicalDeviceMeshShaderFeaturesEXT<'static>,
    vk::PhysicalDeviceMultiviewFeatures<'static>,
    vk::PhysicalDevicePortabilitySubsetFeaturesKHR<'static>,
    vk::PhysicalDeviceRayQueryFeaturesKHR<'static>,
    vk::PhysicalDeviceRayTracingPipelineFeaturesKHR<'static>,
    vk::PhysicalDeviceRayTracingPositionFetchFeaturesKHR<'static>,
    vk::PhysicalDeviceRobustness2FeaturesEXT<'static>,
    vk::PhysicalDeviceSamplerYcbcrConversionFeatures<'static>,
    vk::PhysicalDeviceShaderAtomicFloatFeaturesEXT<'static>,
    vk::PhysicalDeviceShaderAtomicInt64Features<'static>,
    vk::PhysicalDeviceShaderDrawParametersFeatures<'static>,
    vk::PhysicalDeviceShaderFloat16Int8Features<'static>,
    vk::PhysicalDeviceShaderImageAtomicInt64FeaturesEXT<'static>,
    vk::PhysicalDeviceShaderIntegerDotProductFeatures<'static>,
    vk::PhysicalDeviceSubgroupSizeControlFeatures<'static>,
    vk::PhysicalDeviceTextureCompressionASTCHDRFeatures<'static>,
    vk::PhysicalDeviceTimelineSemaphoreFeatures<'static>,
    vk::PhysicalDeviceVulkanMemoryModelFeatures<'static>,
    vk::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures<'static>
);

fn promoted(ty: vk::StructureType) -> Option<(vk::StructureType, usize)> {
    use vk::{
        PhysicalDeviceVulkan11Features as V11, PhysicalDeviceVulkan12Features as V12,
        PhysicalDeviceVulkan13Features as V13,
    };
    let table = [
        (vk::PhysicalDevice16BitStorageFeatures::STRUCTURE_TYPE, V11::STRUCTURE_TYPE, offset_of!(V11, storage_buffer16_bit_access)),
        (vk::PhysicalDeviceMultiviewFeatures::STRUCTURE_TYPE, V11::STRUCTURE_TYPE, offset_of!(V11, multiview)),
        (vk::PhysicalDeviceSamplerYcbcrConversionFeatures::STRUCTURE_TYPE, V11::STRUCTURE_TYPE, offset_of!(V11, sampler_ycbcr_conversion)),
        (vk::PhysicalDeviceShaderDrawParametersFeatures::STRUCTURE_TYPE, V11::STRUCTURE_TYPE, offset_of!(V11, shader_draw_parameters)),
        (vk::PhysicalDeviceShaderAtomicInt64Features::STRUCTURE_TYPE, V12::STRUCTURE_TYPE, offset_of!(V12, shader_buffer_int64_atomics)),
        (vk::PhysicalDeviceShaderFloat16Int8Features::STRUCTURE_TYPE, V12::STRUCTURE_TYPE, offset_of!(V12, shader_float16)),
        (vk::PhysicalDeviceDescriptorIndexingFeatures::STRUCTURE_TYPE, V12::STRUCTURE_TYPE, offset_of!(V12, shader_input_attachment_array_dynamic_indexing)),
        (vk::PhysicalDeviceTimelineSemaphoreFeatures::STRUCTURE_TYPE, V12::STRUCTURE_TYPE, offset_of!(V12, timeline_semaphore)),
        (vk::PhysicalDeviceBufferDeviceAddressFeatures::STRUCTURE_TYPE, V12::STRUCTURE_TYPE, offset_of!(V12, buffer_device_address)),
        (vk::PhysicalDeviceVulkanMemoryModelFeatures::STRUCTURE_TYPE, V12::STRUCTURE_TYPE, offset_of!(V12, vulkan_memory_model)),
        (vk::PhysicalDeviceImageRobustnessFeatures::STRUCTURE_TYPE, V13::STRUCTURE_TYPE, offset_of!(V13, robust_image_access)),
        (vk::PhysicalDeviceSubgroupSizeControlFeatures::STRUCTURE_TYPE, V13::STRUCTURE_TYPE, offset_of!(V13, subgroup_size_control)),
        (vk::PhysicalDeviceTextureCompressionASTCHDRFeatures::STRUCTURE_TYPE, V13::STRUCTURE_TYPE, offset_of!(V13, texture_compression_astc_hdr)),
        (vk::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures::STRUCTURE_TYPE, V13::STRUCTURE_TYPE, offset_of!(V13, shader_zero_initialize_workgroup_memory)),
        (vk::PhysicalDeviceShaderIntegerDotProductFeatures::STRUCTURE_TYPE, V13::STRUCTURE_TYPE, offset_of!(V13, shader_integer_dot_product)),
        (vk::PhysicalDeviceMaintenance4Features::STRUCTURE_TYPE, V13::STRUCTURE_TYPE, offset_of!(V13, maintenance4)),
    ];
    table
        .iter()
        .find(|(own, _, _)| *own == ty)
        .map(|(_, core, at)| (*core, *at))
}

unsafe extern "system" fn create_device(
    physical_device: vk::PhysicalDevice,
    info: *const vk::DeviceCreateInfo,
    allocator: *const vk::AllocationCallbacks,
    out: *mut vk::Device,
) -> vk::Result {
    let mut guard = HOOK.lock().unwrap();
    let hook = guard.as_mut().unwrap();
    let entry = entry(hook.get_instance_proc_addr);
    let raw_instance = unsafe { ash::Instance::load(entry.static_fn(), hook.instance) };
    let real: vk::PFN_vkCreateDevice = unsafe {
        std::mem::transmute((hook.get_instance_proc_addr)(
            hook.instance,
            c"vkCreateDevice".as_ptr(),
        ))
    };
    let instance = match unsafe {
        hal::vulkan::Instance::from_raw(
            entry,
            raw_instance,
            hook.api_version,
            0,
            None,
            hook.instance_extensions.clone(),
            wgpu::InstanceFlags::empty(),
            wgpu::MemoryBudgetThresholds::default(),
            false,
            Some(Box::new(|| {})),
        )
    } {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("[dv2ruffle] wgpu instance from Unity's failed: {error:?}");
            return unsafe { real(physical_device, info, allocator, out) };
        }
    };
    let Some(adapter) = instance.expose_adapter(physical_device) else {
        eprintln!("[dv2ruffle] wgpu cannot use Unity's physical device");
        return unsafe { real(physical_device, info, allocator, out) };
    };
    let (features, limits) =
        device_features_and_limits(adapter.features, adapter.capabilities.limits.clone());
    let info = unsafe { *info };
    let mut extensions =
        unsafe { names(info.enabled_extension_count, info.pp_enabled_extension_names) };
    for name in adapter.adapter.required_device_extensions(features) {
        if !extensions.contains(&name) {
            extensions.push(name);
        }
    }
    let mut wanted = adapter
        .adapter
        .physical_device_features(&extensions, features);
    let wgpu_info = wanted.add_to_device_create(vk::DeviceCreateInfo::default());
    let mut merged = info;
    let pointers: Vec<*const c_char> = extensions.iter().map(|n| n.as_ptr()).collect();
    merged.enabled_extension_count = pointers.len() as u32;
    merged.pp_enabled_extension_names = pointers.as_ptr();

    let unity = unsafe { chain(info.p_next) };
    let find = |ty: vk::StructureType| unity.iter().copied().find(|s| unsafe { (**s).s_type } == ty);
    let core_words = size_of::<vk::PhysicalDeviceFeatures>() / 4;
    let mut core = vk::PhysicalDeviceFeatures::default();
    if !wgpu_info.p_enabled_features.is_null() {
        if let Some(features2) = find(vk::PhysicalDeviceFeatures2::STRUCTURE_TYPE) {
            let at = offset_of!(vk::PhysicalDeviceFeatures2, features);
            let wgpu_core = wgpu_info.p_enabled_features as *const u8;
            for i in 0..core_words {
                unsafe {
                    *((features2 as *mut u8).add(at + 4 * i) as *mut vk::Bool32) |=
                        *(wgpu_core.add(4 * i) as *const vk::Bool32);
                }
            }
        } else {
            if !info.p_enabled_features.is_null() {
                core = unsafe { *info.p_enabled_features };
            }
            let into = &mut core as *mut vk::PhysicalDeviceFeatures as *mut vk::Bool32;
            let from = wgpu_info.p_enabled_features as *const vk::Bool32;
            for i in 0..core_words {
                unsafe { *into.add(i) |= *from.add(i) };
            }
            merged.p_enabled_features = &core;
        }
    }
    for item in unsafe { chain(wgpu_info.p_next) } {
        let ty = unsafe { (*item).s_type };
        let Some(count) = own_size(ty) else {
            eprintln!("[dv2ruffle] feature struct {ty:?} unknown, left out");
            continue;
        };
        if let Some(same) = find(ty) {
            unsafe {
                or_words(
                    same as *mut c_void,
                    size_of::<vk::BaseOutStructure>(),
                    item as *const c_void,
                    count,
                )
            };
            continue;
        }
        if let Some((core_ty, at)) = promoted(ty)
            && let Some(core_struct) = find(core_ty)
        {
            unsafe { or_words(core_struct as *mut c_void, at, item as *const c_void, count) };
            continue;
        }
        unsafe { (*item).p_next = merged.p_next as *mut vk::BaseOutStructure<'static> };
        merged.p_next = item as *const c_void;
    }
    let result = unsafe { real(physical_device, &merged, allocator, out) };
    eprintln!(
        "[dv2ruffle] vkCreateDevice {result:?}, Unity chain {:?}, extensions {extensions:?}, features {features:?}",
        unity.iter().map(|s| unsafe { (**s).s_type }).collect::<Vec<_>>()
    );
    drop(wanted);
    if result == vk::Result::SUCCESS {
        hook.prepared = Some(Prepared {
            instance,
            adapter,
            extensions,
            features,
            limits,
        });
    }
    result
}

unsafe extern "C" fn device_event(event: i32) {
    if event == DEVICE_EVENT_SHUTDOWN {
        return;
    }
    if event != DEVICE_EVENT_INITIALIZE || GPU.get().is_some() {
        return;
    }
    let graphics = GRAPHICS.load(Ordering::SeqCst);
    let vulkan = VULKAN.load(Ordering::SeqCst);
    if graphics.is_null() || vulkan.is_null() {
        return;
    }
    if unsafe { ((*graphics).get_renderer)() } != RENDERER_VULKAN {
        eprintln!("[dv2ruffle] Unity is not running on Vulkan");
        return;
    }
    let unity = unsafe { ((*vulkan).instance)() };
    if unity.device == vk::Device::null() {
        return;
    }
    let Some(prepared) = HOOK.lock().unwrap().as_mut().and_then(|h| h.prepared.take()) else {
        eprintln!("[dv2ruffle] Unity's device was created before the plugin hooked it");
        return;
    };
    let raw_device = unsafe {
        ash::Device::load(
            prepared.instance.shared_instance().raw_instance().fp_v1_0(),
            unity.device,
        )
    };
    let queue_zero = unsafe { raw_device.get_device_queue(unity.queue_family_index, 0) };
    if queue_zero != unity.graphics_queue {
        eprintln!("[dv2ruffle] Unity's graphics queue is not queue 0 of its family");
        return;
    }
    let open = match unsafe {
        prepared.adapter.adapter.device_from_raw(
            raw_device,
            Some(Box::new(|| {})),
            &prepared.extensions,
            prepared.features,
            &prepared.limits,
            &wgpu::MemoryHints::default(),
            unity.queue_family_index,
            0,
        )
    } {
        Ok(open) => open,
        Err(error) => {
            eprintln!("[dv2ruffle] wgpu device from Unity's failed: {error:?}");
            return;
        }
    };
    let instance = unsafe { wgpu::Instance::from_hal::<Vulkan>(prepared.instance) };
    let adapter = unsafe { instance.create_adapter_from_hal(prepared.adapter) };
    let (device, queue) = match unsafe {
        adapter.create_device_from_hal(
            open,
            &wgpu::DeviceDescriptor {
                label: None,
                required_features: prepared.features,
                required_limits: prepared.limits,
                memory_hints: Default::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            },
        )
    } {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("[dv2ruffle] wgpu device failed: {error:?}");
            return;
        }
    };
    let id = unsafe { ((*graphics).reserve_event_id_range)(1) };
    unsafe {
        ((*vulkan).configure_event)(
            id,
            &UnityVulkanPluginEventConfig {
                render_pass_precondition: RENDER_PASS_ENSURE_OUTSIDE,
                graphics_queue_access: QUEUE_ACCESS_ALLOW,
                flags: EVENT_ENSURE_PREVIOUS_FRAME_SUBMISSION
                    | EVENT_FLUSH_COMMAND_BUFFERS
                    | EVENT_MODIFIES_COMMAND_BUFFERS_STATE,
            },
        )
    };
    EVENT.store(id, Ordering::SeqCst);
    let _ = GPU.set(Arc::new(Descriptors::new(instance, adapter, device, queue)));
    eprintln!("[dv2ruffle] wgpu runs on Unity's Vulkan device, render event {id}");
}

pub fn gpu() -> Option<Arc<Descriptors>> {
    GPU.get().cloned()
}

#[derive(Debug)]
pub struct UnityFrame(wgpu::TextureView);

impl RenderTargetFrame for UnityFrame {
    fn into_view(self) -> wgpu::TextureView {
        self.0
    }

    fn view(&self) -> &wgpu::TextureView {
        &self.0
    }
}

#[derive(Debug)]
pub struct UnityTarget;

impl RenderTarget for UnityTarget {
    type Frame = UnityFrame;

    fn resize(&mut self, _: &wgpu::Device, _: u32, _: u32) {}

    fn format(&self) -> wgpu::TextureFormat {
        FRAME
            .lock()
            .unwrap()
            .as_ref()
            .map_or(wgpu::TextureFormat::Rgba8Unorm, |t| t.format())
    }

    fn width(&self) -> u32 {
        FRAME.lock().unwrap().as_ref().map_or(1, |t| t.width())
    }

    fn height(&self) -> u32 {
        FRAME.lock().unwrap().as_ref().map_or(1, |t| t.height())
    }

    fn get_next_texture(&mut self) -> Option<UnityFrame> {
        FRAME
            .lock()
            .unwrap()
            .as_ref()
            .map(|t| UnityFrame(t.create_view(&Default::default())))
    }

    fn submit<I: IntoIterator<Item = wgpu::CommandBuffer>>(
        &self,
        _: &wgpu::Device,
        queue: &wgpu::Queue,
        command_buffers: I,
        _: UnityFrame,
    ) -> wgpu::SubmissionIndex {
        queue.submit(command_buffers)
    }

    fn existing(&self) -> Option<wgpu::Texture> {
        FRAME.lock().unwrap().clone()
    }
}

fn texture_format(format: vk::Format) -> Option<wgpu::TextureFormat> {
    Some(match format {
        vk::Format::R8G8B8A8_UNORM => wgpu::TextureFormat::Rgba8Unorm,
        vk::Format::R8G8B8A8_SRGB => wgpu::TextureFormat::Rgba8UnormSrgb,
        vk::Format::B8G8R8A8_UNORM => wgpu::TextureFormat::Bgra8Unorm,
        vk::Format::B8G8R8A8_SRGB => wgpu::TextureFormat::Bgra8UnormSrgb,
        _ => return None,
    })
}

#[repr(C)]
pub struct Dv2rFrame {
    render_buffer: *mut c_void,
    count: i32,
    movies: *const *mut Movie,
}

unsafe extern "C" fn render(event: i32, data: *mut c_void) {
    let data = data as usize;
    if std::panic::catch_unwind(|| unsafe { render_frame(event, data as *mut c_void) }).is_err() {
        *FRAME.lock().unwrap_or_else(|e| e.into_inner()) = None;
        eprintln!("[dv2ruffle] the movie render panicked; frame skipped");
    }
}

unsafe fn render_frame(event: i32, data: *mut c_void) {
    if event != EVENT.load(Ordering::SeqCst) || data.is_null() {
        return;
    }
    let (Some(descriptors), vulkan) = (GPU.get(), VULKAN.load(Ordering::SeqCst)) else {
        return;
    };
    let frame = unsafe { &*(data as *const Dv2rFrame) };
    let mut image: UnityVulkanImage = unsafe { std::mem::zeroed() };
    if !unsafe {
        ((*vulkan).access_render_buffer_texture)(
            frame.render_buffer,
            std::ptr::null(),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            ACCESS_PIPELINE_BARRIER,
            &mut image,
        )
    } {
        eprintln!("[dv2ruffle] no image for render buffer {:?}", frame.render_buffer);
        return;
    }
    let Some(format) = texture_format(image.format) else {
        eprintln!("[dv2ruffle] colour target format {:?} not handled", image.format);
        return;
    };
    let size = wgpu::Extent3d {
        width: image.extent.width,
        height: image.extent.height,
        depth_or_array_layers: 1,
    };
    let Some(hal_device) = (unsafe { descriptors.device.as_hal::<Vulkan>() }) else {
        return;
    };
    let hal_texture = unsafe {
        hal_device.texture_from_raw(
            image.image,
            &hal::TextureDescriptor {
                label: Some("Unity colour target"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUses::COLOR_TARGET
                    | wgpu::TextureUses::COPY_SRC
                    | wgpu::TextureUses::COPY_DST
                    | wgpu::TextureUses::RESOURCE,
                memory_flags: hal::MemoryFlags::empty(),
                view_formats: Vec::new(),
            },
            Some(Box::new(|| {})),
            hal::vulkan::TextureMemory::External,
        )
    };
    drop(hal_device);
    let texture = unsafe {
        descriptors.device.create_texture_from_hal::<Vulkan>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("Unity colour target"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::TextureUses::COLOR_TARGET,
        )
    };
    *FRAME.lock().unwrap() = Some(texture);
    for i in 0..frame.count as usize {
        let movie = unsafe { *frame.movies.add(i) };
        if !movie.is_null() {
            unsafe { (*movie).render_into_frame(size.width, size.height, format) };
        }
    }
    *FRAME.lock().unwrap() = None;
}

#[unsafe(no_mangle)]
pub extern "C" fn dv2r_render_event() -> unsafe extern "C" fn(i32, *mut c_void) {
    render
}

#[unsafe(no_mangle)]
pub extern "C" fn dv2r_render_event_id() -> i32 {
    EVENT.load(Ordering::SeqCst)
}

#[unsafe(no_mangle)]
pub extern "C" fn dv2r_on_unity_device() -> bool {
    GPU.get().is_some()
}
