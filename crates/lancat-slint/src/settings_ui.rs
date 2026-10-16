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

/// toLocaleTimeString([], { hour, minute, second: "2-digit" })
fn fmt_hms(ts: i64) -> String {
    use chrono::{Local, TimeZone};
    Local.timestamp_millis_opt(ts).single().map(|d| d.format(crate::time_fmt(true)).to_string()).unwrap_or_default()
}

impl SettingsCtl {
    pub fn new(node: Arc<Node>, rt: tokio::runtime::Handle) -> Option<Self> {
        #[cfg(windows)]
        let win = crate::win::with_normal_window(SettingsWindow::new).ok()?;
        #[cfg(not(windows))]
        let win = SettingsWindow::new().ok()?;
        install_fx!(win, true);
        let me = node.me();
        win.set_me_name(me.name.clone().into());
        win.set_me_id(me.id.clone().into());
        win.set_me_port(node.port().to_string().into());
        win.set_me_ver(crate::update::VERSION.into());
        win.set_me_os(os_glyph(&me.os).into());
        win.set_emoji_count(crate::emoji_catalog::EMOJI.len() as i32);
        let n = crate::emoji_catalog::EMOJI.len();
        flow::install(
            &win,
            &[
                (&format!("内置 {n} 个动态表情来自 Google "), false),
                ("Noto Animated Emoji", true),
                ("，以 ", false),
                ("CC BY 4.0", true),
                (" 许可使用（https://creativecommons.org/licenses/by/4.0/）。", false),
            ],
        );
        win.set_dry_run_forced(crate::diag::dry_run_forced());
        wire(&win);
        let ctl = SettingsCtl { win, node, rt, save_timer: Default::default(), saved_timer: Default::default(), clear_timer: Default::default(), results: HashMap::new(), checking: false };
        Some(ctl)
    }

    pub fn set_dark(&self, dark: bool) {
        self.win.global::<Theme>().set_dark(dark);
        // 标题栏跟系统深浅色走，不跟应用主题（旧版 Tauri 窗口就是这样：应用深色、系统浅色时标题栏仍是浅色）
        let _ = dark;
        #[cfg(windows)]
        crate::win::set_normal_dark(crate::theme_is_dark("system"));
    }

    pub fn show(&mut self, goto_diag: bool, peers: &[PeerView]) {
        let s = self.node.settings();
        self.load(&s, true);
        self.peers_changed(peers);
        self.load_update();
        self.win.set_autostart(crate::dock::tray::autostart_enabled());
        self.win.set_autostart_enabled(!crate::dock::tray::test_instance());
        let mons: Vec<SharedString> = crate::dock::monitor_labels().into_iter().map(Into::into).collect();
        self.win.set_monitors(ModelRc::new(VecModel::from(mons)));
        if let Some(d) = crate::diag::last() {
            self.diag_changed(&d);
        }
        let _ = self.win.show();
        self.win.global::<crate::Fx>().set_dpr(self.win.window().scale_factor());
        #[cfg(target_os = "linux")]
        self.win.global::<crate::Fx>().set_hair(1.0 / self.win.window().scale_factor().max(1.0));
        // 旧版：打开设置页就检查一次（DiagSection onMount）；从「去修复」打开时滚过去再查
        self.recheck();
        if goto_diag {
            let w = self.win.as_weak();
            slint::Timer::single_shot(Duration::from_millis(60), move || {
                if let Some(win) = w.upgrade() {
                    goto_diag_section(&win);
                }
            });
        }
    }

    fn load_update(&self) {
        let st = crate::update::status();
        self.win.set_upd_state(st.state.into());
        self.win.set_upd_text(st.text.into());
    }

    pub fn is_open(&self) -> bool {
        self.win.window().is_visible()
    }

    /// 已开着的设置窗口提到前台（show() 对已显示的窗口会激活它）
    pub fn raise(&self) {
        let _ = self.win.show();
    }

    /// 已经开着时再次被要求跳到诊断（lancat://goto-diag）：滚过去 + 闪烁 + 重新检查
    pub fn goto_diag(&mut self) {
        goto_diag_section(&self.win);
        self.recheck();
    }

    fn load(&self, s: &Settings, text_too: bool) {
        let w = &self.win;
        if text_too {
            w.set_device_name(s.device_name.clone().into());
            w.set_max_file_mb(s.max_file_mb.to_string().into());
            w.set_blocked_ips(s.blocked_ips.join("\n").into());
        }
        // 正在拖滑块 / 防抖保存还没落盘：不拿保存回来的（可能更旧的）值覆盖界面 —— 否则滑块会跳回去
        let editing = w.get_editing() || self.save_timer.running();
        if text_too || !editing {
            w.set_dock_offset(s.dock_offset as f32);
            w.set_dock_length(s.dock_length as f32);
            w.set_hover_delay(s.hover_delay_ms as f32);
            w.set_collapse_delay(s.collapse_delay_ms as f32);
            w.set_keep_alive(s.panel_keep_alive_s as f32);
        }
        w.set_dock_edge(s.dock_edge.clone().into());
        w.set_dock_monitor(s.dock_monitor as i32);
        w.set_theme(s.theme.clone().into());
        w.set_auto_update(s.auto_update);
        w.set_message_sound(s.message_sound);
        let dir = self.node.receive_dir().display().to_string();
        w.set_recv_dir(dir.clone().into());
        w.set_recv_dir_shown(rtl_shown(&dir).into());
        w.set_recv_custom(!s.receive_dir.is_empty());
        let mp: Vec<SharedString> = s.manual_peers.iter().map(|a| a.into()).collect();
        w.set_manual_peers(ModelRc::new(VecModel::from(mp)));
        w.set_public_access(s.public_access);
        w.set_pair_code(s.pair_code.clone().into());
        let pr: Vec<DevItem> = s
            .paired_remotes
            .iter()
            .map(|p| DevItem { id: p.id.clone().into(), name: p.name.clone().into(), sub: "已通过公网配对".into(), online: false, blocked: false })
            .collect();
        w.set_paired_remotes(ModelRc::new(VecModel::from(pr)));
        let rp: Vec<DevItem> = s
            .remote_peers
            .iter()
            .map(|r| {
                let online = !r.peer_id.is_empty() && self.node.is_online(&r.peer_id);
                let state = if online {
                    "已连接"
                } else if r.token.is_empty() && r.code.is_empty() {
                    "配对失败：请在主界面重新输入「地址 配对码」"
                } else if r.token.is_empty() {
                    "正在配对…"
                } else {
                    "未连接（对方离线或地址不通）"
                };
                let name = if r.name.is_empty() { r.addr.clone() } else { r.name.clone() };
                DevItem { id: r.addr.clone().into(), name: name.into(), sub: format!("{} · {state}", r.addr).into(), online, blocked: false }
            })
            .collect();
        w.set_remote_peers(ModelRc::new(VecModel::from(rp)));
    }

    pub fn remote_alert(&self, detail: &str) {
        self.win.set_remote_msg(detail.into());
        self.win.set_remote_msg_bad(true);
    }

    pub fn settings_changed(&mut self, s: &Settings) {
        // 文本框正在编辑：不覆盖（只刷新非文本项）
        self.load(s, false);
    }

    pub fn peers_changed(&mut self, peers: &[PeerView]) {
        self.load(&self.node.settings(), false);
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
        s.blocked_ips = w.get_blocked_ips().split(|c: char| c.is_whitespace() || c == ',').map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        match self.node.set_settings(s) {
            Ok(_) => self.flash_saved(),
            Err(e) => log::warn!("save settings: {e}"),
        }
    }

    /// 「已保存」1.2 s：旧版每次保存都 setTimeout(saved = false, 1200) 且不清前一个 —— 第一次的到点就收起
    fn flash_saved(&mut self) {
        if self.saved_timer.running() {
            return;
        }
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

    pub fn diag_changed(&mut self, d: &crate::diag::Diagnostics) {
        let rows: Vec<DiagCheck> = d
            .checks
            .iter()
            .map(|c| {
                let (result, cmds, ok, fail) = self.results.get(&c.id).cloned().unwrap_or_default();
                DiagCheck {
                    id: c.id.clone().into(),
                    title: c.title.clone().into(),
                    status: c.status.clone().into(),
                    summary: c.summary.clone().into(),
                    detail: c.detail.join("\n").into(),
                    help: c.help.clone().unwrap_or_default().into(),
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
        self.win.set_diag_time(fmt_hms(d.ts).into());
        self.win.set_dry_run_forced(d.dry_run_forced);
    }

    fn run_fix(&mut self, fix_id: String, dry: bool) {
        // which check carries this fix (the result shows under it)
        let check_id = crate::diag::last().and_then(|d| d.checks.iter().find(|c| c.fix.as_ref().is_some_and(|f| f.id == fix_id)).map(|c| c.id.clone())).unwrap_or_default();
        self.win.set_fix_busy(format!("{fix_id}:{dry}").into());
        self.results.remove(&check_id);
        crate::diag::runtime().spawn(async move {
            let r = crate::diag::run_fix(fix_id, dry).await;
            post(move |app| {
                let Some(sc) = app.settings.as_mut() else { return };
                sc.win.set_fix_busy("".into());
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

/// scrollIntoView({ behavior: "smooth", block: "start" })（scroll-margin-top 12）+ 区块闪烁 1.4 s
/// 更新状态变了（检查 / 下载 / 出错）：设置窗口开着就刷新。
pub fn update_status_changed() {
    ctl(|sc| sc.load_update());
}

/// 滚到「更新」（设置页最底下两节之一）。
/// 滚到「公网连接」并把焦点放进地址框（主界面「通过公网地址连接」点进来）；`addr` 有值时预填，焦点给配对码
pub fn goto_public(addr: String) {
    ctl(move |sc| {
        let win = &sc.win;
        if !addr.is_empty() {
            win.set_remote_addr(addr.clone().into());
        }
        let max = (win.get_content_h() - win.get_view_h()).max(0.0);
        let to = -(win.get_pub_y() - 12.0).clamp(0.0, max);
        let from = win.get_scroll_y();
        let w = win.as_weak();
        crate::smooth_scroll("settings", from, to, move |v| {
            if let Some(win) = w.upgrade() {
                win.set_scroll_y(v);
            }
        });
        if addr.is_empty() {
            win.invoke_focus_remote_addr();
        } else {
            win.invoke_focus_remote_code();
        }
    });
}

pub fn goto_update() {
    ctl(|sc| {
        let win = &sc.win;
        let max = (win.get_content_h() - win.get_view_h()).max(0.0);
        let from = win.get_scroll_y();
        let w = win.as_weak();
        crate::smooth_scroll("settings", from, -max, move |v| {
            if let Some(win) = w.upgrade() {
                win.set_scroll_y(v);
            }
        });
    });
}

fn goto_diag_section(win: &SettingsWindow) {
    let y = win.get_diag_y();
    let max = (win.get_content_h() - win.get_view_h()).max(0.0);
    let to = -(y - 12.0).clamp(0.0, max);
    let from = win.get_scroll_y();
    let w = win.as_weak();
    crate::smooth_scroll("settings", from, to, move |v| {
        if let Some(win) = w.upgrade() {
            win.set_scroll_y(v);
        }
    });
    // sec-flash 1.4 s var(--ease)：0–40% 不变，之后淡出
    let w = win.as_weak();
    crate::tween("diag-flash", 1400.0, move |t| {
        if let Some(win) = w.upgrade() {
            let a = if t <= 0.4 { 1.0 } else { 1.0 - crate::fx::bezier(0.2, 0.8, 0.3, 1.0, (t - 0.4) / 0.6) };
            win.set_diag_flash(a);
        }
    });
}

fn ctl(f: impl FnOnce(&mut SettingsCtl)) {
    with_app(|app| {
        if let Some(sc) = app.settings.as_mut() {
            f(sc);
        }
    });
}

/// compositor 要客户端自己画装饰（Wayland，niri 没开 prefer-no-csd）：显示旧版 GTK 那条标题栏
#[cfg(target_os = "linux")]
pub fn wl_decor(csd: bool, maximized: bool) {
    crate::post(move |app| {
        if let Some(sc) = app.settings.as_ref() {
            sc.win.set_csd(csd);
            sc.win.set_csd_maximized(maximized);
        }
    });
}

fn wire(w: &SettingsWindow) {
    // 自绘标题栏：0 拖动 1 最小化 2 最大化/还原 3 关闭 4 窗口菜单（x, y）
    w.on_csd_act(|a, _x, _y| {
        #[cfg(target_os = "linux")]
        {
            use crate::wl::{push, Cmd, TopAct};
            match a {
                0 => push(Cmd::TopAct(TopAct::Move)),
                1 => push(Cmd::TopAct(TopAct::Minimize)),
                2 => push(Cmd::TopAct(TopAct::Maximize)),
                4 => push(Cmd::TopAct(TopAct::Menu(_x as i32, _y as i32))),
                _ => {}
            }
        }
        if a == 3 {
            ctl(|sc| {
                let _ = sc.win.hide();
            });
        }
    });
    w.on_save(|| {
        ctl(|sc| {
            // 防抖 250 ms（旧版 save()）
            sc.save_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(250), || ctl(|sc| sc.save_now()));
        });
    });
    w.on_save_now(|| ctl(|sc| sc.save_now()));
    w.on_set_edge(|e| ctl(|sc| sc.update(|s| s.dock_edge = e.to_string())));
    w.on_set_monitor(|m| ctl(|sc| sc.update(|s| s.dock_monitor = m.max(0) as usize)));
    w.on_set_theme(|t| ctl(|sc| sc.update(|s| s.theme = t.to_string())));
    w.on_reset_dir(|| ctl(|sc| sc.update(|s| s.receive_dir.clear())));
    w.on_pick_dir(|| {
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
    w.on_toggle_public(|| {
        ctl(|sc| {
            let on = !sc.node.settings().public_access;
            sc.node.set_public_access(on);
        })
    });
    w.on_regen_code(|| {
        ctl(|sc| {
            sc.node.regen_pair_code();
        })
    });
    w.on_unpair(|id| ctl(|sc| sc.node.unpair_remote(id.as_str())));
    w.on_remove_remote(|a| ctl(|sc| sc.node.remove_remote_peer(a.as_str())));
    w.on_add_remote(|a, c| {
        ctl(|sc| {
            let (a, c) = (a.trim().to_string(), c.trim().to_string());
            let w = &sc.win;
            if a.is_empty() {
                w.set_remote_msg("先填对方给你的地址".into());
                w.set_remote_msg_bad(true);
                w.invoke_focus_remote_addr();
                return;
            }
            if lancat_core::remote_ws_url(&a).is_none() {
                w.set_remote_msg("这是局域网地址，不需要配对：直接在主界面「手动添加」里输入即可".into());
                w.set_remote_msg_bad(true);
                return;
            }
            match sc.node.add_remote_peer(&a, &c) {
                Ok(()) => {
                    w.set_remote_addr("".into());
                    w.set_remote_code("".into());
                    w.set_remote_msg(format!("正在连接 {a}…，结果显示在下面的列表里").into());
                    w.set_remote_msg_bad(false);
                }
                Err(e) => {
                    w.set_remote_msg(e.to_string().into());
                    w.set_remote_msg_bad(true);
                    w.invoke_focus_remote_code();
                }
            }
        })
    });
    w.on_toggle_autostart(|| {
        ctl(|sc| {
            let on = !crate::dock::tray::autostart_enabled();
            crate::dock::tray::set_autostart(on);
            sc.win.set_autostart(crate::dock::tray::autostart_enabled());
        });
    });
    w.on_clear_all(|| {
        ctl(|sc| {
            if !sc.win.get_confirm_clear() {
                // 旧版：4 s 内再点一次才清空
                sc.win.set_confirm_clear(true);
                let w = sc.win.as_weak();
                sc.clear_timer.start(slint::TimerMode::SingleShot, Duration::from_secs(4), move || {
                    if let Some(win) = w.upgrade() {
                        win.set_confirm_clear(false);
                    }
                });
                return;
            }
            sc.win.set_confirm_clear(false);
            sc.clear_timer.stop();
            if let Err(e) = sc.node.clear_history(None) {
                log::warn!("clear history: {e}");
            }
            sc.flash_saved();
        });
    });
    w.on_recheck(|| ctl(|sc| sc.recheck()));
    w.on_check_update(|| crate::update::check_bg(true));
    w.on_do_update(crate::update::install_bg);
    w.on_open_release(crate::update::open_release_page);
    w.on_toggle_auto_update(|| {
        ctl(|sc| sc.update(|s| s.auto_update = !s.auto_update));
        if let Some(on) = with_app(|app| app.node.settings().auto_update) {
            crate::update::auto_changed(on);
        }
    });
    w.on_toggle_message_sound(|| {
        ctl(|sc| sc.update(|s| s.message_sound = !s.message_sound));
        // 打开时试听一下
        with_app(|app| {
            if app.node.settings().message_sound {
                crate::sound::message();
            }
        });
    });
    w.on_run_fix(|id, dry| {
        ctl(move |sc| sc.run_fix(id.to_string(), dry));
    });
}

/// 测试：在设置窗口里点这个修复（调用方保证 LANCAT_FIX_DRY_RUN=1，只出预览）
pub fn test_fix(fix_id: String) {
    slint::Timer::single_shot(Duration::from_millis(0), move || ctl(move |sc| sc.run_fix(fix_id, false)));
}

/// 关于页的加粗段落：行内混排 + 断行（近似 UAX#14：汉字之间可断；空格后可断；URL 在「/」后接字母数字处可断；
/// 闭合标点不放行首、开括号不放行尾）。每段的宽度由 Slint 量好报回来，宽度变化时重排。
mod flow {
    use crate::{FlowTok, SettingsWindow};
    use slint::{ComponentHandle, Model, ModelRc, VecModel};
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Tok {
        t: String,
        bold: bool,
        /// 这段之前可以断行
        brk: bool,
        space: bool,
    }

    #[derive(Default)]
    struct Flow {
        toks: Vec<Tok>,
        widths: Vec<f32>,
        width: f32,
        model: Option<Rc<VecModel<FlowTok>>>,
        win: Option<slint::Weak<SettingsWindow>>,
        queued: bool,
    }

    thread_local! {
        static FLOW: RefCell<Flow> = RefCell::new(Flow::default());
    }

    fn cjk(c: char) -> bool {
        matches!(c as u32, 0x2E80..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
    }
    fn closing(c: char) -> bool {
        "，。、）」』：；！？】》,.;:!?)".contains(c)
    }
    fn opening(c: char) -> bool {
        "（「『【《(".contains(c)
    }

    fn tokenize(segs: &[(&str, bool)]) -> Vec<Tok> {
        let mut out: Vec<Tok> = Vec::new();
        for (s, b) in segs {
            let cs: Vec<char> = s.chars().collect();
            let mut i = 0;
            while i < cs.len() {
                let c = cs[i];
                if c == ' ' {
                    out.push(Tok { t: " ".into(), bold: *b, brk: false, space: true });
                    i += 1;
                    continue;
                }
                let prev_open = out.last().and_then(|t| t.t.chars().last()).is_some_and(opening);
                let prev_space = out.last().is_some_and(|t| t.space);
                let prev_cjk = out.last().and_then(|t| t.t.chars().last()).is_some_and(cjk);
                if cjk(c) {
                    let brk = !out.is_empty() && !closing(c) && !prev_open;
                    out.push(Tok { t: c.to_string(), bold: *b, brk, space: false });
                    i += 1;
                    continue;
                }
                // 西文：到空格 / 汉字为止；「/」后面接字母数字处切开（可断）
                let mut j = i;
                let mut t = String::new();
                while j < cs.len() && cs[j] != ' ' && !cjk(cs[j]) {
                    t.push(cs[j]);
                    j += 1;
                    // Chromium（Windows）在「/」后可断；旧版 Linux（WebKitGTK）实测整段网址不断开
                    if cfg!(not(target_os = "linux")) && cs[j - 1] == '/' && j < cs.len() && cs[j].is_alphanumeric() {
                        break;
                    }
                }
                let continues = out.last().is_some_and(|p| !p.space && !p.t.chars().last().is_some_and(cjk) && !p.t.ends_with('/'));
                let brk = !out.is_empty() && !closing(c) && !prev_open && (prev_space || prev_cjk || !continues);
                out.push(Tok { t, bold: *b, brk, space: false });
                i = j;
            }
        }
        out
    }

    pub fn install(win: &SettingsWindow, segs: &[(&str, bool)]) {
        let toks = tokenize(segs);
        let model = Rc::new(VecModel::from(toks.iter().map(|t| FlowTok { t: t.t.clone().into(), bold: t.bold, x: -10000.0, y: 0.0 }).collect::<Vec<_>>()));
        win.set_about_toks(ModelRc::from(model.clone()));
        let n = toks.len();
        FLOW.with(|f| {
            *f.borrow_mut() = Flow { toks, widths: vec![-1.0; n], width: 0.0, model: Some(model), win: Some(win.as_weak()), queued: false };
        });
        win.on_about_measured(|i, w| {
            FLOW.with(|f| {
                if let Some(x) = f.borrow_mut().widths.get_mut(i as usize) {
                    *x = w;
                }
            });
            queue();
        });
        win.on_about_width(|w| {
            FLOW.with(|f| f.borrow_mut().width = w);
            queue();
        });
    }

    fn queue() {
        let go = FLOW.with(|f| !std::mem::replace(&mut f.borrow_mut().queued, true));
        if go {
            slint::Timer::single_shot(std::time::Duration::ZERO, layout);
        }
    }

    fn layout() {
        FLOW.with(|f| {
            let mut f = f.borrow_mut();
            f.queued = false;
            if f.width <= 1.0 || f.widths.iter().any(|w| *w < 0.0) {
                return;
            }
            // 与 FlowNote.fs 相同（Linux 12px，Windows 11.5px）
            let lh = crate::metrics::line_height(if cfg!(target_os = "linux") { 12.0 } else { 11.5 }, 1.5);
            let maxw = f.width;
            let mut pos = vec![(0.0f32, 0.0f32); f.toks.len()];
            let (mut x, mut line) = (0.0f32, 0usize);
            let mut i = 0;
            while i < f.toks.len() {
                // 一个不可断的「词」：i..j（不含尾随空格），尾随空格 j..k
                let mut j = i + 1;
                while j < f.toks.len() && !f.toks[j].brk && !f.toks[j].space {
                    j += 1;
                }
                let mut k = j;
                while k < f.toks.len() && f.toks[k].space {
                    k += 1;
                }
                let w: f32 = f.widths[i..j].iter().sum();
                if x > 0.0 && x + w > maxw + 0.5 {
                    line += 1;
                    x = 0.0;
                }
                for (m, p) in pos.iter_mut().enumerate().take(k).skip(i) {
                    *p = (x, line as f32 * lh);
                    x += f.widths[m];
                }
                i = k;
            }
            if let Some(model) = f.model.as_ref() {
                for (m, (px, py)) in pos.iter().enumerate() {
                    if let Some(mut r) = model.row_data(m) {
                        if r.x != *px || r.y != *py {
                            r.x = *px;
                            r.y = *py;
                            model.set_row_data(m, r);
                        }
                    }
                }
            }
            if let Some(w) = f.win.as_ref().and_then(|w| w.upgrade()) {
                w.set_about_lines(line as i32 + 1);
            }
        });
    }
}

/// 旧版 .path 是 `direction: rtl`（为了从开头省略）：整串是 LTR 文字、开头的「/」是中性字符，按段落方向 RTL 排到了
/// 最右边 —— 旧版显示成 "home/motto/…/recv/"。Windows 路径以盘符开头，不受影响。
fn rtl_shown(p: &str) -> String {
    match p.strip_prefix('/') {
        Some(rest) => format!("{rest}/"),
        None => p.to_string(),
    }
}
