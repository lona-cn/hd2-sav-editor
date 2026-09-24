//! UI layer.

pub mod state;
pub mod workspace;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{px, rgb, App};

/// Keep GPUI Kit controls in the same tactical palette as the workspace.
pub fn configure_theme(cx: &mut App) {
    {
        let theme = Theme::global_mut(cx);
        theme.mode = ThemeMode::Dark;
        theme.font_family = "Bahnschrift".into();
        theme.mono_font_family = "Cascadia Mono".into();
        theme.font_size = px(14.0);
        theme.mono_font_size = px(12.0);
        theme.radius = px(3.0);
        theme.radius_lg = px(4.0);
        theme.shadow = true;

        theme.background = rgb(0x08151D).into();
        theme.foreground = rgb(0xF1F3EE).into();
        theme.border = rgb(0x30434C).into();
        theme.input = rgb(0x30434C).into();
        theme.muted = rgb(0x11232D).into();
        theme.muted_foreground = rgb(0xA5B4B9).into();
        theme.popover = rgb(0x10212A).into();
        theme.popover_foreground = rgb(0xF1F3EE).into();
        theme.secondary = rgb(0x1A303A).into();
        theme.secondary_hover = rgb(0x29424C).into();
        theme.secondary_active = rgb(0x10232C).into();
        theme.secondary_foreground = rgb(0xF1F3EE).into();
        theme.selection = rgb(0x4E4818).into();
        theme.scrollbar = rgb(0x08151D).into();
        theme.scrollbar_thumb = rgb(0x34505A).into();
        theme.scrollbar_thumb_hover = rgb(0x51707A).into();

        theme.primary = rgb(0xFFE710).into();
        theme.primary_hover = rgb(0xFFF166).into();
        theme.primary_active = rgb(0xD6C500).into();
        theme.primary_foreground = rgb(0x101A1C).into();
        theme.button_primary = theme.primary;
        theme.button_primary_hover = theme.primary_hover;
        theme.button_primary_active = theme.primary_active;
        theme.button_primary_foreground = theme.primary_foreground;

        theme.button = rgb(0x1A303A).into();
        theme.button_hover = rgb(0x29424C).into();
        theme.button_active = rgb(0x10232C).into();
        theme.button_foreground = rgb(0xF1F3EE).into();
        theme.button_secondary = rgb(0x1A303A).into();
        theme.button_secondary_hover = rgb(0x29424C).into();
        theme.button_secondary_active = rgb(0x10232C).into();
        theme.button_secondary_foreground = rgb(0xF1F3EE).into();

        theme.success = rgb(0x65D49A).into();
        theme.success_foreground = rgb(0x09120D).into();
        theme.success_hover = rgb(0x78E0AB).into();
        theme.success_active = rgb(0x47B97B).into();
        theme.button_success = theme.success;
        theme.button_success_hover = theme.success_hover;
        theme.button_success_active = theme.success_active;
        theme.button_success_foreground = theme.success_foreground;
        theme.warning = rgb(0xFFE710).into();
        theme.warning_foreground = rgb(0x101A1C).into();
        theme.danger = rgb(0xF06B67).into();
        theme.danger_foreground = rgb(0x170809).into();
        theme.danger_hover = rgb(0xFF7C78).into();
        theme.danger_active = rgb(0xD9514E).into();
        theme.button_danger = theme.danger;
        theme.button_danger_hover = theme.danger_hover;
        theme.button_danger_active = theme.danger_active;
        theme.button_danger_foreground = theme.danger_foreground;
        theme.info = rgb(0x74BED0).into();
        theme.info_foreground = rgb(0x08151D).into();
        theme.ring = rgb(0xFFE710).into();
        theme.link = rgb(0xFFE710).into();
        theme.link_hover = rgb(0xFFF166).into();
        theme.link_active = rgb(0xD6C500).into();

        // GPUI Kit components paint backgrounds from resolved tokens while
        // foregrounds still read the legacy fields. Keep both projections in
        // lockstep after applying the application palette.
        theme.tokens = theme.colors.into();
    }
    Theme::sync_base(cx);
}
pub use workspace::WorkspaceView;
