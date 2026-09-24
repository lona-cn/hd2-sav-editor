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
    let mut initial_path = None;
    let mut health_marker = None;
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        if argument.as_os_str() == std::ffi::OsStr::new("--hd2-update-health") {
            health_marker = arguments.next().map(std::path::PathBuf::from);
        } else if initial_path.is_none() {
            initial_path = Some(std::path::PathBuf::from(argument));
        }
    }

    let install_dir = match std::env::current_exe()
        .map_err(|error| error.to_string())
        .and_then(|executable| {
            executable
                .parent()
                .ok_or_else(|| "当前程序路径没有父目录".to_string())?
                .canonicalize()
                .map_err(|error| error.to_string())
        }) {
        Ok(path) => path,
        Err(error) => {
            hd2_updater::show_update_failure(&format!("无法定位程序目录：{error}"));
            return;
        }
    };

    let install_lock = if health_marker.is_none() {
        match hd2_updater::acquire_install_lock(&install_dir) {
            Ok(lock) => Some(lock),
            Err(error) => {
                hd2_updater::show_update_failure(&format!("无法锁定程序目录：{error}"));
                return;
            }
        }
    } else {
        None
    };
    let _instance_guard = match SingleInstanceGuard::acquire() {
        Ok(guard) => guard,
        Err(error) => {
            notify_startup_blocked(&error);
            return;
        }
    };

    if install_lock.is_some() {
        let preparation = (|| {
            hd2_updater::recover_incomplete_updates(&install_dir)
                .map_err(|error| format!("无法恢复未完成的更新：{error}"))?;
            hd2_updater::cleanup_update_staging(&install_dir)
                .map_err(|error| format!("无法清理更新暂存文件：{error}"))
        })();
        if let Err(error) = preparation {
            hd2_updater::show_update_failure(&error);
            return;
        }
    }
    drop(install_lock);

    gpui_kit::application().run(move |cx: &mut App| {
        gpui_kit::init(cx);
        configure_theme(cx);
        let health_marker = health_marker.clone();
        let install_dir = install_dir.clone();
        let initial_path = initial_path.clone();
        cx.spawn(async move |cx| {
            // Explicit origin: no display query needed before the window exists.
            let bounds = Bounds {
                origin: point(px(32.0), px(24.0)),
                size: size(px(1200.0), px(680.0)),
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
                    let root = cx.new(|cx| Root::new(view, window, cx));
                    if let Some(marker) = &health_marker {
                        if let Err(error) = hd2_updater::write_update_health_marker(
                            &install_dir,
                            marker,
                            hd2_armor_desk::update::CURRENT_RELEASE_TAG,
                        ) {
                            hd2_updater::show_update_failure(&format!(
                                "更新后的程序健康检查失败：{error}"
                            ));
                            std::process::exit(1);
                        }
                    }
                    root
                },
            )
            .expect("无法创建窗口");
        })
        .detach();
    });
}
