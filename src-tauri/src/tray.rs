//! System tray: open/pin the panel, receive folder, settings, autostart, quit.
//! Left click toggles the panel (Windows/macOS; Linux trays only show the menu).

use std::sync::atomic::Ordering;
use std::sync::Mutex;

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_autostart::ManagerExt;

use crate::commands::Core;
use crate::dock::{self, DockState};

pub struct TrayState {
    pin: CheckMenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
    icon: TrayIcon<Wry>,
    last_unread: Mutex<i64>,
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "打开 Lancat", true, None::<&str>)?;
    let pin = CheckMenuItem::with_id(app, "pin", "钉住面板（不自动收起）", true, false, None::<&str>)?;
    let recv = MenuItem::with_id(app, "recv", "打开接收文件夹", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?;
    let enabled = app.autolaunch().is_enabled().unwrap_or(false);
    let autostart = CheckMenuItem::with_id(app, "autostart", "开机自动启动", true, enabled, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Lancat", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &pin,
            &PredefinedMenuItem::separator(app)?,
            &recv,
            &settings,
            &autostart,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id("lancat")
        .tooltip("Lancat")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| match ev.id().as_ref() {
            "open" => dock::open_panel(app, "tray"),
            "pin" => {
                let st = app.state::<DockState>();
                let v = !st.pinned.load(Ordering::SeqCst);
                dock::set_pinned(app.clone(), v);
                if v {
                    dock::open_panel(app, "tray");
                }
            }
            "recv" => {
                let dir = app.state::<Core>().0.receive_dir();
                let _ = std::fs::create_dir_all(&dir);
                // a directory, never a received file: opening it cannot execute anything
                let _ = tauri_plugin_opener::open_path(dir.to_string_lossy().to_string(), None::<&str>);
            }
            "settings" => {
                let _ = dock::open_settings(app);
            }
            "autostart" => {
                let al = app.autolaunch();
                let on = al.is_enabled().unwrap_or(false);
                let _ = if on { al.disable() } else { al.enable() };
                sync(app);
            }
            "quit" => {
                app.state::<Core>().0.shutdown();
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                dock::toggle_panel(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let icon = builder.build(app)?;
    app.manage(TrayState { pin, autostart, icon, last_unread: Mutex::new(-1) });
    Ok(())
}

/// Re-syncs check marks with the real state.
pub fn sync(app: &AppHandle) {
    let Some(t) = app.try_state::<TrayState>() else { return };
    let _ = t.pin.set_checked(app.state::<DockState>().pinned.load(Ordering::SeqCst));
    let _ = t.autostart.set_checked(app.autolaunch().is_enabled().unwrap_or(false));
}

/// Tooltip shows the unread count.
pub fn update_unread(app: &AppHandle) {
    let Some(t) = app.try_state::<TrayState>() else { return };
    let n: i64 = app.state::<Core>().0.peers().iter().filter(|p| !p.blocked).map(|p| p.unread).sum();
    let mut last = t.last_unread.lock().unwrap();
    if *last == n {
        return;
    }
    *last = n;
    let tip = if n > 0 { format!("Lancat — {n} 条未读") } else { "Lancat".to_string() };
    let _ = t.icon.set_tooltip(Some(tip));
}
