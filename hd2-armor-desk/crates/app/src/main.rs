#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

//! HD2 Armor Desk — Windows 双甲配置器入口。
//!
//! 窗口结构：`Root` 包裹 `WorkspaceView`，由 `Root` 负责对话框、通知等覆盖层。

use gpui_kit::component::Root;
use gpui_kit::{point, px, size, App, AppContext as _, Bounds, WindowBounds, WindowOptions};
use hd2_armor_desk::single_instance::{notify_startup_blocked, SingleInstanceGuard};
use hd2_armor_desk::ui::{configure_theme, WorkspaceView};

fn main() {
    let _instance_guard = match SingleInstanceGuard::acquire() {
        Ok(guard) => guard,
        Err(error) => {
            notify_startup_blocked(&error);
            return;
        }
    };

    // Optional: a save path on the command line (file association, drag-and-drop
    // onto the exe, or a smoke test). Everything else is unchanged.
    let initial_path = std::env::args_os().nth(1).map(std::path::PathBuf::from);

    gpui_kit::application().run(|cx: &mut App| {
        gpui_kit::init(cx);
        configure_theme(cx);
        cx.spawn(async move |cx| {
            // Explicit origin: no display query needed before the window exists.
            let bounds = Bounds {
                origin: point(px(120.0), px(80.0)),
                size: size(px(1360.0), px(860.0)),
            };
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui_kit::TitlebarOptions {
                        title: Some("Armor Desk · HELLDIVERS 2 双甲配置器".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                move |window, cx| {
                    let view = cx.new(|cx| WorkspaceView::new(window, cx));
                    if let Some(path) = initial_path.clone() {
                        view.update(cx, |view, cx| view.open_initial_path(path, cx));
                    }
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .expect("无法创建窗口");
        })
        .detach();
    });
}
