//! Android shell: the same core and IPC commands as the desktop, one full-screen window, no dock.
//! The Kotlin side (gen/android: MainActivity, LancatService, LancatPlugin) keeps the process
//! alive in a foreground service, holds the Wi-Fi multicast lock discovery needs, and picks files
//! (content URIs are copied into the cache, the core sends plain paths).

use std::sync::Arc;

use lancat_core::{Node, NodeConfig};
use serde::Deserialize;
use tauri::plugin::{Builder as PluginBuilder, PluginHandle, TauriPlugin};
use tauri::{Emitter, Manager, Runtime};

use crate::commands::{self, Core};
use crate::EVENT;

pub struct Native<R: Runtime>(pub PluginHandle<R>);

fn native_plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("lancat")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            {
                let h = api.register_android_plugin("app.lancat.mobile", "LancatPlugin")?;
                app.manage(Native(h));
            }
            #[cfg(not(target_os = "android"))]
            let _ = (app, api);
            Ok(())
        })
        .build()
}

#[derive(Deserialize)]
struct Picked {
    #[serde(default)]
    paths: Vec<String>,
    /// Shared files still being copied.
    #[serde(default)]
    busy: bool,
}

/// Opens the system file picker; the chosen files come back as paths in our cache.
#[tauri::command]
async fn pick_files(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    let h = app.try_state::<Native<tauri::Wry>>().ok_or("no native plugin")?.0.clone();
    let r = tauri::async_runtime::spawn_blocking(move || h.run_mobile_plugin::<Picked>("pickFiles", ())).await.map_err(|e| e.to_string())?;
    r.map(|p| p.paths).map_err(|e| e.to_string())
}

/// Files shared to Lancat from another app (share sheet) since the last call, as cache paths.
#[tauri::command]
async fn take_shared(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    let h = app.try_state::<Native<tauri::Wry>>().ok_or("no native plugin")?.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // a big file shared a moment ago may still be copying: wait for it (bounded)
        let mut all = vec![];
        for _ in 0..600 {
            let p = h.run_mobile_plugin::<Picked>("takeShared", ()).map_err(|e| e.to_string())?;
            all.extend(p.paths);
            if !p.busy {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        Ok(all)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Where received files go: Download/Lancat when it can be written, else the app's own folder.
fn receive_dir(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    let public = std::path::PathBuf::from("/storage/emulated/0/Download/Lancat");
    if std::fs::create_dir_all(&public).is_ok() && std::fs::write(public.join(".lancat-probe"), b"").is_ok() {
        let _ = std::fs::remove_file(public.join(".lancat-probe"));
        return Some(public);
    }
    let own = app.path().document_dir().ok().or_else(|| app.path().app_data_dir().ok().map(|d| d.join("received")))?;
    let _ = std::fs::create_dir_all(&own);
    Some(own)
}

fn start_core(app: &tauri::App) -> anyhow::Result<Arc<Node>> {
    let data_dir = app.path().app_data_dir()?;
    let mut cfg = NodeConfig::new(data_dir).with_env();
    if cfg.recv_dir_override.is_none() {
        cfg.recv_dir_override = receive_dir(app.handle());
    }
    if cfg.name_override.is_none() {
        cfg.name_override = device_name();
    }
    tauri::async_runtime::block_on(async {
        match Node::start(cfg.clone()).await {
            Ok(n) => Ok(n),
            Err(e) => {
                log::warn!("{e:#}; falling back to a random port");
                let mut c = cfg;
                c.port = 0;
                Node::start(c).await
            }
        }
    })
}

/// The phone's model as its name (hostname is "localhost" on Android).
fn device_name() -> Option<String> {
    let get = |k: &str| -> Option<String> {
        let out = std::process::Command::new("/system/bin/getprop").arg(k).output().ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    get("persist.sys.device_name").or_else(|| get("ro.product.marketname")).or_else(|| {
        let brand = get("ro.product.brand").unwrap_or_default();
        let model = get("ro.product.model")?;
        Some(if model.to_lowercase().starts_with(&brand.to_lowercase()) { model } else { format!("{brand} {model}") })
    })
}

pub fn run() {
    #[cfg(target_os = "android")]
    android_logger_init();
    tauri::Builder::default()
        .plugin(native_plugin())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::ui_log,
            commands::get_me,
            commands::list_peers,
            commands::refresh_peers,
            commands::get_messages,
            commands::send_text,
            commands::send_emoji,
            commands::send_files,
            commands::send_typing,
            commands::mark_read,
            commands::retry_transfer,
            commands::pause_transfer,
            commands::cancel_transfer,
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
            pick_files,
            take_shared,
        ])
        .setup(|app| {
            let node = start_core(app).map_err(|e| e.to_string())?;
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
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::default()).build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Lancat");
}

/// Rust `log` lines go to logcat (tag "lancat"): `adb logcat -s lancat`.
#[cfg(target_os = "android")]
fn android_logger_init() {
    struct L;
    impl log::Log for L {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info || m.target().starts_with("lancat")
        }
        fn log(&self, r: &log::Record) {
            if !self.enabled(r.metadata()) {
                return;
            }
            let prio = match r.level() {
                log::Level::Error => 6,
                log::Level::Warn => 5,
                log::Level::Info => 4,
                _ => 3,
            };
            let msg = std::ffi::CString::new(format!("{}: {}", r.target(), r.args())).unwrap_or_default();
            unsafe { __android_log_write(prio, c"lancat".as_ptr(), msg.as_ptr()) };
        }
        fn flush(&self) {}
    }
    extern "C" {
        fn __android_log_write(prio: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32;
    }
    static LOGGER: L = L;
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Debug);
}
