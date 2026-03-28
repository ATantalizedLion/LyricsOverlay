/// Transparent, without decoration and resizable does not work nicely. So here's custom resizing!
use egui::{Context, CursorIcon, ResizeDirection, ViewportCommand};

pub fn handle_resize(ctx: &Context, clickable_margin: f32) {
    let rect = ctx.viewport_rect();

    let pointer = ctx.input(|i| i.pointer.hover_pos());
    let Some(pos) = pointer else { return };

    let on_left = pos.x - rect.min.x < clickable_margin;
    let on_right = rect.max.x - pos.x < clickable_margin;
    let on_top = pos.y - rect.min.y < clickable_margin;
    let on_bottom = rect.max.y - pos.y < clickable_margin;

    let direction = match (on_top, on_bottom, on_left, on_right) {
        (true, false, true, false) => Some(ResizeDirection::NorthWest),
        (true, false, false, true) => Some(ResizeDirection::NorthEast),
        (false, true, true, false) => Some(ResizeDirection::SouthWest),
        (false, true, false, true) => Some(ResizeDirection::SouthEast),
        (true, false, false, false) => Some(ResizeDirection::North),
        (false, true, false, false) => Some(ResizeDirection::South),
        (false, false, true, false) => Some(ResizeDirection::West),
        (false, false, false, true) => Some(ResizeDirection::East),
        _ => None,
    };

    if let Some(dir) = direction {
        let icon = match dir {
            ResizeDirection::North | ResizeDirection::South => CursorIcon::ResizeVertical,
            ResizeDirection::East | ResizeDirection::West => CursorIcon::ResizeHorizontal,
            ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::ResizeNwSe,
            ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::ResizeNeSw,
        };
        ctx.set_cursor_icon(icon);

        let pressed = ctx.input(|i| i.pointer.primary_pressed());
        if pressed {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        }
    }
}
