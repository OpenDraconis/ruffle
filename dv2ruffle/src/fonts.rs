use std::ffi::{CStr, c_char};
use std::sync::Mutex;

use ruffle_core::Player;

static MOVIES: Mutex<Vec<(String, Vec<u8>)>> = Mutex::new(Vec::new());
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

// CGFxPlayStreamManager::CGFxPlayStreamManager @1081e60 decomp
pub fn install(player: &mut Player) {
    player.mutate_with_update_context(|context| context.library.add_font_lib_fonts(std::iter::empty()));
    for (_, data) in MOVIES.lock().unwrap().iter() {
        if let Err(error) = player.add_fonts_from_movie(data) {
            eprintln!("[dv2ruffle] font movie unreadable: {error}");
        }
    }
    for (font_id, font_name, flags) in MAPPINGS.lock().unwrap().iter() {
        player.add_font_mapping(font_id, font_name, *flags);
    }
}
