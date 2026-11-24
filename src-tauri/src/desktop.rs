//! Native utility integration. No hardware ownership or control is implied.
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Emitter, Manager, Window, WindowEvent,
};

const TRAY_ID: &str = "macs-fan-tray";

/// Once the frontend listener is ready, every normal exit round-trips through it.
/// This avoids relying on a dirty-state IPC that may still be in flight.
#[derive(Default)]
pub struct EditorState {
    ready: AtomicBool,
    one_shot_approval: AtomicBool,
}
impl EditorState {
    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
    pub fn approve_exit(&self) {
        self.one_shot_approval.store(true, Ordering::SeqCst);
    }
    pub fn should_prevent_exit(&self) -> bool {
        if self.one_shot_approval.swap(false, Ordering::SeqCst) {
            false
        } else {
            self.is_ready()
        }
    }
}

pub fn should_prevent_exit(app: &AppHandle) -> bool {
    app.state::<EditorState>().should_prevent_exit()
}

/// The main webview retains the draft and owns the custom discard confirmation.
pub fn request_editor_exit(app: &AppHandle) {
    show(app);
    if let Err(error) = app.emit_to("main", "editor-exit-requested", ()) {
        // Keep the process and draft alive even if the listener is unavailable.
        eprintln!("发送草稿退出确认失败：{error}");
    }
}

pub fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if let Err(error) = window
            .unminimize()
            .and_then(|_| window.show())
            .and_then(|_| window.set_focus())
        {
            eprintln!("显示窗口失败：{error}");
        }
    }
}

fn hide(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if let Err(error) = window.hide() {
            eprintln!("隐藏窗口失败：{error}");
        }
    }
}

/// Install the application menu before the frontend can complete its ready handshake.
#[cfg(target_os = "macos")]
pub fn install_application_menu(app: &App) -> tauri::Result<()> {
    use tauri::menu::{AboutMetadata, MenuItemKind, Submenu};

    const QUIT_MAIN: &str = "quit-main";
    let menu = Menu::default(app.handle())?;
    // Tauri 2.12.5 menu/menu.rs puts the app submenu first on macOS.
    // Do not locate it by localized text; retain all other default submenus.
    if !matches!(menu.items()?.first(), Some(MenuItemKind::Submenu(_))) {
        return Err(std::io::Error::other("无法安装受保护的应用退出菜单").into());
    }
    let package = app.package_info();
    let config = app.config();
    let about = AboutMetadata {
        name: Some(package.name.clone()),
        version: Some(package.version.to_string()),
        copyright: config.bundle.copyright.clone(),
        authors: config
            .bundle
            .publisher
            .clone()
            .map(|publisher| vec![publisher]),
        ..Default::default()
    };
    // Native smoke proved the default predefined Quit bypasses ExitRequested:
    // muda 0.20.0 macOS maps it directly to NSApp's terminate: selector.
    // A normal MenuItem accelerator dispatches our event, so app.exit(0)
    // reaches the ready/one-shot ExitRequested guard instead.
    let quit = MenuItem::with_id(app, QUIT_MAIN, "退出 Macs Fan", true, Some("CmdOrCtrl+Q"))?;
    let application = Submenu::with_id_and_items(
        app,
        "macs-fan-application-menu",
        package.name.clone(),
        true,
        &[
            &PredefinedMenuItem::about(app, None, Some(about))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    menu.remove_at(0)?
        .ok_or_else(|| std::io::Error::other("应用菜单缺失"))?;
    menu.prepend(&application)?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        if event.id.as_ref() == QUIT_MAIN {
            app.exit(0);
        }
    });
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn install_application_menu(_app: &App) -> tauri::Result<()> {
    Ok(())
}

pub fn install(app: &App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(
        app,
        "show-main",
        "显示窗口",
        true,
        Some("CmdOrCtrl+Shift+F"),
    )?;
    let hide_item = MenuItem::with_id(app, "hide-main", "隐藏窗口", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "quit-app", "退出 Macs Fan", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &hide_item, &separator, &quit_item])?;
    let icon = Image::new_owned(
        include_bytes!("../icons/tray-template.rgba").to_vec(),
        36,
        36,
    );
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Macs Fan")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show-main" => show(app),
            "hide-main" => hide(app),
            "quit-app" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                let app = tray.app_handle();
                let visible = app
                    .get_webview_window("main")
                    .and_then(|w| w.is_visible().ok())
                    .unwrap_or(false);
                if visible {
                    hide(app);
                } else {
                    show(app);
                }
            }
        })
        .build(app)?;
    Ok(())
}

pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if window.label() == "main" {
        if let WindowEvent::CloseRequested { api, .. } = event {
            // Never strand a hidden app if the native tray could not be created.
            if window.app_handle().tray_by_id(TRAY_ID).is_some() && window.hide().is_ok() {
                api.prevent_close();
            } else if window.app_handle().state::<EditorState>().is_ready() {
                // A real close must round-trip through the frontend once it is ready.
                // Do not consume the one-shot approval here; only ExitRequested may.
                api.prevent_close();
                request_editor_exit(window.app_handle());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EditorState;

    #[test]
    fn ready_guard_covers_quit_before_a_dirty_message_arrives() {
        let state = EditorState::default();
        // Before the listener handshake, editing is disabled by the frontend.
        assert!(!state.is_ready());
        assert!(!state.should_prevent_exit());
        state.mark_ready();
        // The user may edit now. No later dirty-state message is needed for safety.
        assert!(state.should_prevent_exit());
        assert!(state.should_prevent_exit());
        state.mark_ready();
        assert!(state.should_prevent_exit());
    }

    #[test]
    fn explicit_approval_is_consumed_only_once_by_exit_requested() {
        let state = EditorState::default();
        state.mark_ready();
        state.approve_exit();
        // Real-close fallback checks readiness without taking approval.
        assert!(state.is_ready());
        assert!(state.is_ready());
        assert!(!state.should_prevent_exit());
        assert!(state.is_ready());
        assert!(state.should_prevent_exit());
        state.approve_exit();
        assert!(!state.should_prevent_exit());
        assert!(state.should_prevent_exit());
    }
}
