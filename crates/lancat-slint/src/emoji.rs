//! 表情：内置 Noto 动态表情（嵌入、按需解压）+ 导入的 .tgs/.json 贴纸；表情面板（最近 / 分类 / 搜索 /
//! 贴纸导入删除）、首帧海报（后台线程用 ThorVG 渲染，内存缓存）、聊天里的贴纸播放（新消息播 2 遍，悬停循环）、
//! 面板格子悬停播放。与旧版 EmojiPanel.svelte / Sticker.svelte / lottie.ts 行为一致。

use crate::emoji_catalog::{Emoji, CATEGORIES, EMOJI};
use crate::lottie::Player;
use crate::{post, with_app, EmojiCell, EmojiSection, PanelWindow};
use lancat_core::{EmojiPayload, Node, Settings};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

include!(concat!(env!("OUT_DIR"), "/emoji_blobs.rs"));

pub fn by_code(code: &str) -> Option<&'static Emoji> {
    EMOJI.iter().find(|e| e.code == code)
}

/// 整条文字就是一个目录里的表情（soloEmoji）
pub fn by_char(t: &str) -> Option<&'static Emoji> {
    if t.is_empty() {
        return None;
    }
    let strip = |s: &str| s.chars().filter(|c| *c != '\u{fe0f}').collect::<String>();
    let t2 = strip(t);
    EMOJI.iter().find(|e| e.ch == t || strip(e.ch) == t2)
}

fn noto_json(code: &str) -> Option<String> {
    let z = EMOJI_Z.iter().find(|(c, _)| *c == code).map(|(_, z)| *z)?;
    let mut s = String::new();
    flate2::read::DeflateDecoder::new(z).read_to_string(&mut s).ok()?;
    Some(s)
}

// ------------------------------------------------------------------ stickers (commands.rs 同款存储)

fn sticker_dir(node: &Node) -> PathBuf {
    node.data_dir().join("stickers")
}

pub struct StickerMeta {
    pub id: String,
    pub name: String,
}

pub fn list_stickers(node: &Node) -> Vec<StickerMeta> {
    let dir = sticker_dir(node);
    let mut v: Vec<(std::time::SystemTime, StickerMeta)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|ent| {
            let p = ent.path();
            if p.extension()? != "json" {
                return None;
            }
            let id = p.file_stem()?.to_string_lossy().to_string();
            let name = std::fs::read_to_string(dir.join(format!("{id}.name"))).unwrap_or_else(|_| id.clone());
            Some((ent.metadata().ok()?.modified().ok()?, StickerMeta { id, name }))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().map(|x| x.1).collect()
}

fn get_sticker(node: &Node, id: &str) -> Option<String> {
    if !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    std::fs::read_to_string(sticker_dir(node).join(format!("{id}.json"))).ok()
}

/// 读 .tgs（gzip 的 Lottie）或 .json，校验后存进贴纸目录（≤1 MiB）
pub fn import_sticker(node: &Node, path: &std::path::Path) -> Result<StickerMeta, String> {
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    let json = if raw.starts_with(&[0x1f, 0x8b]) {
        let mut s = String::new();
        flate2::read::GzDecoder::new(&raw[..]).read_to_string(&mut s).map_err(|_| "无法解压 .tgs".to_string())?;
        s
    } else {
        String::from_utf8(raw).map_err(|_| "不是有效的 Lottie JSON".to_string())?
    };
    if json.len() > 1 << 20 {
        return Err("贴纸太大（>1MB）".into());
    }
    let v: serde_json::Value = serde_json::from_str(&json).map_err(|_| "不是有效的 Lottie JSON".to_string())?;
    if v.get("layers").is_none() {
        return Err("不是有效的 Lottie 动画".into());
    }
    let dir = sticker_dir(node);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let id = format!("{:032x}", rand_u128());
    std::fs::write(dir.join(format!("{id}.json")), &json).map_err(|e| e.to_string())?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let name: String = stem.chars().take(64).collect();
    std::fs::write(dir.join(format!("{id}.name")), &name).map_err(|e| e.to_string())?;
    Ok(StickerMeta { id, name })
}

fn rand_u128() -> u128 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    let a = h.finish() as u128;
    let mut h2 = std::collections::hash_map::RandomState::new().build_hasher();
    h2.write_u64(a as u64 ^ 0x9e37_79b9_7f4a_7c15);
    (a << 64) | h2.finish() as u128
}

fn delete_sticker(node: &Node, id: &str) -> Result<(), String> {
    if !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("bad id".into());
    }
    let dir = sticker_dir(node);
    let _ = std::fs::remove_file(dir.join(format!("{id}.name")));
    std::fs::remove_file(dir.join(format!("{id}.json"))).map_err(|e| e.to_string())
}

/// 贴纸 / 表情 → Lottie JSON（贴纸消息自带 data；本机导入的从目录读）
fn lottie_data(node: &Node, e: &EmojiPayload) -> Option<String> {
    if e.kind == "sticker" {
        if let Some(d) = &e.data {
            return Some(d.clone());
        }
        return get_sticker(node, &e.code);
    }
    noto_json(&e.code)
}

fn poster_key(e: &EmojiPayload, px: u32) -> String {
    format!("{}:{}:{px}", e.kind, e.code)
}

// ------------------------------------------------------------------ poster worker

struct Job {
    key: String,
    json: String,
    px: u32,
}

fn spawn_worker() -> mpsc::Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("lancat-posters".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                let px = job.px;
                let buf = Player::new(&job.json, px).map(|mut p| p.render_raw(0.0));
                let key = job.key;
                post(move |app| app.emoji.poster_done(key, px, buf));
            }
        })
        .expect("poster thread");
    tx
}

// ------------------------------------------------------------------ state

struct ChatAnim {
    player: Player,
    frame: f32,
    loops: u32,
    hover: bool,
}

pub struct EmojiState {
    node: Arc<Node>,
    posters: HashMap<String, slint::Image>,
    pending: HashSet<String>,
    worker: mpsc::Sender<Job>,
    chat: HashMap<String, ChatAnim>,
    hover: Option<(String, Player, f32)>,
    timer: slint::Timer,
    last_tick: Instant,
    recent: Vec<String>,
    stickers: Vec<StickerMeta>,
    open: bool,
    query: String,
    importing: bool,
    rebuild_queued: bool,
}

const GRID_PX: f32 = 30.0;

impl EmojiState {
    pub fn new(node: Arc<Node>, s: &Settings) -> Self {
        EmojiState {
            node,
            posters: HashMap::new(),
            pending: HashSet::new(),
            worker: spawn_worker(),
            chat: HashMap::new(),
            hover: None,
            timer: slint::Timer::default(),
            last_tick: Instant::now(),
            recent: s.recent_emoji.clone(),
            stickers: Vec::new(),
            open: false,
            query: String::new(),
            importing: false,
            rebuild_queued: false,
        }
    }

    pub fn settings_changed(&mut self, s: &Settings) {
        if s.recent_emoji != self.recent {
            self.recent = s.recent_emoji.clone();
            if self.open {
                self.queue_rebuild();
            }
        }
    }

    /// 已缓存的首帧海报；没有就排队后台渲染（完成后刷新用到它的地方）并返回 None
    pub fn poster(&mut self, e: &EmojiPayload, px: u32) -> Option<slint::Image> {
        let key = poster_key(e, px);
        if let Some(i) = self.posters.get(&key) {
            return Some(i.clone());
        }
        if self.pending.insert(key.clone()) {
            match lottie_data(&self.node, e) {
                Some(json) => {
                    let _ = self.worker.send(Job { key, json, px });
                }
                None => {
                    self.pending.remove(&key);
                }
            }
        }
        None
    }

    fn poster_done(&mut self, key: String, px: u32, buf: Option<Vec<u8>>) {
        self.pending.remove(&key);
        let Some(buf) = buf else { return };
        let img = image_from(&buf, px);
        self.posters.insert(key.clone(), img);
        // chat rows waiting for it / the open grid
        if key.ends_with(&format!(":{}", (160.0 * scale()) as u32)) {
            with_app_later(|app| app.refresh_stickers());
        }
        if self.open {
            self.queue_rebuild();
        }
    }

    fn queue_rebuild(&mut self) {
        if self.rebuild_queued {
            return;
        }
        self.rebuild_queued = true;
        slint::Timer::single_shot(Duration::from_millis(60), || {
            with_app(|app| {
                app.emoji.rebuild_queued = false;
                app.emoji.rebuild_sections();
            });
        });
    }

    fn cell(&mut self, key: &str) -> Option<EmojiCell> {
        let px = (GRID_PX * scale()) as u32;
        if let Some(id) = key.strip_prefix("s:") {
            let name = self.stickers.iter().find(|s| s.id == id)?.name.clone();
            let e = EmojiPayload { kind: "sticker".into(), code: id.into(), alt: name.clone(), data: None };
            let img = self.poster(&e, px).unwrap_or_default();
            return Some(EmojiCell { key: key.into(), img, title: format!("{name}（右键删除）").into() });
        }
        let em = by_code(key)?;
        let e = EmojiPayload { kind: "noto".into(), code: em.code.into(), alt: em.ch.into(), data: None };
        let img = self.poster(&e, px).unwrap_or_default();
        Some(EmojiCell { key: key.into(), img, title: em.kw.split(' ').next().unwrap_or("").into() })
    }

    fn rebuild_sections(&mut self) {
        let Some(ui) = ui() else { return };
        let mut secs: Vec<EmojiSection> = Vec::new();
        let q = self.query.trim().to_lowercase();
        if !q.is_empty() {
            let keys: Vec<&str> = EMOJI.iter().filter(|e| e.kw.to_lowercase().contains(&q) || e.ch == q).map(|e| e.code).collect();
            let cells: Vec<EmojiCell> = keys.iter().filter_map(|k| self.cell(k)).collect();
            let note = if cells.is_empty() { format!("没有找到「{}」", self.query.trim()) } else { String::new() };
            secs.push(EmojiSection { id: "search".into(), label: "".into(), cells: ModelRc::new(VecModel::from(cells)), stickers: false, note: note.into() });
        } else {
            for c in CATEGORIES {
                match c.id {
                    "recent" => {
                        let keys: Vec<String> = self.recent.iter().take(21).cloned().collect();
                        let cells: Vec<EmojiCell> = keys.iter().filter_map(|k| self.cell(k)).collect();
                        if !cells.is_empty() {
                            secs.push(EmojiSection { id: "recent".into(), label: "最近使用".into(), cells: ModelRc::new(VecModel::from(cells)), stickers: false, note: "".into() });
                        }
                    }
                    "stickers" => {
                        let keys: Vec<String> = self.stickers.iter().map(|s| format!("s:{}", s.id)).collect();
                        let cells: Vec<EmojiCell> = keys.iter().filter_map(|k| self.cell(k)).collect();
                        let note = if cells.is_empty() { "导入 Telegram 的 .tgs 动态贴纸（或 Lottie .json），即可在这里发送。" } else { "" };
                        secs.push(EmojiSection { id: "stickers".into(), label: "贴纸".into(), cells: ModelRc::new(VecModel::from(cells)), stickers: true, note: note.into() });
                    }
                    id => {
                        let keys: Vec<&str> = EMOJI.iter().filter(|e| e.cat == id).map(|e| e.code).collect();
                        let cells: Vec<EmojiCell> = keys.iter().filter_map(|k| self.cell(k)).collect();
                        secs.push(EmojiSection { id: id.into(), label: c.label.into(), cells: ModelRc::new(VecModel::from(cells)), stickers: false, note: "".into() });
                    }
                }
            }
        }
        ui.set_emoji_sections(ModelRc::new(VecModel::from(secs)));
        ui.set_importing(self.importing);
    }

    pub fn opened_pub(&mut self, open: bool) {
        self.opened(open);
    }

    fn opened(&mut self, open: bool) {
        self.open = open;
        if open {
            self.stickers = list_stickers(&self.node);
            self.query.clear();
            if let Some(ui) = ui() {
                ui.set_emoji_query("".into());
                ui.set_emoji_scroll(0.0);
                ui.set_emoji_tab("recent".into());
            }
            self.rebuild_sections();
            if let Some(ui) = ui() {
                ui.invoke_focus_emoji_search();
            }
        } else {
            self.hover = None;
            if let Some(ui) = ui() {
                ui.set_emoji_hovered("".into());
            }
        }
    }

    fn remember(&mut self, key: &str) {
        let mut s = self.node.settings();
        let mut next = vec![key.to_string()];
        next.extend(s.recent_emoji.iter().filter(|k| *k != key).cloned());
        next.truncate(24);
        s.recent_emoji = next.clone();
        self.recent = next;
        let _ = self.node.set_settings(s);
    }

    fn payload(&self, key: &str) -> Option<EmojiPayload> {
        if let Some(id) = key.strip_prefix("s:") {
            let data = get_sticker(&self.node, id)?;
            let name = self.stickers.iter().find(|s| s.id == id).map(|s| s.name.clone()).unwrap_or_default();
            return Some(EmojiPayload { kind: "sticker".into(), code: id.into(), alt: name, data: Some(data) });
        }
        let e = by_code(key)?;
        Some(EmojiPayload { kind: "noto".into(), code: e.code.into(), alt: e.ch.into(), data: None })
    }

    // ---------------- animation ----------------

    fn ensure_timer(&mut self) {
        if self.timer.running() {
            return;
        }
        self.last_tick = Instant::now();
        self.timer.start(slint::TimerMode::Repeated, Duration::from_millis(33), || {
            with_app(|app| app.tick_emoji());
        });
    }

    pub fn play_chat(&mut self, msg_id: &str, e: &EmojiPayload, px: u32, hover: bool) {
        if let Some(a) = self.chat.get_mut(msg_id) {
            a.hover |= hover;
            self.ensure_timer();
            return;
        }
        let Some(json) = lottie_data(&self.node, e) else { return };
        let Some(player) = Player::new(&json, px) else { return };
        self.chat.insert(msg_id.to_string(), ChatAnim { player, frame: 0.0, loops: 0, hover });
        self.ensure_timer();
    }

    pub fn unhover_chat(&mut self, msg_id: &str) {
        if let Some(a) = self.chat.get_mut(msg_id) {
            a.hover = false;
            if a.loops >= 2 {
                a.loops = 2;
            }
        }
    }

    pub fn stop_chat(&mut self) {
        self.chat.clear();
    }

    pub fn stop_all(&mut self) {
        self.chat.clear();
        self.hover = None;
        self.timer.stop();
    }

    fn grid_hover(&mut self, key: &str, on: bool) {
        if !on {
            if self.hover.as_ref().is_some_and(|h| h.0 == key) {
                self.hover = None;
            }
            return;
        }
        let Some(e) = self.payload(key) else { return };
        let Some(json) = lottie_data(&self.node, &e) else { return };
        let px = (GRID_PX * scale()) as u32;
        if let Some(mut p) = Player::new(&json, px) {
            let img = p.render(0.0);
            if let Some(ui) = ui() {
                ui.set_emoji_hover_img(img);
                ui.set_emoji_hovered(key.into());
            }
            self.hover = Some((key.to_string(), p, 0.0));
            self.ensure_timer();
        }
    }
}

impl crate::App {
    fn tick_emoji(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.emoji.last_tick).as_secs_f32().min(0.2);
        self.emoji.last_tick = now;
        let mut frames: Vec<(String, slint::Image)> = Vec::new();
        let mut done: Vec<String> = Vec::new();
        for (id, a) in self.emoji.chat.iter_mut() {
            a.frame += a.player.fps() * dt;
            if a.frame >= a.player.total {
                a.frame = 0.0;
                a.loops += 1;
                if !a.hover && a.loops >= 2 {
                    done.push(id.clone());
                    // stop on the first frame (= the poster)
                    frames.push((id.clone(), a.player.render(0.0)));
                    continue;
                }
            }
            frames.push((id.clone(), a.player.render(a.frame)));
        }
        for id in done {
            self.emoji.chat.remove(&id);
        }
        for (id, img) in frames {
            self.set_sticker_frame(&id, img);
        }
        if let Some((_, p, f)) = self.emoji.hover.as_mut() {
            *f += p.fps() * dt;
            if *f >= p.total {
                *f = 0.0;
            }
            let img = p.render(*f);
            if let Some(ui) = self.ui.upgrade() {
                ui.set_emoji_hover_img(img);
            }
        }
        if self.emoji.chat.is_empty() && self.emoji.hover.is_none() {
            self.emoji.timer.stop();
        }
    }

    /// 海报到了：刷新还没有图的贴纸行
    fn refresh_stickers(&mut self) {
        let ids: Vec<usize> = (0..self.msgs.len()).filter(|&i| self.sticker_of(&self.msgs[i]).is_some()).collect();
        for i in ids {
            if self.emoji.chat.contains_key(&self.msgs[i].id) {
                continue;
            }
            self.refresh_row_pub(i);
        }
    }
}

fn image_from(buf: &[u8], px: u32) -> slint::Image {
    let mut pb = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(px, px);
    pb.make_mut_bytes().copy_from_slice(buf);
    slint::Image::from_rgba8_premultiplied(pb)
}

fn ui() -> Option<PanelWindow> {
    with_app_ui()
}

fn with_app_ui() -> Option<PanelWindow> {
    UI.with(|u| u.borrow().as_ref().and_then(|w| w.upgrade()))
}

thread_local! {
    static UI: std::cell::RefCell<Option<slint::Weak<PanelWindow>>> = const { std::cell::RefCell::new(None) };
}

fn scale() -> f32 {
    ui().map(|u| u.window().scale_factor()).unwrap_or(1.0)
}

fn with_app_later(f: impl FnOnce(&mut crate::App) + 'static) {
    slint::Timer::single_shot(Duration::from_millis(0), move || {
        with_app(f);
    });
}

pub fn wire(w: &PanelWindow) {
    UI.with(|u| *u.borrow_mut() = Some(w.as_weak()));
    w.on_emoji_toggled(|open| {
        with_app(|app| app.emoji.opened(open));
    });
    w.on_emoji_pick(|key: SharedString| {
        with_app(|app| {
            let Some(p) = app.emoji.payload(key.as_str()) else { return };
            app.emoji.remember(key.as_str());
            app.send_emoji(p);
        });
    });
    w.on_emoji_hover(|key, on| {
        with_app(|app| app.emoji.grid_hover(key.as_str(), on));
    });
    w.on_emoji_remove(|key| {
        let Some(id) = key.strip_prefix("s:").map(String::from) else { return };
        with_app(move |app| {
            match delete_sticker(&app.node, &id) {
                Ok(()) => {
                    app.emoji.stickers.retain(|s| s.id != id);
                    app.flash("已删除贴纸");
                }
                Err(e) => app.flash(e),
            }
            app.emoji.rebuild_sections();
        });
    });
    w.on_emoji_search(|q| {
        with_app(|app| {
            app.emoji.query = q.to_string();
            app.emoji.rebuild_sections();
            if let Some(ui) = ui() {
                ui.set_emoji_scroll(0.0);
            }
        });
    });
    w.on_emoji_jump(|tab| {
        with_app(|app| {
            app.emoji.query.clear();
            let Some(ui) = ui() else { return };
            ui.set_emoji_query("".into());
            ui.set_emoji_tab(tab.clone());
            app.emoji.rebuild_sections();
            // section offsets: header 26 + rows × 38 (+ note 40)
            let cols = ui.get_emoji_cols().max(1) as usize;
            let mut y = 0.0f32;
            let secs = ui.get_emoji_sections();
            use slint::Model;
            for i in 0..secs.row_count() {
                let s = secs.row_data(i).unwrap();
                if s.id == tab {
                    break;
                }
                let n = s.cells.row_count();
                y += 26.0 + (n.div_ceil(cols) as f32) * 38.0 + if s.note.is_empty() { 0.0 } else { 40.0 };
            }
            ui.set_emoji_scroll(-y);
        });
    });
    w.on_emoji_import(|| {
        with_app(|app| {
            app.emoji.importing = true;
            app.dialog_open_pub(true);
            #[cfg(windows)]
            crate::filedlg::pick_filtered(&[("Lottie 贴纸 (*.tgs;*.json)", "*.tgs;*.json")], |paths| {
                with_app(move |app| {
                    app.dialog_open_pub(false);
                    let mut n = 0;
                    for p in &paths {
                        match import_sticker(&app.node, std::path::Path::new(p)) {
                            Ok(_) => n += 1,
                            Err(e) => {
                                let f = std::path::Path::new(p).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                                app.flash(format!("{f}: {e}"));
                            }
                        }
                    }
                    app.emoji.importing = false;
                    app.emoji.stickers = list_stickers(&app.node);
                    if n > 0 {
                        app.flash(format!("已导入 {n} 个贴纸"));
                    }
                    app.emoji.rebuild_sections();
                });
            });
        });
    });
}
