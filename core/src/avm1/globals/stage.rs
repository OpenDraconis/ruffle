//! Stage object
//!
//! TODO: This is a very rough stub with not much implementation.

use crate::avm1::activation::Activation;
use crate::avm1::error::Error;
use crate::avm1::property_decl::{DeclContext, StaticDeclarations};
use crate::avm1::{Object, Value};
use crate::display_object::{StageDisplayState, TDisplayObject};
use crate::display_object::stage::gfx;
use crate::string::{AvmString, WStr, WString};
use ruffle_macros::istr;

const OBJECT_DECLS: StaticDeclarations = declare_static_properties! {
    "align" => property(align, set_align);
    "scaleMode" => property(scale_mode, set_scale_mode);
    "height" => property(height);
    "width" => property(width);
    "showMenu" => property(show_menu, set_show_menu);
    "displayState" => property(display_state, set_display_state);
    "visibleRect" => property(visible_rect);
    "safeRect" => property(visible_rect);
    "originalRect" => property(original_rect);
};

pub fn create<'gc>(context: &mut DeclContext<'_, 'gc>) -> Object<'gc> {
    let stage = Object::new(context.strings, Some(context.object_proto));
    context.define_properties_on(stage, OBJECT_DECLS(context));
    stage
}

// GASGlobalObject::SetMember @f07700, GASStageCtorFunction::GetMember @fb1b50 decomp
fn gfx_extensions<'gc>(activation: &mut Activation<'_, 'gc>) -> Result<bool, Error<'gc>> {
    let name = AvmString::new_utf8(activation.gc(), "gfxExtensions");
    let value = activation.global_object().get(name, activation)?;
    Ok(value.as_bool(activation.swf_version()))
}

// GASStageCtorFunction::CreateRectangleObject @fb1ad0, GASRectangleObject::SetProperties @f854d0 decomp
fn gfx_rect<'gc>(
    activation: &mut Activation<'_, 'gc>,
    [left, top, right, bottom]: [f32; 4],
) -> Result<Value<'gc>, Error<'gc>> {
    let (left, top, right, bottom) = (
        f64::from(left),
        f64::from(top),
        f64::from(right),
        f64::from(bottom),
    );
    activation.instantiate_class_as_script(
        [istr!("flash"), istr!("geom"), istr!("Rectangle")],
        &[
            left.into(),
            top.into(),
            (right - left).into(),
            (bottom - top).into(),
        ],
    )
}

// GASStageCtorFunction::GetMember @fb1b50, GFxMovieRoot::GetVisibleFrameRect @f2e730 decomp
pub fn gfx_visible_rect<'gc>(
    activation: &mut Activation<'_, 'gc>,
) -> Result<Option<Value<'gc>>, Error<'gc>> {
    let Some(view) = activation.context.stage.gfx_view() else {
        return Ok(None);
    };
    if !gfx_extensions(activation)? {
        return Ok(None);
    }
    Ok(Some(gfx_rect(activation, view.rect.map(|t| t / 20.0))?))
}

fn visible_rect<'gc>(
    activation: &mut Activation<'_, 'gc>,
    _this: Object<'gc>,
    _args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    Ok(gfx_visible_rect(activation)?.unwrap_or(Value::Undefined))
}

// GASStageCtorFunction::GetMember @fb1b50, GFxMovieDefImpl::GetFrameRect @ec98d0 decomp
fn original_rect<'gc>(
    activation: &mut Activation<'_, 'gc>,
    _this: Object<'gc>,
    _args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    if activation.context.stage.gfx_view().is_none() || !gfx_extensions(activation)? {
        return Ok(Value::Undefined);
    }
    let frame = *activation.context.stage.movie().stage_size();
    gfx_rect(
        activation,
        [frame.x_min, frame.y_min, frame.x_max, frame.y_max].map(|t| t.get() as f32 / 20.0),
    )
}

fn align<'gc>(
    activation: &mut Activation<'_, 'gc>,
    _this: Object<'gc>,
    _args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    if let Some(view) = activation.context.stage.gfx_view() {
        return Ok(AvmString::new_utf8(activation.gc(), gfx::align_name(view.align)).into());
    }
    let align = activation.context.stage.align();
    let mut s = WString::with_capacity(4, false);
    // Match string values returned by AS.
    // It's possible to have an oxymoronic "LTRB".
    // This acts the same as "TL" (top-left takes priority).
    // This order is different between AVM1 and AVM2!
    use crate::display_object::StageAlign;
    if align.contains(StageAlign::LEFT) {
        s.push_byte(b'L');
    }
    if align.contains(StageAlign::TOP) {
        s.push_byte(b'T');
    }
    if align.contains(StageAlign::RIGHT) {
        s.push_byte(b'R');
    }
    if align.contains(StageAlign::BOTTOM) {
        s.push_byte(b'B');
    }
    let align = AvmString::new(activation.gc(), s);
    Ok(align.into())
}

fn set_align<'gc>(
    activation: &mut Activation<'_, 'gc>,
    _this: Object<'gc>,
    args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    let align = args
        .get(0)
        .unwrap_or(&Value::Undefined)
        .coerce_to_string(activation)?;
    if activation.context.stage.gfx_view().is_some() {
        let align = gfx::parse_align(&align);
        let stage = activation.context.stage;
        stage.set_gfx_align(activation.context, align);
        return Ok(Value::Undefined);
    }
    let align = align.parse().unwrap_or_default();
    activation
        .context
        .stage
        .set_align(activation.context, align);
    Ok(Value::Undefined)
}

fn height<'gc>(
    activation: &mut Activation<'_, 'gc>,
    _this: Object<'gc>,
    _args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    Ok(activation.context.stage.stage_size().1.into())
}

fn scale_mode<'gc>(
    activation: &mut Activation<'_, 'gc>,
    _this: Object<'gc>,
    _args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    if let Some(view) = activation.context.stage.gfx_view() {
        let name = gfx::scale_mode_name(view.scale_mode);
        return Ok(AvmString::new_utf8(activation.gc(), name).into());
    }
    let scale_mode = AvmString::new_utf8(
        activation.gc(),
        activation.context.stage.scale_mode().to_avm_string(),
    );
    Ok(scale_mode.into())
}

fn set_scale_mode<'gc>(
    activation: &mut Activation<'_, 'gc>,
    _this: Object<'gc>,
    args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    let scale_mode = args
        .get(0)
        .unwrap_or(&Value::Undefined)
        .coerce_to_string(activation)?;
    if let Some(view) = activation.context.stage.gfx_view() {
        let mode = gfx::parse_scale_mode(&scale_mode);
        let stage = activation.context.stage;
        stage.set_gfx_scale_mode(activation.context, mode);
        if view.scale_mode != mode && mode == 0 {
            stage.notify_gfx_resize(activation.context);
        }
        return Ok(Value::Undefined);
    }
    let scale_mode = scale_mode.parse().unwrap_or_default();
    activation
        .context
        .stage
        .set_scale_mode(activation.context, scale_mode, true);
    Ok(Value::Undefined)
}

fn display_state<'gc>(
    activation: &mut Activation<'_, 'gc>,
    _this: Object<'gc>,
    _args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    let state = if activation.context.stage.is_fullscreen() {
        istr!("fullScreen")
    } else {
        istr!("normal")
    };

    Ok(state.into())
}

fn set_display_state<'gc>(
    activation: &mut Activation<'_, 'gc>,
    _this: Object<'gc>,
    args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    let display_state = args
        .get(0)
        .unwrap_or(&Value::Undefined)
        .coerce_to_string(activation)?;

    if display_state.eq_ignore_case(WStr::from_units(b"fullscreen")) {
        activation
            .context
            .stage
            .set_display_state(activation.context, StageDisplayState::FullScreen);
    } else if display_state.eq_ignore_case(WStr::from_units(b"normal")) {
        activation
            .context
            .stage
            .set_display_state(activation.context, StageDisplayState::Normal);
    }

    Ok(Value::Undefined)
}

fn show_menu<'gc>(
    activation: &mut Activation<'_, 'gc>,
    _this: Object<'gc>,
    _args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    Ok(activation.context.stage.show_menu().into())
}

fn set_show_menu<'gc>(
    activation: &mut Activation<'_, 'gc>,
    _this: Object<'gc>,
    args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    let show_menu = args
        .get(0)
        .unwrap_or(&true.into())
        .to_owned()
        .as_bool(activation.swf_version());
    activation.context.stage.set_show_menu(show_menu);
    Ok(Value::Undefined)
}

fn width<'gc>(
    activation: &mut Activation<'_, 'gc>,
    _this: Object<'gc>,
    _args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    Ok(activation.context.stage.stage_size().0.into())
}
