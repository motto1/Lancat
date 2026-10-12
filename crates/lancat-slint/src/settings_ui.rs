//! 设置窗口控制（旧版 Settings.svelte / DiagSection.svelte）：读写 lancat-core Settings（防抖保存，
//! 「已保存」提示）、设备拉黑、手动设备、开机自启、主题、清空记录、权限与诊断（检查 / 修复 / 预览）。

use crate::{os_glyph, post, with_app, DevItem, DiagCheck, SettingsWindow, Theme};
use lancat_core::{Node, PeerView, Settings};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub struct SettingsCtl {
    win: SettingsWindow,
    node: Arc<Node>,
    #[allow(dead_code)]
    rt: tokio::runtime::Handle,
    save_timer: slint::Timer,
    saved_timer: slint::Timer,
    clear_timer: slint::Timer,
    /// 修复 / 预览结果，按检查 id
    results: HashMap<String, (String, String, bool, bool)>,
    checking: bool,
}

fn fmt_hm(ts: i64) -> String {
    use chrono::{Local, TimeZone};
    Local.timestamp_millis_opt(ts).single().map(|d| d.format("%H:%M").to_string()).unwrap_or_default()
}

impl SettingsCtl {
    pub fn new(node: Arc<Node>, rt: tokio::runtime::Handle) -> Option<Self> {
        #[cfg(windows)]
        let win = crate::win::with_normal_window(SettingsWindow::new).ok()?;
        #[cfg(not(windows))]
        let win = SettingsWindow::new().ok()?;
        let me = node.me();
        win.set_me_name(me.name.clone().into());
        win.set_me_id(me.id.clone().into());
        win.set_me_port(node.port().to_string().into());
        win.set_me_ver(me.ver.clone().into());
        win.set_me_os(os_glyph(&me.os).into());
        win.set_emoji_count(crate::emoji_catalog::EMOJI.len() as i32);
        #[cfg(windows)]
        win.set_dry_run_forced(crate::diag::dry_run_forced());
        wire(&win);
        let ctl = SettingsCtl { win, node, rt, save_timer: Default::default(), saved_timer: Default::default(), clear_timer: Default::default(), results: HashMap::new(), checking: false };
        Some(ctl)
    }

    pub fn set_dark(&self, dark: bool) {
        self.win.global::<Theme>().set_dark(dark);
        #[cfg(windows)]
        crate::win::set_normal_dark(dark);
    }

    pub fn show(&mut self, goto_diag: bool, peers: &[PeerView]) {
        let s = self.node.settings();
        self.load(&s, true);
        self.peers_changed(peers);
        #[cfg(windows)]
        {
            self.win.set_autostart(crate::dock::tray::autostart_enabled());
            self.win.set_autostart_enabled(!crate::dock::tray::test_instance());
            let mons: Vec<SharedString> = crate::dock::monitor_labels().into_iter().map(Into::into).collect();
            self.win.set_monitors(ModelRc::new(VecModel::from(mons)));
            if let Some(d) = crate::diag::last() {
                self.diag_changed(&d);
            }
        }
        let _ = self.win.show();
        self.recheck();
        if goto_diag {
            let w = self.win.as_weak();
            slint::Timer::single_shot(Duration::from_millis(50), move || {
                if let Some(win) = w.upgrade() {
                    let y = win.get_diag_y();
                    win.set_scroll_y(-(y - 4.0));
                }
            });
        }
    }

    fn load(&self, s: &Settings, text_too: bool) {
        let w = &self.win;
        if text_too {
            w.set_device_name(s.device_name.clone().into());
            w.set_max_file_mb(s.max_file_mb.to_string().into());
            w.set_blocked_ips(s.blocked_ips.join("\n").into());
        }
        w.set_dock_edge(s.dock_edge.clone().into());
        w.set_dock_offset(s.dock_offset as f32);
        w.set_dock_length(s.dock_length as f32);
        w.set_hover_delay(s.hover_delay_ms as f32);
        w.set_collapse_delay(s.collapse_delay_ms as f32);
        w.set_keep_alive(s.panel_keep_alive_s as f32);
        w.set_dock_monitor(s.dock_monitor as i32);
        w.set_theme(s.theme.clone().into());
        w.set_recv_dir(self.node.receive_dir().display().to_string().into());
        w.set_recv_custom(!s.receive_dir.is_empty());
        let mp: Vec<SharedString> = s.manual_peers.iter().map(|a| a.into()).collect();
        w.set_manual_peers(ModelRc::new(VecModel::from(mp)));
    }

    pub fn settings_changed(&mut self, s: &Settings) {
        // 文本框正在编辑：不覆盖（只刷新非文本项）
        self.load(s, false);
    }

    pub fn peers_changed(&mut self, peers: &[PeerView]) {
        let v: Vec<DevItem> = peers
            .iter()
            .map(|p| DevItem {
                id: p.id.clone().into(),
                name: p.name.clone().into(),
                sub: format!("{} · {}", os_glyph(&p.os), if p.addr.is_empty() { "—" } else { &p.addr }).into(),
                online: p.online,
                blocked: p.blocked,
            })
            .collect();
        self.win.set_devices(ModelRc::new(VecModel::from(v)));
    }

    /// 从界面读回 Settings 并保存
    fn save_now(&mut self) {
        let w = &self.win;
        let mut s = self.node.settings();
        s.device_name = w.get_device_name().trim().to_string();
        s.dock_offset = (w.get_dock_offset() as f64 * 100.0).round() / 100.0;
        s.dock_length = w.get_dock_length().round() as f64;
        s.hover_delay_ms = w.get_hover_delay().round() as u64;
        s.collapse_delay_ms = w.get_collapse_delay().round() as u64;
        s.panel_keep_alive_s = w.get_keep_alive().round() as u64;
        s.max_file_mb = w.get_max_file_mb().trim().parse().unwrap_or(0);
        s.blocked_ips = w.get_blocked_ips().lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        match self.node.set_settings(s) {
            Ok(_) => self.flash_saved(),
            Err(e) => log::warn!("save settings: {e}"),
        }
    }

    fn flash_saved(&mut self) {
        self.win.set_saved_flash(true);
        let w = self.win.as_weak();
        self.saved_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(1200), move || {
            if let Some(win) = w.upgrade() {
                win.set_saved_flash(false);
            }
        });
    }

    fn update(&mut self, f: impl FnOnce(&mut Settings)) {
        let mut s = self.node.settings();
        f(&mut s);
        match self.node.set_settings(s) {
            Ok(s) => {
                self.load(&s, false);
                self.flash_saved();
            }
            Err(e) => log::warn!("save settings: {e}"),
        }
    }

    // ---------------- diagnostics ----------------

    fn recheck(&mut self) {
        #[cfg(windows)]
        {
            if self.checking {
                return;
            }
            self.checking = true;
            self.win.set_checking(true);
            self.win.set_diag_error("".into());
            crate::diag::runtime().spawn(async move {
                let r = crate::diag::refresh().await;
                post(move |app| {
                    if let Some(sc) = app.settings.as_mut() {
                        sc.checking = false;
                        sc.win.set_checking(false);
                        match r {
                            Ok(d) => sc.diag_changed(&d),
                            Err(e) => sc.win.set_diag_error(e.into()),
                        }
                    }
                });
            });
        }
    }

    #[cfg(windows)]
    pub fn diag_changed(&mut self, d: &crate::diag::Diagnostics) {
        let rows: Vec<DiagCheck> = d
            .checks
            .iter()
            .map(|c| {
                let mut detail = c.detail.join("\n");
                if let Some(h) = &c.help {
                    if !detail.is_empty() {
                        detail.push_str("\n\n");
                    }
                    detail.push_str(h);
                }
                let (result, cmds, ok, fail) = self.results.get(&c.id).cloned().unwrap_or_default();
                DiagCheck {
                    id: c.id.clone().into(),
                    title: c.title.clone().into(),
                    status: c.status.clone().into(),
                    summary: c.summary.clone().into(),
                    detail: detail.into(),
                    fix_id: c.fix.as_ref().map(|f| f.id.clone()).unwrap_or_default().into(),
                    fix_label: c.fix.as_ref().map(|f| f.label.clone()).unwrap_or_default().into(),
                    fix_admin: c.fix.as_ref().is_some_and(|f| f.admin),
                    fix_confirm: c.fix.as_ref().and_then(|f| f.confirm.clone()).unwrap_or_default().into(),
                    result: result.into(),
                    result_cmds: cmds.into(),
                    result_ok: ok,
                    result_fail: fail,
                }
            })
            .collect();
        self.win.set_checks(ModelRc::new(VecModel::from(rows)));
        self.win.set_problems(d.problems as i32);
        self.win.set_diag_time(fmt_hm(d.ts).into());
        self.win.set_dry_run_forced(d.dry_run_forced);
    }

    #[cfg(windows)]
    fn run_fix(&mut self, fix_id: String, dry: bool) {
        // which check carries this fix (the result shows under it)
        let check_id = crate::diag::last().and_then(|d| d.checks.iter().find(|c| c.fix.as_ref().is_some_and(|f| f.id == fix_id)).map(|c| c.id.clone())).unwrap_or_default();
        self.win.set_fix_busy(true);
        crate::diag::runtime().spawn(async move {
            let r = crate::diag::run_fix(fix_id, dry).await;
            post(move |app| {
                let Some(sc) = app.settings.as_mut() else { return };
                sc.win.set_fix_busy(false);
                let entry = match &r {
                    Ok(fr) => (fr.message.clone(), if fr.dry_run { fr.commands.join("\n") } else { String::new() }, fr.ok && !fr.dry_run, !fr.ok),
                    Err(e) => (e.clone(), String::new(), false, true),
                };
                sc.results.insert(check_id, entry);
                let d = r.ok().and_then(|fr| fr.diagnostics).or_else(crate::diag::last);
                if let Some(d) = d {
                    sc.diag_changed(&d);
                }
            });
        });
    }
}

fn ctl(f: impl FnOnce(&mut SettingsCtl)) {
    with_app(|app| {
        if let Some(sc) = app.settings.as_mut() {
            f(sc);
        }
    });
}

fn wire(w: &SettingsWindow) {
    w.on_save(|| {
        ctl(|sc| {
            // 防抖 300 ms（滑块拖动、打字）
            sc.save_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(300), || ctl(|sc| sc.save_now()));
        });
    });
    w.on_save_now(|| ctl(|sc| sc.save_now()));
    w.on_set_edge(|e| ctl(|sc| sc.update(|s| s.dock_edge = e.to_string())));
    w.on_set_monitor(|m| ctl(|sc| sc.update(|s| s.dock_monitor = m.max(0) as usize)));
    w.on_set_theme(|t| ctl(|sc| sc.update(|s| s.theme = t.to_string())));
    w.on_reset_dir(|| ctl(|sc| sc.update(|s| s.receive_dir.clear())));
    w.on_pick_dir(|| {
        #[cfg(windows)]
        crate::filedlg::pick_folder(|p| {
            if let Some(p) = p {
                ctl(move |sc| sc.update(|s| s.receive_dir = p));
            }
        });
    });
    w.on_toggle_block(|id| {
        with_app(|app| {
            let blocked = app.peers.iter().find(|p| p.id == id.as_str()).is_some_and(|p| p.blocked);
            if let Err(e) = app.node.set_blocked(id.as_str(), !blocked) {
                app.flash(e.to_string());
            }
        });
    });
    w.on_remove_manual(|a| ctl(|sc| sc.update(|s| s.manual_peers.retain(|x| x != a.as_str()))));
    w.on_toggle_autostart(|| {
        #[cfg(windows)]
        ctl(|sc| {
            let on = !crate::dock::tray::autostart_enabled();
            crate::dock::tray::set_autostart(on);
            sc.win.set_autostart(crate::dock::tray::autostart_enabled());
        });
    });
    w.on_clear_all(|| {
        ctl(|sc| {
            if !sc.win.get_confirm_clear() {
                sc.win.set_confirm_clear(true);
                let w = sc.win.as_weak();
                sc.clear_timer.start(slint::TimerMode::SingleShot, Duration::from_secs(3), move || {
                    if let Some(win) = w.upgrade() {
                        win.set_confirm_clear(false);
                    }
                });
                return;
            }
            sc.win.set_confirm_clear(false);
            if let Err(e) = sc.node.clear_history(None) {
                log::warn!("clear history: {e}");
            }
        });
    });
    w.on_recheck(|| ctl(|sc| sc.recheck()));
    w.on_run_fix(|id, dry| {
        #[cfg(windows)]
        ctl(move |sc| sc.run_fix(id.to_string(), dry));
        #[cfg(not(windows))]
        let _ = (id, dry);
    });
}

/// 测试：在设置窗口里点这个修复（调用方保证 LANCAT_FIX_DRY_RUN=1，只出预览）
#[cfg(windows)]
pub fn test_fix(fix_id: String) {
    slint::Timer::single_shot(Duration::from_millis(0), move || ctl(move |sc| sc.run_fix(fix_id, false)));
}
