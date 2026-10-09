//! Lancat desktop shell: starts the Rust core, exposes IPC commands, forwards core events.

mod commands;
mod dock;

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
        ])
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
            tauri::WebviewWindowBuilder::new(app, "panel", tauri::WebviewUrl::App("index.html".into()))
                .title("Lancat")
                .inner_size(360.0, 520.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Lancat");
}
