# vendor/

Local patches for third-party crates that can't be applied via a normal
Cargo dependency. Referenced from the root `Cargo.toml` via `[patch.crates-io]`.

## egui-winit-0.33.3

Unmodified `egui-winit` 0.33.3 source, with one addition: on Windows, a
transparent window now also gets `with_no_redirection_bitmap(true)` (see
`src/lib.rs`, search for "LOCAL PATCH").

**Why:** Without this, `eframe`'s wgpu backend renders our transparent overlay
window fully opaque on Windows - any alpha < 1 blends toward solid white
instead of the real desktop behind the window. DWM composites the window
through its legacy GDI redirection-bitmap surface by default, which doesn't
preserve per-pixel alpha from a DXGI swapchain. `winit` already supports
opting out via `WindowAttributesExtWindows::with_no_redirection_bitmap`
(sets `WS_EX_NOREDIRECTIONBITMAP` before window creation); `egui-winit` just
never wired it up for transparent windows.

**Tracking:** [emilk/egui#8116](https://github.com/emilk/egui/pull/8116) is a
draft PR upstream doing this properly (plus additional DirectComposition
work in `egui-wgpu`). Once that lands in a released `eframe`, remove this
directory and the `[patch.crates-io]` entry in the root `Cargo.toml`, and
drop the `wgpu` feature override on `eframe` if it's no longer needed.
