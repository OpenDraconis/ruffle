use crate::prelude::*;
use crate::string::WStr;

// GFxMovieView::ScaleModeType SM_FitVertical, GFxMovieRoot::GFxMovieRoot @f2e1a0 decomp
pub const FIT_VERTICAL: u8 = 4;
pub const SHOW_ALL: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GfxView {
    pub scale_mode: u8,
    pub align: u8,
    pub width: i32,
    pub height: i32,
    pub aspect: f32,
    pub rect: [f32; 4],
}

impl Default for GfxView {
    // GFxMovieRoot::GFxMovieRoot @f2e1a0 decomp
    fn default() -> Self {
        Self {
            scale_mode: SHOW_ALL,
            align: 0,
            width: 1,
            height: 1,
            aspect: 1.0,
            rect: [0.0; 4],
        }
    }
}

fn to_pixels(twips: f32) -> f32 {
    twips / 20.0
}

// GFxMovieRoot::UpdateViewport @f21bf0 decomp
pub fn visible_rect(view: &GfxView, frame_width: f32, frame_height: f32) -> [f32; 4] {
    let viewport_width = view.width as f32 * 20.0;
    let viewport_height = view.height as f32 * 20.0;
    let aspect = view.aspect;
    let centered =
        |frame: f32, size: f32| ((to_pixels(frame * 0.5 - 0.5 * size) as i32) * 20) as f32;
    match view.scale_mode {
        0 => {
            let width = aspect * viewport_width;
            let height = viewport_height;
            let (x, y) = match view.align {
                0 => (
                    centered(frame_width, width),
                    centered(frame_height, height),
                ),
                1 => (centered(frame_width, width), 0.0),
                2 => (centered(frame_width, width), frame_height - height),
                3 => (0.0, centered(frame_height, height)),
                4 => (frame_width - width, centered(frame_height, height)),
                5 => (0.0, 0.0),
                6 => (frame_width - width, 0.0),
                7 => (0.0, frame_height - height),
                _ => (frame_width - width, frame_height - height),
            };
            [x, y, width + x, y + height]
        }
        1 | 3 => {
            let width = aspect * viewport_width;
            let shown_wider = if view.scale_mode == 1 {
                width / frame_width < viewport_height / frame_height
            } else {
                viewport_height / frame_height < width / frame_width
            };
            if shown_wider {
                let height = (frame_width * viewport_height) / width;
                let y = frame_height * 0.5 - height * 0.5;
                [0.0, y, frame_width, height + y]
            } else {
                let width = (width * frame_height) / viewport_height;
                let x = frame_width * 0.5 - width * 0.5;
                [x, 0.0, x + width, frame_height]
            }
        }
        2 => [0.0, 0.0, frame_width, frame_height],
        _ => {
            let width = (frame_height * aspect * viewport_width) / viewport_height;
            let x = frame_width * 0.5 - width * 0.5;
            [x, 0.0, x + width, frame_height]
        }
    }
}

// GFxMovieRoot::UpdateViewport @f21bf0 decomp
pub fn view_matrix(view: &GfxView) -> Matrix {
    let [x0, y0, x1, y1] = view.rect;
    let scale_x = view.width as f32 / (x1 - x0);
    let scale_y = view.height as f32 / (y1 - y0);
    Matrix {
        a: scale_x * 20.0,
        b: 0.0,
        c: 0.0,
        d: scale_y * 20.0,
        tx: Twips::from_pixels(f64::from(-x0 * scale_x)),
        ty: Twips::from_pixels(f64::from(-y0 * scale_y)),
    }
}

// GFxMovieRoot::SetViewport @f2a020 decomp
pub fn resized(old: &GfxView, new: &GfxView) -> bool {
    old.rect != new.rect
        || (new.scale_mode != 2
            && (old.width != new.width || old.height != new.height || old.aspect != new.aspect))
}

// GASStageCtorFunction::SetMember @fb1ee0 decomp
pub fn parse_scale_mode(mode: &WStr) -> u8 {
    if mode.eq_ignore_case(WStr::from_units(b"noScale")) {
        0
    } else if mode.eq_ignore_case(WStr::from_units(b"exactFit")) {
        2
    } else if mode.eq_ignore_case(WStr::from_units(b"noBorder")) {
        3
    } else {
        SHOW_ALL
    }
}

// GASStageCtorFunction::GetMemberRaw @fb1820 decomp
pub fn scale_mode_name(mode: u8) -> &'static str {
    match mode {
        0 => "noScale",
        2 => "exactFit",
        3 => "noBorder",
        _ => "showAll",
    }
}

// GASStageCtorFunction::SetMember @fb1ee0 decomp
pub fn parse_align(align: &WStr) -> u8 {
    let upper = |c: u16| if (0x61..=0x7a).contains(&c) { c - 0x20 } else { c };
    let first = align.get(0).map_or(0, upper);
    let second = align.get(1).map_or(0, upper);
    match (first, second) {
        (0x54, 0x4c) | (0x4c, 0x54) => 5,
        (0x54, 0x52) | (0x52, 0x54) => 6,
        (0x54, _) => 1,
        (0x42, 0x4c) | (0x4c, 0x42) => 7,
        (0x42, 0x52) | (0x52, 0x42) => 8,
        (0x42, _) => 2,
        (0x4c, _) => 3,
        (0x52, _) => 4,
        _ => 0,
    }
}

// GASStageCtorFunction::GetMemberRaw @fb1820 decomp
pub fn align_name(align: u8) -> &'static str {
    match align {
        1 => "T",
        2 => "B",
        3 => "L",
        4 => "R",
        5 => "LT",
        6 => "TR",
        7 => "LB",
        8 => "RB",
        _ => "",
    }
}
