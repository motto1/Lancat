//! Lancat desktop shell: starts the Rust core, exposes IPC commands, forwards core events.

mod commands;
mod dock;
mod platform;
mod tray;

use std::sync::Arc;

use lancat_core::{Node, NodeConfig};
use tauri::{Emitter, Manager};

use commands::Core;

pub const EVENT: &str = "lancat://event";

fn start_core(app: &tauri::App) -> anyhow::Result<Arc<Node>> {
    let data_dir = app.path().app_data_dir()?;
    let cfg = NodeConfig::new(data_dir).with_env();
    let node = tauri::async_runtime::block_on(async {
        match Node::start(cfg.clone()).await {
            Ok(n) => Ok(n),
            Err(e) => {
                // 47820 taken (e.g. a CLI peer): fall back to a random port; discovery still advertises it.
                log::warn!("{e:#}; falling back to a random port");
                let mut c = cfg;
                c.port = 0;
                Node::start(c).await
            }
        }
    })?;
    Ok(node)
}

pub fn run() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,tao=warn,wry=warn")).try_init();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_me,
            commands::list_peers,
            commands::get_messages,
            commands::send_text,
            commands::send_emoji,
            commands::send_files,
            commands::send_typing,
            commands::mark_read,
            commands::retry_transfer,
            commands::clear_history,
            commands::delete_message,
            commands::get_settings,
            commands::set_settings,
            commands::set_blocked,
            commands::forget_peer,
            commands::add_peer,
            commands::receive_dir,
            commands::is_transferring,
            commands::reveal_file,
            commands::list_stickers,
            commands::import_sticker,
            commands::get_sticker,
            commands::delete_sticker,
            dock::cmd_open_panel,
            dock::cmd_hide_panel,
            dock::set_pinned,
            dock::set_hold,
            dock::cmd_open_settings,
            dock::list_monitors,
        ])
        .on_window_event(|window, event| {
            // the panel/dock are never destroyed; closing settings just closes it
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "panel" {
                    api.prevent_close();
                    dock::hide_panel(window.app_handle());
                }
            }
        })
        .setup(|app| {
            let node = start_core(app).map_err(|e| e.to_string())?;
            // forward core events to every webview
            let handle = app.handle().clone();
            let mut rx = node.subscribe();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            let _ = handle.emit(EVENT, &ev);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });
            app.manage(Core(node));
            app.manage(dock::DockState::default());
            platform::init_app(app);
            dock::create_windows(app.handle())?;
            platform::first_run(app.handle());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Lancat");
}
