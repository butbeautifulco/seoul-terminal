use gpui::{Pixels, Window, px};

#[cfg(all(target_os = "macos", macos_sdk_26))]
pub const TRAFFIC_LIGHT_PADDING: f32 = 78.0;

#[cfg(all(target_os = "macos", not(macos_sdk_26)))]
pub const TRAFFIC_LIGHT_PADDING: f32 = 71.0;

#[cfg(not(target_os = "macos"))]
pub const TRAFFIC_LIGHT_PADDING: f32 = 0.0;

pub const MAX_BRANCH_NAME_LENGTH: usize = 40;

#[cfg(not(target_os = "windows"))]
pub fn platform_title_bar_height(window: &Window) -> Pixels {
    (1.75 * window.rem_size()).max(px(34.0))
}

#[cfg(target_os = "windows")]
pub fn platform_title_bar_height(_window: &Window) -> Pixels {
    px(32.0)
}
