//! Lancat desktop shell: starts the Rust core, exposes IPC commands, forwards core events.

mod commands;
mod diag;
mod dock;
mod native_dock;
mod platform;
mod timers;
mod tray;

use std::sync::Arc;

use lancat_core::{Node, NodeConfig};
use tauri::{Emitter, Manager};

use commands::Core;

pub const EVENT: &str = "lancat://event";

/// Starts the core; also returns the TCP port it was configured for (diagnostics: taken?).
fn start_core(app: &tauri::App) -> anyhow::Result<(Arc<Node>, u16)> {
    let data_dir = app.path().app_data_dir()?;
    let cfg = NodeConfig::new(data_dir).with_env();
    let wanted = cfg.port;
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
    Ok((node, wanted))
}

#[tauri::command]
fn autostart_enabled(app: tauri::AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let al = app.autolaunch();
    let r = if enabled { al.enable() } else { al.disable() };
    tray::sync(&app);
    r.map_err(|e| e.to_string())
}

/// Measurement hooks (scripts/memprobe.sh), real-time signals relative to SIGRTMIN:
/// +6 open the panel, +7 collapse it, +8 pointer entered the strip (pre-warm), +9 pointer left.
/// Works with or without the single-instance lock (LANCAT_DATA_DIR disables it).
/// Not SIGUSR1/SIGUSR2: JavaScriptCore uses SIGUSR2 to suspend threads for GC (JSC_SIGNAL_FOR_GC)
/// and replacing that handler crashes the process.
#[cfg(unix)]
fn spawn_signal_control(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let rtmin = 34; // glibc SIGRTMIN (32 and 33 are reserved by NPTL)
        let mut sigs = vec![];
        for n in 6..=9 {
            match signal(SignalKind::from_raw(rtmin + n)) {
                Ok(s) => sigs.push(s),
                Err(_) => return,
            }
        }
        let [mut open, mut close, mut enter, mut leave]: [tokio::signal::unix::Signal; 4] = match sigs.try_into() {
            Ok(a) => a,
            Err(_) => return,
        };
        loop {
            let what = tokio::select! {
                _ = open.recv() => 6,
                _ = close.recv() => 7,
                _ = enter.recv() => 8,
                _ = leave.recv() => 9,
            };
            let a = app.clone();
            let _ = app.run_on_main_thread(move || match what {
                6 => dock::open_panel(&a, "probe"),
                7 => dock::request_collapse(&a),
                8 => dock::on_hover_start(&a),
                _ => dock::on_hover_end(&a),
            });
        }
    });
}

pub fn run() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,tao=warn,wry=warn")).try_init();
    // graphics env defaults must be in place before GTK/EGL/WebKit start (Linux)
    platform::early_init();
    // A LAN share tool needs no per-core worker pool: 2 workers (LANCAT_RT_WORKERS overrides).
    // Tauri would otherwise start one tokio worker per CPU.
    let workers = std::env::var("LANCAT_RT_WORKERS").ok().and_then(|v| v.parse().ok()).unwrap_or(2usize).max(1);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .max_blocking_threads(8)
        .thread_name("lancat-rt")
        .enable_all()
        .build()
        .expect("tokio runtime");
    tauri::async_runtime::set(rt.handle().clone());
    // the runtime lives as long as the process
    std::mem::forget(rt);
    let mut builder = tauri::Builder::default();
    // One instance per user. A second launch just opens the panel of the running one.
    // (Skipped when LANCAT_DATA_DIR is set, so two dev instances can run side by side.)
    if std::env::var_os("LANCAT_DATA_DIR").is_none() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // `lancat --settings` opens settings, `--hide-panel` collapses the running instance's
            // panel and `--open-panel` opens it (memprobe.ps1 uses both); any other second launch opens it.
            if args.iter().any(|a| a == "--settings") {
                let _ = dock::open_settings(app);
            } else if args.iter().any(|a| a == "--hide-panel") {
                dock::request_collapse(app);
            } else if args.iter().any(|a| a == "--open-panel") {
                dock::open_panel(app, "probe");
            } else {
                dock::open_panel(app, "tray");
            }
        }));
    }
    builder
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--autostart"])))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::ui_log,
            commands::dock_move_start,
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
            dock::cmd_open_panel,
            dock::cmd_hide_panel,
            dock::set_pinned,
            dock::set_hold,
            dock::panel_state,
            dock::panel_shown,
            dock::panel_ready,
            dock::cmd_open_settings,
            dock::list_monitors,
            autostart_enabled,
            set_autostart,
            diag::get_diagnostics,
            diag::run_fix,
            diag::diag_status,
            diag::dismiss_alert,
            diag::open_diagnostics,
            diag::diag_take_goto,
        ])
        .on_window_event(|window, event| {
            // closing the panel only hides it (dock.rs destroys it later); closing settings closes it
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "panel" {
                    api.prevent_close();
                    dock::hide_panel(window.app_handle());
                }
            }
            // system light/dark switch: the native material's tint follows ("system" theme)
            #[cfg(windows)]
            if let tauri::WindowEvent::ThemeChanged(_) = event {
                if let Some(p) = window.app_handle().get_webview_window("panel") {
                    platform::refresh_material(&p);
                }
            }
        })
        .setup(|app| {
            let (node, wanted_port) = start_core(app).map_err(|e| e.to_string())?;
            // forward core events to every webview
            let handle = app.handle().clone();
            let mut rx = node.subscribe();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            let _ = handle.emit(EVENT, &ev);
                            if let lancat_core::CoreEvent::Message { message } = &ev {
                                let (h, m) = (handle.clone(), message.clone());
                                let _ = handle.run_on_main_thread(move || dock::on_incoming(&h, &m));
                            }
                            if let lancat_core::CoreEvent::Alert { kind, report, detail } = &ev {
                                diag::on_core_alert(&handle, kind, report.as_ref(), detail);
                            }
                            if matches!(ev, lancat_core::CoreEvent::Peer { .. } | lancat_core::CoreEvent::HistoryCleared { .. }) {
                                tray::update_unread(&handle);
                                let h = handle.clone();
                                let _ = handle.run_on_main_thread(move || dock::refresh_visual(&h));
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });
            app.manage(Core(node));
            app.manage(dock::DockState::default());
            diag::init(app.handle(), wanted_port);
            platform::init_app(app);
            dock::create_windows(app.handle())?;
            tray::create(app.handle())?;
            if std::env::var_os("LANCAT_OPEN_ON_START").is_some() {
                // debugging / screenshots: show the panel once the webview is up
                let h = app.handle().clone();
                timers::after(std::time::Duration::from_millis(1500), move || {
                    let h2 = h.clone();
                    let _ = h.run_on_main_thread(move || dock::open_panel(&h2, "tray"));
                });
            }
            platform::first_run(app.handle());
            if std::env::args().any(|a| a == "--settings") {
                let _ = dock::open_settings(app.handle());
            }
            #[cfg(unix)]
            spawn_signal_control(app.handle().clone());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Lancat")
        .run(|app, event| match event {
            // The dock strip is not a tauri window, so destroying the idle panel can leave zero
            // tauri windows; that must not quit the app. Explicit exits (tray → quit) carry a code.
            tauri::RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
            // any real exit says goodbye so peers mark us offline at once (idempotent)
            tauri::RunEvent::Exit => {
                // a frozen (SIGSTOPped) panel renderer must be able to notice we are gone and exit
                #[cfg(target_os = "linux")]
                platform::thaw_all();
                if let Some(core) = app.try_state::<Core>() {
                    core.0.shutdown();
                }
            }
            _ => {}
        });
}
