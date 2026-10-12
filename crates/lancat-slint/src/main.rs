//! Lancat 面板（Slint 版）：直接起 lancat-core 节点，功能与旧版 Svelte 面板一致（见 docs/slint/inventory.md）。
//! Windows 用自写 Win32 Platform（软件渲染 → 预乘 BGRA DIB，Acrylic + DWM 阴影 + 淡入）+ 停靠；
//! Linux 用自写 SCTK layer-shell 后端（LANCAT_SLINT_WL=1）或 winit。
//!
//! 测试隔离（硬性规则）：默认数据目录 `<data>/lancat-slint-dev`、端口 47840、关闭 mDNS/UDP 发现，
//! 所以不会碰到局域网里的真实设备；对端只能用 LANCAT_PROBE / 手动地址指定本机另一实例。
//! `LANCAT_SLINT_DISCOVERY=1` 才打开发现（正式替换旧版之前不要用）。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

slint::include_modules!();

#[cfg(windows)]
mod diag;
#[cfg(windows)]
mod dock;
#[cfg(target_os = "linux")]
mod dock_wl;
mod emoji;
mod emoji_catalog;
#[cfg(windows)]
mod filedlg;
mod icons;
mod lottie;
mod settings_ui;
#[cfg(windows)]
mod win;
#[cfg(target_os = "linux")]
mod wl;

use lancat_core::{CoreEvent, EmojiPayload, Message, Node, NodeConfig, PeerView, Settings};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

pub(crate) fn t0() -> Instant {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *T0.get_or_init(Instant::now)
}

/// 实时传输进度；rate（字节/秒）按 ≥0.5 s 窗口平滑（与旧版 state.svelte.ts 相同）
#[derive(Clone, Copy)]
struct Prog {
    done: u64,
    total: u64,
    rate: f64,
    at: Instant,
    base: u64,
}

pub(crate) struct App {
    ui: slint::Weak<PanelWindow>,
    node: Arc<Node>,
    rt: tokio::runtime::Handle,
    peers: Vec<PeerView>,
    peer_model: Rc<VecModel<PeerItem>>,
    msgs: Vec<Message>,
    has_more: bool,
    loading_older: bool,
    msg_model: Rc<VecModel<MsgItem>>,
    current: String,
    broadcast: bool,
    selected: Vec<String>,
    /// peer → 「正在输入」到期时间
    typing: HashMap<String, Instant>,
    typing_timer: slint::Timer,
    progress: HashMap<String, Prog>,
    last_typing_sent: Option<Instant>,
    typing_peer: Option<String>,
    refreshing_until: Option<Instant>,
    /// 原生文件对话框开着：不收起
    dialog_open: bool,
    visible: bool,
    /// 会话滚动在底部附近：新消息跟随到底
    stick: bool,
    toast_timer: slint::Timer,
    pub(crate) emoji: emoji::EmojiState,
    pub(crate) settings: Option<settings_ui::SettingsCtl>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub(crate) fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    // try_borrow_mut：停靠回调可能在 App 方法里同步回到这里，嵌套那次跳过而不是 panic
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

/// Run `f` with the app on the UI thread (from any thread).
pub(crate) fn post(f: impl FnOnce(&mut App) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        with_app(f);
    });
}

// ------------------------------------------------------------------ helpers (lib/api.ts)

fn hsl(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    (r + m, g + m, b + m)
}

/// avatarColor：hsl(hues[h % 9] 70% 55%)，渐变顶部 color-mix(c 82%, white)
pub(crate) fn avatar_colors(id: &str, gray: bool) -> (slint::Color, slint::Color) {
    let h = id.encode_utf16().fold(0u32, |h, c| h.wrapping_mul(31).wrapping_add(c as u32));
    const HUES: [f32; 9] = [211.0, 262.0, 330.0, 14.0, 32.0, 145.0, 175.0, 195.0, 290.0];
    let (mut r, mut g, mut b) = hsl(HUES[(h % 9) as usize], 0.7, 0.55);
    if gray {
        // .blocked：filter: grayscale(1)
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        (r, g, b) = (y, y, y);
    }
    let c2 = slint::Color::from_rgb_f32(r, g, b);
    let c1 = slint::Color::from_rgb_f32(r * 0.82 + 0.18, g * 0.82 + 0.18, b * 0.82 + 0.18);
    (c1, c2)
}

pub(crate) fn initials(name: &str) -> String {
    let n = name.trim();
    if n.is_empty() {
        return "?".into();
    }
    if let Some(c) = n.chars().find(|c| ('\u{3400}'..='\u{9fff}').contains(c)) {
        return c.to_string();
    }
    let parts: Vec<&str> = n.split(|c: char| c.is_whitespace() || "-_.".contains(c)).filter(|s| !s.is_empty()).collect();
    if parts.len() >= 2 {
        let a = parts[0].chars().next().unwrap();
        let b = parts[1].chars().next().unwrap();
        return format!("{a}{b}").to_uppercase();
    }
    n.chars().take(2).collect::<String>().to_uppercase()
}

pub(crate) fn os_glyph(os: &str) -> String {
    match os {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        "android" => "Android",
        "ios" => "iOS",
        o => o,
    }
    .to_string()
}

/// formatSize
pub(crate) fn human(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let u = ["KB", "MB", "GB", "TB"];
    let mut v = n as f64 / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < u.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", u[i])
    } else {
        format!("{} {}", v.round(), u[i])
    }
}

/// formatTime：今天 HH:MM / 昨天 HH:MM / M月D日 HH:MM（本地时区）
pub(crate) fn format_time(ts: i64) -> String {
    use chrono::{Datelike, Local, TimeZone};
    let Some(d) = Local.timestamp_millis_opt(ts).single() else { return String::new() };
    let now = Local::now();
    let hm = d.format("%H:%M").to_string();
    if d.date_naive() == now.date_naive() {
        return hm;
    }
    if Some(d.date_naive()) == now.date_naive().pred_opt() {
        return format!("昨天 {hm}");
    }
    format!("{}月{}日 {hm}", d.month(), d.day())
}

fn failure_info(code: &str) -> Option<(&'static str, &'static str, bool)> {
    Some(match code {
        "peer_offline" => ("对方不在线", "等对方上线后点重试，会从断点继续", false),
        "peer_inbound_blocked" => ("对方的防火墙拦截了连接", "在对方电脑上打开 Lancat 设置 →「权限与诊断」点修复（或把对方的网络设为专用），然后重试", false),
        "peer_outdated" => ("对方版本较旧且入站被拦", "请对方更新 Lancat，或在对方电脑上放行 Lancat 的防火墙，然后重试", false),
        "peer_no_response" => ("对方长时间没有回应", "对方可能休眠了或网络断开，恢复后点重试", false),
        "link_lost" => ("连接中断", "网络恢复后点重试，会从断点继续", false),
        "peer_dir_unwritable" => ("对方无法保存文件", "对方的接收目录不可写或磁盘已满，请对方在设置里换一个接收目录", false),
        "peer_too_large" => ("超过对方的单文件上限", "请对方在设置 →「接收」里调大单文件大小上限", false),
        "peer_rejected" => ("对方拒绝接收", "", false),
        "local_dir_unwritable" => ("本机接收目录不可写", "换一个可写的接收目录（或恢复默认的下载目录），对方重试即可", true),
        "source_missing" => ("源文件已不存在", "文件被移动或删除了，请重新拖进来发送", false),
        "source_unreadable" => ("读取源文件失败", "文件可能正被其他程序占用，稍后重试", false),
        "verify" => ("文件校验失败", "点重试重新传输", false),
        "network" => ("网络错误", "检查网络后点重试", false),
        _ => return None,
    })
}

fn file_icon(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    let n = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "bmp" | "svg" | "avif" => "image",
        "mp4" | "mov" | "mkv" | "avi" | "webm" => "film",
        "mp3" | "flac" | "wav" | "m4a" | "ogg" | "aac" => "music",
        "zip" | "7z" | "rar" | "tar" | "gz" | "xz" | "dmg" | "iso" => "archive",
        _ => "doc",
    };
    icons::icon(n)
}

fn s(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn file_item(m: &Message, it: &mut MsgItem, prog: Option<&Prog>) {
    let name = s(&m.body, "name");
    let size = m.body.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
    let code = s(&m.body, "code");
    let err = s(&m.body, "error");
    let failure = if m.status == "failed" { failure_info(&code) } else { None };
    let active = m.status == "transferring" || m.status == "receiving";
    let pct = match prog {
        Some(p) if p.total > 0 => p.done as f64 / p.total as f64,
        _ if m.status == "done" => 1.0,
        _ => 0.0,
    };
    it.kind = "file".into();
    it.ficon = file_icon(&name).into();
    it.fname = name.into();
    let mut meta = human(size);
    let show_bar = active || (m.status == "paused" && prog.is_some());
    if show_bar {
        meta += &format!(" · {}%", (pct * 100.0).floor() as u32);
    }
    meta += " · ";
    it.fmeta = meta.into();
    let speed = match prog {
        Some(p) if active && p.rate >= 1.0 => {
            let left = p.total.saturating_sub(p.done) as f64 / p.rate;
            let eta = if left < 1.0 {
                String::new()
            } else if left < 60.0 {
                format!(" · 剩 {} 秒", left.ceil())
            } else if left < 3600.0 {
                format!(" · 剩 {} 分", (left / 60.0).ceil())
            } else {
                format!(" · 剩 {:.1} 小时", left / 3600.0)
            };
            format!("{}/s{eta}", human(p.rate as u64))
        }
        _ => String::new(),
    };
    let label = match m.status.as_str() {
        "transferring" => if m.outgoing { "发送中".to_string() } else { "接收中".to_string() },
        "receiving" => "接收中".into(),
        "done" => if m.outgoing { "已送达".to_string() } else { "已保存".to_string() },
        "paused" => "已暂停".into(),
        "cancelled" => if err.is_empty() { "已取消".into() } else { format!("已取消：{err}") },
        "failed" => match failure {
            Some((r, _, _)) => format!("失败：{r}"),
            None if !err.is_empty() => format!("失败：{err}"),
            None => "失败".into(),
        },
        _ => String::new(),
    };
    it.fst = if speed.is_empty() { label.into() } else { speed.into() };
    it.failed = m.status == "failed";
    it.cancelled = m.status == "cancelled";
    if let Some((_, hint, local)) = failure {
        it.why = hint.into();
        it.fixlink = local;
    }
    it.progress = if show_bar { pct as f32 } else { -1.0 };
    it.paused = m.status == "paused";
    let st = m.status.as_str();
    it.act1 = if m.outgoing && st == "transferring" {
        "pause"
    } else if m.outgoing && st == "paused" {
        "play"
    } else if m.outgoing && st == "failed" {
        "retry"
    } else {
        ""
    }
    .into();
    it.act2 = if (m.outgoing && ["transferring", "paused", "failed"].contains(&st)) || (!m.outgoing && ["receiving", "failed"].contains(&st)) {
        "cancel"
    } else if st == "done" && !s(&m.body, "path").is_empty() {
        "reveal"
    } else {
        ""
    }
    .into();
}

impl App {
    fn ui(&self) -> Option<PanelWindow> {
        self.ui.upgrade()
    }

    pub(crate) fn flash(&mut self, msg: impl Into<SharedString>) {
        let Some(ui) = self.ui() else { return };
        ui.set_toast(msg.into());
        let w = ui.as_weak();
        // one timer for all toasts: each flash restarts the 2.6 s
        self.toast_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(2600), move || {
            if let Some(ui) = w.upgrade() {
                ui.set_toast("".into());
            }
        });
    }

    fn online_peers(&self) -> Vec<&PeerView> {
        self.peers.iter().filter(|p| p.online && !p.blocked).collect()
    }

    /// 输入栏消息发给谁（targets()）
    fn targets(&self) -> Vec<String> {
        if self.broadcast {
            return self.online_peers().iter().map(|p| p.id.clone()).collect();
        }
        if self.selected.len() > 1 {
            return self.selected.clone();
        }
        if self.current.is_empty() {
            vec![]
        } else {
            vec![self.current.clone()]
        }
    }

    fn peer_name(&self, id: &str) -> String {
        self.peers.iter().find(|p| p.id == id).map(|p| p.name.clone()).unwrap_or_else(|| "?".into())
    }

    fn rebuild_peers(&mut self) {
        self.peers.sort_by(|a, b| b.online.cmp(&a.online).then(b.last_ts.cmp(&a.last_ts)).then(a.name.cmp(&b.name)));
        let items: Vec<PeerItem> = self
            .peers
            .iter()
            .map(|p| {
                let (c1, c2) = avatar_colors(&p.id, p.blocked);
                PeerItem {
                    id: p.id.clone().into(),
                    name: p.name.clone().into(),
                    initials: initials(&p.name).into(),
                    c1,
                    c2,
                    online: p.online,
                    unread: p.unread as i32,
                    blocked: p.blocked,
                    picked: self.selected.contains(&p.id),
                    os: os_glyph(&p.os).into(),
                }
            })
            .collect();
        self.peer_model.set_vec(items);
        if let Some(ui) = self.ui() {
            ui.set_no_peers(self.peers.is_empty());
            ui.set_online_count(self.online_peers().len() as i32);
        }
        self.update_header();
        #[cfg(windows)]
        dock::refresh_visual();
    }

    fn update_header(&mut self) {
        let Some(ui) = self.ui() else { return };
        let now = Instant::now();
        let (title, sub) = if self.broadcast {
            ("全部设备".to_string(), format!("群发给 {} 台在线设备", self.online_peers().len()))
        } else if self.selected.len() > 1 {
            (format!("已选 {} 台设备", self.selected.len()), self.selected.iter().map(|id| self.peer_name(id)).collect::<Vec<_>>().join("、"))
        } else if let Some(p) = self.peers.iter().find(|p| p.id == self.current) {
            let typing = self.typing.get(&p.id).is_some_and(|t| *t > now);
            let sub = if typing {
                "正在输入…".to_string()
            } else if p.blocked {
                "已拉黑".into()
            } else if p.online {
                match &p.via {
                    Some(v) => format!("在线 · {} · 经 {v} 中转", os_glyph(&p.os)),
                    None => format!("在线 · {} · {}", os_glyph(&p.os), p.addr),
                }
            } else {
                "离线".into()
            };
            (p.name.clone(), sub)
        } else {
            ("Lancat".to_string(), format!("本机：{}", self.node.me().name))
        };
        ui.set_subtitle_accent(sub == "正在输入…");
        ui.set_header_title(title.into());
        ui.set_subtitle(sub.into());
        ui.set_broadcast(self.broadcast);
        ui.set_current_id(self.current.clone().into());
        // composer placeholder / disabled
        let t = self.targets();
        ui.set_can_send(!t.is_empty());
        ui.set_placeholder(
            if t.is_empty() {
                "没有可发送的对象".to_string()
            } else if t.len() > 1 {
                format!("发送给 {} 台设备…", t.len())
            } else {
                format!("发给 {}…", self.peer_name(&t[0]))
            }
            .into(),
        );
        // typing expires at a known time: re-render exactly then
        if let Some(until) = self.typing.get(&self.current).copied().filter(|t| *t > now) {
            let w = self.ui.clone();
            self.typing_timer.start(slint::TimerMode::SingleShot, until - now + Duration::from_millis(20), move || {
                let _ = w;
                with_app(|app| app.update_header());
            });
        }
    }

    pub(crate) fn open(&mut self, id: &str) {
        self.current = id.to_string();
        self.broadcast = false;
        self.update_header();
        self.load_messages();
        self.mark_read_if_visible();
    }

    fn mark_read_if_visible(&mut self) {
        if self.current.is_empty() || !self.visible {
            return;
        }
        if let Some(p) = self.peers.iter_mut().find(|p| p.id == self.current) {
            if p.unread > 0 {
                p.unread = 0;
                let _ = self.node.mark_read(&self.current);
                self.rebuild_peers();
            }
        }
    }

    fn load_messages(&mut self) {
        self.msgs = if self.current.is_empty() {
            Vec::new()
        } else {
            let mut v = self.node.messages(&self.current, None, 80).unwrap_or_default();
            v.sort_by_key(|m| m.ts);
            v
        };
        self.has_more = self.msgs.len() >= 80;
        self.emoji.stop_chat();
        self.rebuild_messages(true);
        // newest stickers play twice (旧版：可见的贴纸播 2 遍)
        let ids: Vec<String> = self.msgs.iter().rev().take(4).filter(|m| self.sticker_of(m).is_some()).map(|m| m.id.clone()).collect();
        for id in ids {
            self.play_sticker(&id, false);
        }
    }

    pub(crate) fn sticker_of(&self, m: &Message) -> Option<EmojiPayload> {
        if m.kind == "emoji" {
            return serde_json::from_value(m.body.clone()).ok();
        }
        if m.kind == "text" {
            let t = s(&m.body, "text");
            let t = t.trim();
            if t.chars().count() <= 8 {
                if let Some(e) = emoji::by_char(t) {
                    return Some(EmojiPayload { kind: "noto".into(), code: e.code.into(), alt: e.ch.into(), data: None });
                }
            }
        }
        None
    }

    fn item(&mut self, i: usize, sep: Option<String>, first: bool) -> MsgItem {
        let m = &self.msgs[i];
        let mut it = MsgItem { id: m.id.clone().into(), outgoing: m.outgoing, status: m.status.clone().into(), first, progress: -1.0, ..Default::default() };
        it.sep = sep.unwrap_or_default().into();
        if m.kind == "file" {
            let p = self.progress.get(&m.id).copied();
            file_item(m, &mut it, p.as_ref());
        } else if let Some(e) = self.sticker_of(m) {
            it.kind = "sticker".into();
            it.text = if e.alt.is_empty() { "🙂".into() } else { e.alt.clone().into() };
            let scale = self.scale();
            if let Some(img) = self.emoji.poster(&e, (160.0 * scale) as u32) {
                it.sticker = img;
                it.has_sticker = true;
            }
        } else {
            it.kind = "text".into();
            it.text = if m.kind == "text" { s(&m.body, "text").into() } else { format!("[{}]", m.kind).into() };
        }
        it
    }

    pub(crate) fn scale(&self) -> f32 {
        self.ui().map(|u| u.window().scale_factor()).unwrap_or(1.0)
    }

    fn rebuild_messages(&mut self, to_end: bool) {
        let mut items = Vec::with_capacity(self.msgs.len());
        for i in 0..self.msgs.len() {
            let (sep, first) = self.row_meta(i);
            items.push(self.item(i, sep, first));
        }
        self.msg_model.set_vec(items);
        if to_end {
            self.scroll_to_end();
        }
    }

    fn row_meta(&self, i: usize) -> (Option<String>, bool) {
        let m = &self.msgs[i];
        let prev = if i > 0 { Some(&self.msgs[i - 1]) } else { None };
        let sep = match prev {
            None => Some(format_time(m.ts)),
            Some(p) if m.ts - p.ts > 5 * 60_000 => Some(format_time(m.ts)),
            _ => None,
        };
        let first = prev.map_or(true, |p| p.outgoing != m.outgoing || m.ts - p.ts > 60_000);
        (sep, first)
    }

    fn scroll_to_end(&self) {
        if let Some(ui) = self.ui() {
            let w = ui.as_weak();
            // after layout: scroll to the newest message
            slint::Timer::single_shot(Duration::from_millis(0), move || {
                if let Some(ui) = w.upgrade() {
                    ui.invoke_scroll_to_end();
                }
            });
        }
    }

    pub(crate) fn refresh_row_pub(&mut self, i: usize) {
        self.refresh_row(i);
    }

    pub(crate) fn emoji_opened_pub(&mut self, v: bool) {
        self.emoji.opened_pub(v);
    }

    pub(crate) fn dialog_open_pub(&mut self, v: bool) {
        self.dialog_open = v;
    }

    fn refresh_row(&mut self, i: usize) {
        let (sep, first) = self.row_meta(i);
        let it = self.item(i, sep, first);
        if i < self.msg_model.row_count() {
            self.msg_model.set_row_data(i, it);
        }
    }

    fn upsert_message(&mut self, m: Message, live: bool) {
        if m.peer_id != self.current {
            return;
        }
        if let Some(i) = self.msgs.iter().position(|x| x.id == m.id) {
            self.msgs[i] = m;
            self.refresh_row(i);
        } else {
            let id = m.id.clone();
            let outgoing = m.outgoing;
            let mut j = self.msgs.len();
            while j > 0 && self.msgs[j - 1].ts > m.ts {
                j -= 1;
            }
            self.msgs.insert(j, m);
            let at_end = j + 1 == self.msgs.len();
            if at_end {
                let (sep, first) = self.row_meta(j);
                let it = self.item(j, sep, first);
                self.msg_model.push(it);
                if self.stick || outgoing {
                    self.scroll_to_end();
                }
            } else {
                self.rebuild_messages(false);
            }
            if live && self.sticker_of(&self.msgs[j]).is_some() {
                self.play_sticker(&id, false);
            }
            self.mark_read_if_visible();
        }
    }

    pub(crate) fn play_sticker(&mut self, msg_id: &str, hover: bool) {
        let Some(m) = self.msgs.iter().find(|m| m.id == msg_id) else { return };
        let Some(e) = self.sticker_of(m) else { return };
        let px = (160.0 * self.scale()) as u32;
        if self.visible {
            self.emoji.play_chat(msg_id, &e, px, hover);
        }
    }

    /// 动画帧写回消息行
    pub(crate) fn set_sticker_frame(&mut self, msg_id: &str, img: slint::Image) {
        if let Some(i) = self.msgs.iter().position(|m| m.id == msg_id) {
            if let Some(mut r) = self.msg_model.row_data(i) {
                r.sticker = img;
                r.has_sticker = true;
                self.msg_model.set_row_data(i, r);
            }
        }
    }

    fn load_older(&mut self) {
        if !self.has_more || self.loading_older || self.current.is_empty() || self.msgs.is_empty() {
            return;
        }
        self.loading_older = true;
        let older = self.node.messages(&self.current, Some(self.msgs[0].ts), 80).unwrap_or_default();
        self.has_more = older.len() >= 80;
        if !older.is_empty() {
            let before = self.ui().map(|u| u.get_list_content_h()).unwrap_or_default();
            let mut older = older;
            older.sort_by_key(|m| m.ts);
            older.retain(|o| !self.msgs.iter().any(|m| m.id == o.id));
            older.extend(self.msgs.drain(..));
            self.msgs = older;
            self.rebuild_messages(false);
            // keep the position after prepending
            let w = self.ui.clone();
            slint::Timer::single_shot(Duration::from_millis(0), move || {
                if let Some(ui) = w.upgrade() {
                    let after = ui.get_list_content_h();
                    ui.invoke_keep_scroll(after - before);
                }
                with_app(|app| app.loading_older = false);
            });
        } else {
            self.loading_older = false;
        }
    }

    fn on_core(&mut self, ev: CoreEvent) {
        match ev {
            CoreEvent::Peer { peer } => {
                let id = peer.id.clone();
                match self.peers.iter_mut().find(|p| p.id == peer.id) {
                    Some(p) => *p = peer,
                    None => self.peers.push(peer),
                }
                self.rebuild_peers();
                if self.current.is_empty() {
                    self.open(&id);
                } else if id == self.current {
                    self.mark_read_if_visible();
                }
                if let Some(sc) = self.settings.as_mut() {
                    sc.peers_changed(&self.peers);
                }
            }
            CoreEvent::PeerRemoved { peer_id } => {
                self.peers.retain(|p| p.id != peer_id);
                self.selected.retain(|x| *x != peer_id);
                if self.current == peer_id {
                    self.current = self.peers.first().map(|p| p.id.clone()).unwrap_or_default();
                    self.load_messages();
                }
                self.rebuild_peers();
                if let Some(sc) = self.settings.as_mut() {
                    sc.peers_changed(&self.peers);
                }
            }
            CoreEvent::Message { message } => {
                #[cfg(windows)]
                dock::on_incoming(&message);
                if !message.outgoing {
                    self.typing.remove(&message.peer_id);
                    self.update_header();
                }
                self.upsert_message(message, true);
                #[cfg(windows)]
                dock::refresh_visual();
            }
            CoreEvent::MessageUpdate { message } => {
                if message.status == "done" || message.status == "failed" {
                    self.progress.remove(&message.id);
                }
                self.upsert_message(message, false);
            }
            CoreEvent::Typing { peer_id, typing } => {
                if typing {
                    self.typing.insert(peer_id, Instant::now() + Duration::from_secs(5));
                } else {
                    self.typing.remove(&peer_id);
                }
                self.update_header();
            }
            CoreEvent::Progress { msg_id, done, total, .. } => {
                let now = Instant::now();
                let p = match self.progress.get(&msg_id).copied() {
                    Some(p) if done >= p.base => {
                        let mut p = Prog { done, total, ..p };
                        let dt = now.duration_since(p.at).as_secs_f64();
                        if dt >= 0.5 {
                            let inst = (done - p.base) as f64 / dt;
                            p.rate = if p.rate > 0.0 { p.rate * 0.6 + inst * 0.4 } else { inst };
                            p.at = now;
                            p.base = done;
                        }
                        p
                    }
                    old => Prog { done, total, rate: old.map(|p| p.rate).unwrap_or(0.0), at: now, base: done },
                };
                self.progress.insert(msg_id.clone(), p);
                if let Some(i) = self.msgs.iter().position(|x| x.id == msg_id) {
                    self.refresh_row(i);
                }
            }
            CoreEvent::HistoryCleared { peer_id } => {
                for p in self.peers.iter_mut() {
                    if peer_id.as_ref().is_none_or(|id| *id == p.id) {
                        p.unread = 0;
                        p.last_preview.clear();
                    }
                }
                self.rebuild_peers();
                if peer_id.as_ref().is_none_or(|id| *id == self.current) {
                    self.load_messages();
                }
            }
            CoreEvent::Settings { settings } => {
                self.apply_theme(&settings);
                self.emoji.settings_changed(&settings);
                if let Some(sc) = self.settings.as_mut() {
                    sc.settings_changed(&settings);
                }
                #[cfg(windows)]
                dock::apply_layout();
            }
            CoreEvent::Alert { kind, report, detail } => {
                #[cfg(windows)]
                diag::on_core_alert(&kind, report.as_ref(), &detail);
                #[cfg(not(windows))]
                let _ = (kind, report, detail);
            }
        }
    }

    pub(crate) fn apply_theme(&self, settings: &Settings) {
        // LANCAT_SLINT_THEME=light|dark：测试用，覆盖设置里的主题
        let dark = theme_is_dark(&env("LANCAT_SLINT_THEME").unwrap_or(settings.theme.clone()));
        if let Some(ui) = self.ui() {
            if ui.global::<Theme>().get_dark() != dark {
                ui.global::<Theme>().set_dark(dark);
            }
            #[cfg(windows)]
            win::refresh_material(&ui, dark);
        }
        if let Some(sc) = self.settings.as_ref() {
            sc.set_dark(dark);
        }
    }

    fn send_text(&mut self, text: String) {
        let text = text.trim().to_string();
        let targets = self.targets();
        if text.is_empty() || targets.is_empty() {
            return;
        }
        self.stop_typing();
        let n = targets.len();
        for id in targets {
            let node = self.node.clone();
            let text = text.clone();
            self.rt.spawn(async move {
                match node.send_text(&id, &text).await {
                    Ok(m) => post(move |app| app.upsert_message(m, true)),
                    Err(e) => {
                        let e = e.to_string();
                        log::warn!("send_text to {id} failed: {e}");
                        post(move |app| app.flash(e));
                    }
                }
            });
        }
        if n > 1 {
            self.flash(format!("已发给 {n} 台设备"));
        }
    }

    pub(crate) fn send_emoji(&mut self, e: EmojiPayload) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        for id in targets {
            let node = self.node.clone();
            let e = e.clone();
            self.rt.spawn(async move {
                match node.send_emoji(&id, e).await {
                    Ok(m) => post(move |app| app.upsert_message(m, true)),
                    Err(err) => {
                        let err = err.to_string();
                        post(move |app| app.flash(err));
                    }
                }
            });
        }
    }

    fn stop_typing(&mut self) {
        if let Some(p) = self.typing_peer.take() {
            self.node.send_typing(&p, false);
        }
        self.last_typing_sent = None;
    }

    fn on_typing(&mut self) {
        #[cfg(windows)]
        dock::note_typed();
        let t = self.targets();
        if t.len() == 1 && self.last_typing_sent.is_none_or(|x| x.elapsed() > Duration::from_millis(2500)) {
            self.last_typing_sent = Some(Instant::now());
            self.typing_peer = Some(t[0].clone());
            self.node.send_typing(&t[0], true);
        }
    }

    /// 拖放 / 附件：targets 为空时提示
    pub(crate) fn send_files(&mut self, targets: Vec<String>, files: Vec<String>, open_after: bool) {
        if files.is_empty() {
            return;
        }
        if targets.is_empty() {
            self.flash("没有可发送的在线设备");
            return;
        }
        let names: Vec<String> = targets.iter().map(|t| self.peer_name(t)).collect();
        let msg = format!("已发送 {} 项给 {}", files.len(), if names.len() > 2 { format!("{} 台设备", names.len()) } else { names.join("、") });
        let node = self.node.clone();
        let paths: Vec<std::path::PathBuf> = files.into_iter().map(Into::into).collect();
        if open_after && targets.len() == 1 && targets[0] != self.current {
            let t = targets[0].clone();
            self.open(&t);
        }
        self.rt.spawn(async move {
            match node.send_files(&targets, &paths).await {
                Ok(ms) => post(move |app| {
                    for m in ms {
                        app.upsert_message(m, true);
                    }
                    app.flash(msg);
                }),
                Err(e) => {
                    let e = e.to_string();
                    log::warn!("send_files failed: {e}");
                    post(move |app| app.flash(e));
                }
            }
        });
    }

    fn file_action(&mut self, id: String, act: String) {
        let node = self.node.clone();
        match act.as_str() {
            "pause" => {
                if let Err(e) = node.pause_transfer(&id) {
                    self.flash(e.to_string());
                }
            }
            "cancel" => {
                if let Err(e) = node.cancel_transfer(&id) {
                    self.flash(e.to_string());
                }
            }
            "play" | "retry" => {
                self.rt.spawn(async move {
                    if let Err(e) = node.retry_transfer(&id).await {
                        let e = e.to_string();
                        post(move |app| app.flash(e));
                    }
                });
            }
            "reveal" => {
                if let Some(m) = self.msgs.iter().find(|m| m.id == id) {
                    let p = s(&m.body, "path");
                    reveal(&p);
                }
            }
            _ => {}
        }
    }

    fn attach(&mut self, directory: bool) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        self.dialog_open = true;
        #[cfg(windows)]
        filedlg::pick(directory, move |paths| {
            with_app(move |app| {
                app.dialog_open = false;
                if !paths.is_empty() {
                    let n = targets.len();
                    app.send_files(targets, paths, false);
                    if n > 1 {
                        app.flash(format!("已发送给 {n} 台设备"));
                    }
                }
            });
        });
        #[cfg(not(windows))]
        {
            let _ = (directory, targets);
            self.dialog_open = false;
        }
    }

    fn peer_menu_action(&mut self, act: &str) {
        let Some(ui) = self.ui() else { return };
        let id = ui.get_menu_peer().to_string();
        ui.set_menu_peer("".into());
        let Some(p) = self.peers.iter().find(|p| p.id == id) else { return };
        let r = match act {
            "block" => self.node.set_blocked(&id, !p.blocked),
            "clear" => self.node.clear_history(Some(&id)),
            "forget" => self.node.forget_peer(&id),
            _ => Ok(()),
        };
        if let Err(e) = r {
            self.flash(e.to_string());
        }
    }

    /// 停靠打开面板：从 toast 打开时切到发消息的那个会话。
    #[allow(dead_code)]
    fn on_dock_open(&mut self, reason: &str) {
        if let Some(id) = reason.strip_prefix("toast:") {
            if self.peers.iter().any(|p| p.id == id) && self.current != id {
                let id = id.to_string();
                self.open(&id);
            }
        }
        self.set_visible(true);
        if let Some(ui) = self.ui() {
            if reason == "tray" || reason.starts_with("toast:") {
                ui.invoke_focus_input();
            }
            if reason == "drag" {
                ui.set_dragging(true);
            }
        }
    }

    pub(crate) fn set_visible(&mut self, v: bool) {
        if self.visible == v {
            return;
        }
        self.visible = v;
        if let Some(ui) = self.ui() {
            ui.set_panel_visible(v);
            if !v {
                // collapse closes popovers (旧版 collapse：emojiOpen/menuOpen = false)
                ui.set_emoji_open(false);
                ui.set_attach_open(false);
                ui.set_menu_peer("".into());
                ui.set_dragging(false);
            }
        }
        if v {
            self.mark_read_if_visible();
        } else {
            self.emoji.stop_all();
            self.stop_typing();
        }
    }

    // ---------------- drag & drop (DropPicker) ----------------

    fn drag_hint(&mut self, n: Option<usize>) {
        let Some(ui) = self.ui() else { return };
        match n {
            Some(n) => {
                let mut slots: Vec<DropSlot> = self
                    .online_peers()
                    .iter()
                    .map(|p| {
                        let (c1, c2) = avatar_colors(&p.id, false);
                        DropSlot { key: format!("peer:{}", p.id).into(), name: p.name.clone().into(), initials: initials(&p.name).into(), os: os_glyph(&p.os).into(), c1, c2, all: false }
                    })
                    .collect();
                if slots.len() > 1 {
                    let n = slots.len();
                    slots.push(DropSlot { key: "all".into(), name: "全部设备".into(), os: format!("{n} 台").into(), all: true, ..Default::default() });
                }
                ui.set_drop_slots(ModelRc::new(VecModel::from(slots)));
                ui.set_drag_count(n as i32);
                ui.set_dragging(true);
            }
            None => {
                ui.set_dragging(false);
                ui.set_drop_key("".into());
                ui.set_drop_focus_name("".into());
            }
        }
    }

    fn drop_hit(&self, x: f32, y: f32) -> String {
        let Some(ui) = self.ui() else { return String::new() };
        let size = ui.window().size().to_logical(ui.window().scale_factor());
        let (w, h) = (size.width, size.height);
        let slots = ui.get_drop_slots();
        let n = slots.row_count();
        if n > 0 {
            let x0 = (w - n as f32 * 72.0 - (n as f32 - 1.0) * 8.0) / 2.0;
            let y0 = h / 2.0 - 20.0;
            if y >= y0 && y <= y0 + 100.0 && x >= x0 {
                let i = ((x - x0) / 80.0) as usize;
                if i < n && x - x0 - i as f32 * 80.0 <= 72.0 {
                    return slots.row_data(i).map(|s| s.key.to_string()).unwrap_or_default();
                }
            }
        }
        if x > 66.0 && !self.broadcast && !self.current.is_empty() {
            return "conversation".into();
        }
        String::new()
    }

    fn drag_over(&mut self, x: f32, y: f32) {
        let key = self.drop_hit(x, y);
        let Some(ui) = self.ui() else { return };
        if ui.get_drop_key() == key.as_str() {
            return;
        }
        let name = if let Some(id) = key.strip_prefix("peer:") {
            self.peer_name(id)
        } else if key == "all" {
            "全部设备".into()
        } else {
            String::new()
        };
        ui.set_drop_focus_name(name.into());
        ui.set_drop_key(key.into());
    }

    fn drop_files(&mut self, files: Vec<String>, pos: Option<(f32, f32)>) {
        let key = match pos {
            Some((x, y)) => self.drop_hit(x, y),
            None => String::new(),
        };
        log::info!("drop: {} item(s) on {}", files.len(), if key.is_empty() { "nothing" } else { &key });
        // 停靠条上松手（pos = None）：发给当前目标
        let key = if pos.is_none() { "conversation".to_string() } else { key };
        if key.is_empty() {
            return;
        }
        let targets: Vec<String> = if key == "all" {
            self.online_peers().iter().map(|p| p.id.clone()).collect()
        } else if let Some(id) = key.strip_prefix("peer:") {
            if self.selected.len() > 1 && self.selected.iter().any(|x| x == id) {
                self.selected.clone()
            } else {
                vec![id.to_string()]
            }
        } else {
            self.targets()
        };
        let open_after = key != "conversation";
        self.send_files(targets, files, open_after);
    }
}

impl App {
    /// 诊断状态变化：齿轮红点 + 横幅 + 设置窗口的检查列表
    #[cfg(windows)]
    pub(crate) fn diag_status(&mut self, st: diag::DiagStatus, last: Option<diag::Diagnostics>) {
        if let Some(ui) = self.ui() {
            ui.set_problems(st.problems as i32);
            match &st.alert {
                Some(a) => {
                    ui.set_banner_title(a.title.clone().into());
                    ui.set_banner_detail(a.detail.clone().into());
                }
                None => {
                    ui.set_banner_title("".into());
                    ui.set_banner_detail("".into());
                }
            }
        }
        if let (Some(sc), Some(d)) = (self.settings.as_mut(), last.as_ref()) {
            sc.diag_changed(d);
        }
    }
}

fn reveal(path: &str) {
    if path.is_empty() {
        return;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{path}\"")).spawn();
    }
    #[cfg(not(windows))]
    {
        let dir = std::path::Path::new(path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

pub(crate) fn open_settings(goto_diag: bool) {
    with_app(|app| {
        if app.settings.is_none() {
            app.settings = settings_ui::SettingsCtl::new(app.node.clone(), app.rt.clone());
        }
        let dark = app.ui().map(|u| u.global::<Theme>().get_dark()).unwrap_or(false);
        if let Some(sc) = app.settings.as_mut() {
            sc.set_dark(dark);
            sc.show(goto_diag, &app.peers);
        }
    });
}

pub(crate) fn theme_is_dark(theme: &str) -> bool {
    #[cfg(windows)]
    {
        win::theme_is_dark(theme)
    }
    #[cfg(not(windows))]
    {
        theme == "dark"
    }
}

pub(crate) fn data_dir() -> std::path::PathBuf {
    env("LANCAT_DATA_DIR").map(Into::into).unwrap_or_else(|| dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("lancat-slint-dev"))
}

fn wire_ui(ui: &PanelWindow) {
    ui.on_select_peer(|id: SharedString, shift: bool| {
        with_app(|app| {
            let id = id.to_string();
            if shift {
                // toggleSelect
                app.broadcast = false;
                if let Some(i) = app.selected.iter().position(|x| *x == id) {
                    app.selected.remove(i);
                } else {
                    if app.selected.is_empty() && !app.current.is_empty() && app.current != id {
                        app.selected.push(app.current.clone());
                    }
                    app.selected.push(id);
                }
                app.rebuild_peers();
                return;
            }
            app.selected.clear();
            app.rebuild_peers();
            app.open(&id);
        });
    });
    ui.on_select_all(|| {
        with_app(|app| {
            app.selected.clear();
            app.broadcast = !app.broadcast;
            app.rebuild_peers();
        });
    });
    ui.on_send_text(|t: SharedString| {
        with_app(|app| app.send_text(t.to_string()));
    });
    ui.on_typing(|| {
        with_app(|app| app.on_typing());
    });
    ui.on_composer_blur(|| {
        with_app(|app| app.stop_typing());
    });
    ui.on_refresh(|| {
        with_app(|app| {
            if app.refreshing_until.is_some_and(|t| t > Instant::now()) {
                return;
            }
            app.node.refresh_peers();
            app.refreshing_until = Some(Instant::now() + Duration::from_millis(3200));
            if let Some(ui) = app.ui() {
                ui.set_refreshing(true);
                let w = ui.as_weak();
                slint::Timer::single_shot(Duration::from_millis(3200), move || {
                    if let Some(ui) = w.upgrade() {
                        ui.set_refreshing(false);
                    }
                });
            }
        });
    });
    ui.on_toggle_pin(|| {
        #[cfg(windows)]
        {
            let p = !dock::pinned();
            dock::set_pinned(p);
        }
    });
    ui.on_escape(|| {
        #[cfg(windows)]
        win::request_hide();
    });
    ui.on_open_settings(|| open_settings(false));
    ui.on_open_diag(|| open_settings(true));
    ui.on_dismiss_banner(|| {
        #[cfg(windows)]
        diag::dismiss_alert();
    });
    ui.on_header_pressed(|| {
        // 按住标题栏拖动可以把停靠条移到别的边（旧版 dock_move_start）
        #[cfg(windows)]
        dock::begin_move(false);
    });
    ui.on_peer_menu(|id, x, y| {
        with_app(|app| {
            let blocked = app.peers.iter().find(|p| p.id == id.as_str()).is_some_and(|p| p.blocked);
            if let Some(ui) = app.ui() {
                ui.set_menu_x(x);
                ui.set_menu_y(y);
                ui.set_menu_blocked(blocked);
                ui.set_menu_peer(id);
            }
        });
    });
    ui.on_menu_action(|a| {
        with_app(|app| app.peer_menu_action(a.as_str()));
    });
    ui.on_attach(|dir| {
        with_app(|app| app.attach(dir));
    });
    ui.on_file_action(|id, a| {
        with_app(|app| app.file_action(id.to_string(), a.to_string()));
    });
    ui.on_add_manual(|addr| {
        let addr = addr.trim().to_string();
        if addr.is_empty() {
            return;
        }
        with_app(|app| {
            if let Some(ui) = app.ui() {
                ui.set_adding(true);
            }
            let node = app.node.clone();
            app.rt.spawn(async move {
                let r = node.add_peer_addr(&addr).await;
                post(move |app| {
                    if let Some(ui) = app.ui() {
                        ui.set_adding(false);
                    }
                    match r {
                        Ok(p) => {
                            if let Some(ui) = app.ui() {
                                ui.set_manual_addr("".into());
                            }
                            app.flash(format!("已添加 {}", p.name));
                        }
                        Err(e) => app.flash(format!("连接失败：{e}")),
                    }
                });
            });
        });
    });
    ui.on_scrolled(|vy, h, ch| {
        with_app(|app| {
            // stick to the bottom while within 40 px of it; near the top: load older history
            app.stick = (ch + vy - h) < 40.0;
            if -vy < 30.0 {
                app.load_older();
            }
        });
    });
    ui.on_sticker_hover(|id, h| {
        with_app(|app| {
            if h {
                app.play_sticker(id.as_str(), true);
            } else {
                app.emoji.unhover_chat(id.as_str());
            }
        });
    });
    emoji::wire(ui);
}

fn main() {
    let _ = t0();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // 单实例：已有实例（同一数据目录）在跑就把命令行转给它然后退出
    #[cfg(windows)]
    if win::dock_mode() && dock::forward_to_running() {
        return;
    }

    #[cfg(windows)]
    slint::platform::set_platform(Box::new(win::Win32Platform::new())).expect("set platform");
    // LANCAT_SLINT_WL=1 用自写的 SCTK layer-shell 后端（默认仍是 winit）
    #[cfg(target_os = "linux")]
    if env("LANCAT_SLINT_WL").as_deref() == Some("1") {
        match wl::WlPlatform::new() {
            Ok(p) => slint::platform::set_platform(Box::new(p)).expect("set platform"),
            Err(e) => log::warn!("wayland layer-shell backend unavailable ({e}); using winit"),
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().thread_name("lancat-core").build().expect("tokio");
    let mut cfg = NodeConfig::new(data_dir()).with_env();
    if env("LANCAT_PORT").is_none() {
        cfg.port = 47840;
        cfg.udp_port = 47840;
    }
    if env("LANCAT_SLINT_DISCOVERY").as_deref() != Some("1") {
        cfg.enable_mdns = false;
        cfg.enable_udp = false;
    }
    log::info!("lancat-slint: data {:?} port {} mdns {} udp {}", cfg.data_dir, cfg.port, cfg.enable_mdns, cfg.enable_udp);
    let wanted_port = cfg.port;
    let node = rt.block_on(Node::start(cfg)).expect("start lancat node");
    log::info!("node started in {} ms", t0().elapsed().as_millis());

    let ui = PanelWindow::new().expect("create panel");
    let settings = node.settings();
    let peer_model = Rc::new(VecModel::<PeerItem>::default());
    let msg_model = Rc::new(VecModel::<MsgItem>::default());
    ui.set_peers(ModelRc::from(peer_model.clone()));
    ui.set_messages(ModelRc::from(msg_model.clone()));
    ui.set_me_name(node.me().name.into());

    let app = App {
        ui: ui.as_weak(),
        node: node.clone(),
        rt: rt.handle().clone(),
        peers: node.peers(),
        peer_model,
        msgs: Vec::new(),
        has_more: false,
        loading_older: false,
        msg_model,
        current: String::new(),
        broadcast: false,
        selected: Vec::new(),
        typing: Default::default(),
        typing_timer: Default::default(),
        progress: Default::default(),
        last_typing_sent: None,
        typing_peer: None,
        refreshing_until: None,
        dialog_open: false,
        visible: !cfg!(windows) || !win_dock_mode(),
        toast_timer: Default::default(),
        emoji: emoji::EmojiState::new(node.clone(), &settings),
        settings: None,
        stick: true,
    };
    APP.with(|a| *a.borrow_mut() = Some(app));
    with_app(|app| {
        app.apply_theme(&settings);
        app.rebuild_peers();
        // first peer is the default conversation (旧版 init：peers[0]，在线优先排序)
        if let Some(id) = app.peers.first().map(|p| p.id.clone()) {
            app.open(&id);
        } else {
            app.update_header();
        }
        if let Some(ui) = app.ui() {
            ui.set_panel_visible(app.visible);
        }
    });
    wire_ui(&ui);

    // core events → UI thread
    let mut rx = node.subscribe();
    rt.spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => post(move |app| app.on_core(ev)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => log::warn!("ui lagged {n} core events"),
                Err(_) => break,
            }
        }
    });

    // test hooks (no real input device needed; peers are local throwaway instances):
    // LANCAT_SLINT_PEER=ip:port 启动后手动添加对端；LANCAT_SLINT_SCRIPT="ms|cmd|peer|arg;..."（见 run_script）
    if let Some(addr) = env("LANCAT_SLINT_PEER") {
        let node2 = node.clone();
        rt.spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            if let Err(e) = node2.add_peer_addr(&addr).await {
                log::warn!("LANCAT_SLINT_PEER {addr}: {e}");
            }
        });
    }
    if let Some(script) = env("LANCAT_SLINT_SCRIPT") {
        for step in script.split(';').filter(|s| !s.trim().is_empty()) {
            let parts: Vec<String> = step.splitn(4, '|').map(|s| s.to_string()).collect();
            let ms: u64 = parts.first().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
            slint::Timer::single_shot(Duration::from_millis(ms), move || run_script(&parts));
        }
    }

    ui.show().expect("show");
    log::info!("panel shown {} ms after start", t0().elapsed().as_millis());
    #[cfg(windows)]
    diag::init(node.clone(), rt.handle().clone(), wanted_port);
    #[cfg(not(windows))]
    let _ = wanted_port;
    #[cfg(windows)]
    if win::dock_mode() {
        let w1 = ui.as_weak();
        dock::init(
            node.clone(),
            dock::Hooks {
                composer: Box::new(move || {
                    let dialog = with_app(|app| app.dialog_open).unwrap_or(false);
                    if dialog {
                        return (true, true);
                    }
                    w1.upgrade().map(|u| (u.get_composer_focused(), !u.get_draft().trim().is_empty())).unwrap_or((false, false))
                }),
                on_open: Box::new(|reason| {
                    let r = reason.to_string();
                    with_app(move |app| app.on_dock_open(&r));
                }),
                on_close: Box::new(|| {
                    with_app(|app| app.set_visible(false));
                }),
                on_drag_hint: Box::new(move |n| {
                    with_app(move |app| app.drag_hint(n));
                }),
                on_drag_over: Box::new(|x, y| {
                    with_app(move |app| app.drag_over(x, y));
                }),
                on_drop: Box::new(|files, pos| {
                    with_app(move |app| app.drop_files(files, pos));
                }),
                on_settings: Box::new(|| {
                    open_settings(false);
                }),
                on_pinned: Box::new(|p| {
                    with_app(move |app| {
                        if let Some(ui) = app.ui() {
                            ui.set_pinned(p);
                        }
                    });
                }),
            },
        );
        let args: Vec<String> = std::env::args().skip(1).collect();
        dock::handle_args(&args, false);
    }
    ui.run().expect("run");
    node.shutdown();
}

/// 测试脚本一步：cmd = text|emoji|file（发给名为 peer 的设备）、open、emoji-panel、settings、diag、diagfix、
/// menu（peer 的右键菜单）、attach-menu、theme（arg = light|dark|system）、hide
fn run_script(parts: &[String]) {
    let cmd = parts.get(1).map(String::as_str).unwrap_or("");
    let peer = parts.get(2).cloned().unwrap_or_default();
    let arg = parts.get(3).cloned().unwrap_or_default();
    log::info!("script: {cmd} {peer} {arg}");
    match cmd {
        "settings" => return open_settings(false),
        "diag" => return open_settings(true),
        "open" => {
            #[cfg(windows)]
            dock::open_panel("probe");
            return;
        }
        "hide" => {
            #[cfg(windows)]
            dock::hide_panel();
            return;
        }
        _ => {}
    }
    with_app(move |app| {
        let id = app.peers.iter().find(|p| p.name == peer).map(|p| p.id.clone());
        match cmd {
            "text" => {
                if let Some(id) = id {
                    let node = app.node.clone();
                    app.rt.spawn(async move {
                        if let Ok(m) = node.send_text(&id, &arg).await {
                            post(move |app| app.upsert_message(m, true));
                        }
                    });
                }
            }
            "emoji" => {
                if let Some(id) = id {
                    let alt = emoji::by_code(&arg).map(|e| e.ch.to_string()).unwrap_or_default();
                    let node = app.node.clone();
                    let e = EmojiPayload { kind: "noto".into(), code: arg, alt, data: None };
                    app.rt.spawn(async move {
                        if let Ok(m) = node.send_emoji(&id, e).await {
                            post(move |app| app.upsert_message(m, true));
                        }
                    });
                }
            }
            "file" => {
                if let Some(id) = id {
                    app.send_files(vec![id], arg.split('|').map(String::from).collect(), false);
                }
            }
            "select" => {
                if let Some(id) = id {
                    app.open(&id);
                }
            }
            "emoji-panel" => {
                if let Some(ui) = app.ui() {
                    ui.set_emoji_open(true);
                }
                app.emoji_opened_pub(true);
            }
            "emoji-search" => {
                if let Some(ui) = app.ui() {
                    ui.set_emoji_query(arg.clone().into());
                    ui.invoke_emoji_search(arg.into());
                }
            }
            "emoji-tab" => {
                if let Some(ui) = app.ui() {
                    ui.invoke_emoji_jump(arg.into());
                }
            }
            "menu" => {
                // 脚本在 with_app 里跑：不能 invoke_peer_menu（回调里再 with_app 会借用失败，菜单不出），直接设属性
                if let (Some(id), Some(ui)) = (id, app.ui()) {
                    let blocked = app.peers.iter().find(|p| p.id == id.as_str()).is_some_and(|p| p.blocked);
                    ui.set_menu_x(30.0);
                    ui.set_menu_y(140.0);
                    ui.set_menu_blocked(blocked);
                    ui.set_menu_peer(id.into());
                }
            }
            "attach-menu" => {
                if let Some(ui) = app.ui() {
                    ui.set_attach_open(true);
                }
            }
            "close-popups" => {
                if let Some(ui) = app.ui() {
                    ui.set_attach_open(false);
                    ui.set_menu_peer("".into());
                    ui.set_emoji_open(false);
                }
                app.emoji_opened_pub(false);
            }
            "theme" => {
                let mut s = app.node.settings();
                s.theme = arg;
                let _ = app.node.set_settings(s);
            }
            "diagfix" => {
                // 只走 dry-run：LANCAT_FIX_DRY_RUN=1 时 run_fix 一律只返回预览命令
                #[cfg(windows)]
                if let Some(sc) = app.settings.as_ref() {
                    let _ = sc;
                    let fixes: Vec<String> = diag::last().map(|d| d.checks.iter().filter_map(|c| c.fix.as_ref().map(|f| f.id.clone())).collect()).unwrap_or_default();
                    let f = if arg.is_empty() { fixes.first().cloned() } else { Some(arg) };
                    if let (Some(f), true) = (f, diag::dry_run_forced()) {
                        settings_ui::test_fix(f);
                    }
                }
            }
            _ => log::warn!("script: unknown {cmd}"),
        }
    });
}

fn win_dock_mode() -> bool {
    #[cfg(windows)]
    {
        win::dock_mode()
    }
    #[cfg(not(windows))]
    {
        false
    }
}
