use std::ffi::{CStr, c_char};
use std::sync::{Arc, Mutex, Weak};
use std::sync::atomic::{AtomicUsize, Ordering};

use ruffle_core::Player;
use ruffle_core::font::FontTemplate;
use ruffle_render_wgpu::descriptors::Descriptors;

static MOVIES: Mutex<Vec<(String, Vec<u8>)>> = Mutex::new(Vec::new());
static PARSES: AtomicUsize = AtomicUsize::new(0);
static MAPPINGS: Mutex<Vec<(String, String, u8)>> = Mutex::new(Vec::new());

// CGFxPlayStreamManager::AddFontsFromMovie @1080560 decomp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_add_fonts_from_movie(path: *const c_char) -> bool {
    let path = unsafe { CStr::from_ptr(path) }.to_string_lossy().into_owned();
    let mut movies = MOVIES.lock().unwrap();
    if movies.iter().any(|(added, _)| *added == path) {
        return true;
    }
    let Some(data) = crate::images::read_path(&path) else {
        eprintln!("[dv2ruffle] no font movie {path}");
        return false;
    };
    movies.push((path, data));
    true
}

// CGFxPlayStreamManager::AddFontMapping @10804f0 decomp
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dv2r_add_font_mapping(
    font_id: *const c_char,
    font_name: *const c_char,
    flags: u8,
) {
    let text = |p| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    let entry = (text(font_id), text(font_name), flags);
    let mut mappings = MAPPINGS.lock().unwrap();
    if !mappings.contains(&entry) {
        mappings.push(entry);
    }
}

pub fn parses() -> usize {
    PARSES.load(Ordering::Relaxed)
}

type Templates = Vec<Arc<FontTemplate>>;

static LIBRARIES: Mutex<Vec<(Weak<Descriptors>, Vec<(String, Templates)>)>> = Mutex::new(Vec::new());

fn parse(data: &[u8]) -> Result<Templates, swf::error::Error> {
    PARSES.fetch_add(1, Ordering::Relaxed);
    let buffer = swf::decompress_swf(data)?;
    let parsed = swf::parse_swf(&buffer)?;
    let encoding = swf::SwfStr::encoding_for_version(parsed.header.version());
    Ok(parsed
        .tags
        .into_iter()
        .filter_map(|tag| match tag {
            swf::Tag::DefineFont2(font) => Some(Arc::new(FontTemplate::from_swf_tag(*font, encoding))),
            _ => None,
        })
        .collect())
}

// CGFxPlayStreamManager::CGFxPlayStreamManager @1081e60 decomp
fn library(descriptors: &Arc<Descriptors>) -> Templates {
    let mut libraries = LIBRARIES.lock().unwrap();
    libraries.retain(|(owner, _)| owner.strong_count() > 0);
    let index = match libraries.iter().position(|(owner, _)| owner.as_ptr() == Arc::as_ptr(descriptors)) {
        Some(index) => index,
        None => {
            libraries.push((Arc::downgrade(descriptors), Vec::new()));
            libraries.len() - 1
        }
    };
    let parsed = &mut libraries[index].1;
    for (path, data) in MOVIES.lock().unwrap().iter() {
        if parsed.iter().any(|(done, _)| done == path) {
            continue;
        }
        match parse(data) {
            Ok(templates) => parsed.push((path.clone(), templates)),
            Err(error) => eprintln!("[dv2ruffle] font movie unreadable: {error}"),
        }
    }
    parsed.iter().flat_map(|(_, templates)| templates.iter().cloned()).collect()
}

// CGFxPlayStreamManager::CGFxPlayStreamManager @1081e60 decomp
pub fn install(player: &mut Player, descriptors: &Arc<Descriptors>) {
    player.mutate_with_update_context(|context| context.library.add_font_lib_fonts(std::iter::empty()));
    player.add_font_templates(&library(descriptors));
    for (font_id, font_name, flags) in MAPPINGS.lock().unwrap().iter() {
        player.add_font_mapping(font_id, font_name, *flags);
    }
}
