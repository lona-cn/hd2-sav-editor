//! UI layer.

pub mod state;
pub mod workspace;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{px, rgb, App};

/// Apply the application-owned dark command-console palette to every GPUI Kit
/// component, including dialogs, inputs, buttons and focus rings.
pub fn configure_theme(cx: &mut App) {
    {
        let theme = Theme::global_mut(cx);
        theme.mode = ThemeMode::Dark;
        theme.font_family = "Bahnschrift".into();
        theme.mono_font_family = "Cascadia Mono".into();
        theme.font_size = px(15.0);
        theme.mono_font_size = px(13.0);
        theme.radius = px(6.0);
        theme.radius_lg = px(10.0);
        theme.shadow = true;

        theme.background = rgb(0x0B0E12).into();
        theme.foreground = rgb(0xEEF2F6).into();
        theme.border = rgb(0x2A333F).into();
        theme.input = rgb(0x2A333F).into();
        theme.muted = rgb(0x171D25).into();
        theme.muted_foreground = rgb(0x929CAA).into();
        theme.popover = rgb(0x151A22).into();
        theme.popover_foreground = rgb(0xEEF2F6).into();
        theme.secondary = rgb(0x202731).into();
        theme.secondary_hover = rgb(0x2A333F).into();
        theme.secondary_active = rgb(0x151A20).into();
        theme.secondary_foreground = rgb(0xE9EDF2).into();
        theme.selection = rgb(0x4A3D16).into();
        theme.scrollbar = rgb(0x0B0E12).into();
        theme.scrollbar_thumb = rgb(0x35404E).into();
        theme.scrollbar_thumb_hover = rgb(0x4A5666).into();

        theme.primary = rgb(0xF4C542).into();
        theme.primary_hover = rgb(0xFFD760).into();
        theme.primary_active = rgb(0xD9AA2A).into();
        theme.primary_foreground = rgb(0x111318).into();
        theme.button_primary = theme.primary;
        theme.button_primary_hover = theme.primary_hover;
        theme.button_primary_active = theme.primary_active;
        theme.button_primary_foreground = theme.primary_foreground;

        theme.button = rgb(0x202731).into();
        theme.button_hover = rgb(0x2A333F).into();
        theme.button_active = rgb(0x151A20).into();
        theme.button_foreground = rgb(0xE9EDF2).into();
        theme.button_secondary = rgb(0x202731).into();
        theme.button_secondary_hover = rgb(0x2A333F).into();
        theme.button_secondary_active = rgb(0x151A20).into();
        theme.button_secondary_foreground = rgb(0xE9EDF2).into();

        theme.success = rgb(0x65D49A).into();
        theme.success_foreground = rgb(0x09120D).into();
        theme.success_hover = rgb(0x78E0AB).into();
        theme.success_active = rgb(0x47B97B).into();
        theme.button_success = theme.success;
        theme.button_success_hover = theme.success_hover;
        theme.button_success_active = theme.success_active;
        theme.button_success_foreground = theme.success_foreground;
        theme.warning = rgb(0xF4C542).into();
        theme.warning_foreground = rgb(0x111318).into();
        theme.danger = rgb(0xF06B67).into();
        theme.danger_foreground = rgb(0x170809).into();
        theme.danger_hover = rgb(0xFF7C78).into();
        theme.danger_active = rgb(0xD9514E).into();
        theme.button_danger = theme.danger;
        theme.button_danger_hover = theme.danger_hover;
        theme.button_danger_active = theme.danger_active;
        theme.button_danger_foreground = theme.danger_foreground;
        theme.info = rgb(0x72A7FF).into();
        theme.info_foreground = rgb(0x08101D).into();
        theme.ring = rgb(0xF4C542).into();
        theme.link = rgb(0xF4C542).into();
        theme.link_hover = rgb(0xFFD760).into();
        theme.link_active = rgb(0xD9AA2A).into();

        // GPUI Kit components paint backgrounds from resolved tokens while
        // foregrounds still read the legacy fields. Keep both projections in
        // lockstep after applying the application palette.
        theme.tokens = theme.colors.into();
    }
    Theme::sync_base(cx);
}
pub use workspace::WorkspaceView;
