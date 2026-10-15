//! 表情：内置 Noto 动态表情（嵌入、按需解压）+ 导入的 .tgs/.json 贴纸；表情面板（最近 / 分类 / 搜索 /
//! 贴纸导入删除）、首帧海报（后台线程用 ThorVG 渲染，内存缓存）、聊天里的贴纸播放（新消息播 2 遍，悬停循环）、
//! 面板格子悬停播放。与旧版 EmojiPanel.svelte / Sticker.svelte / lottie.ts 行为一致。

use crate::emoji_catalog::{Emoji, CATEGORIES, EMOJI};
use crate::lottie::Player;
use crate::{post, with_app, EmojiCell, EmojiSection, PanelWindow};
use lancat_core::{EmojiPayload, Node, Settings};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
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
pub(crate) fn lottie_data(node: &Node, e: &EmojiPayload) -> Option<String> {
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
    /// 解压 / 读文件放到后台线程做：主线程只给出取数据的办法
    src: PosterSrc,
    px: u32,
    /// 首帧的磁盘缓存（预乘 RGBA，px×px×4 字节）：第二次启动起表情面板一打开就是满的
    cache: Option<PathBuf>,
}

enum PosterSrc {
    Noto(String),
    Json(String),
    /// 表情面板静止格：系统彩色字形（旧版 .glyph，25px）；画不出来时退回 Lottie 首帧
    Glyph { ch: String, code: String, font_px: f32 },
}

fn poster_cache_dir() -> PathBuf {
    crate::data_dir().join("cache").join("posters")
}

// ------------------------------------------------------------------ lottie engine

/// 所有 ThorVG 工作（首帧海报、表情面板悬停、聊天贴纸）都在这一条线程里：
/// ThorVG 1.1 之前多线程各用各的画布也不安全（thorvg#4120），一条线程天然串行，UI 线程不碰 ThorVG、不会被解析卡住。
/// 优先级：命令 > 到点的帧（悬停 / 聊天） > 预取 > 海报。
enum Cmd {
    Poster(Job),
    /// 表情面板悬停：只理会最后一条
    HoverStart { key: String, src: PosterSrc, px: u32 },
    HoverStop,
    /// 悬停预取（相邻格）：新的一批替换没做完的旧批
    Prefetch(Vec<(String, PosterSrc, u32)>),
    Chat { id: String, e: EmojiPayload, px: u32, ev: ChatEv },
    ChatClear,
    Pause,
    Resume,
    /// 收起超过 keep-alive：丢掉悬停缓存和聊天里的播放器
    Release,
}

const HOVER_KEEP: usize = 32;
const FRAME: Duration = Duration::from_millis(33);
const MAX_PLAYING: usize = 6;

/// 聊天贴纸（旧版 Sticker.svelte mode="chat"）：进入视口（rootMargin 40px）才播，播满 2 遍停在首帧；
/// 悬停从头循环，离开后播完剩余到满 2 遍；离开视口暂停；同时最多 MAX_PLAYING 个（新的挤掉最早开始的）；
/// 面板隐藏时全部暂停。状态跟着「组件」走：切会话时全部重建。
struct ChatAnim {
    player: Player,
    frame: f32,
    loops: u32,
    hover: bool,
    visible: bool,
    /// 被并发上限挤掉（旧版 playing.shift().pause()）：下次 sync 才继续
    bumped: bool,
}

impl ChatAnim {
    fn wants(&self) -> bool {
        self.visible && !self.bumped && (self.hover || self.loops < 2)
    }
}

struct Engine {
    node: Arc<Node>,
    posters: std::collections::VecDeque<Job>,
    /// (key, px, player)，最近用的在前
    hover_cache: Vec<(String, u32, Player)>,
    hover: Option<(String, Instant)>,
    hover_next: Instant,
    prefetch: Vec<(String, PosterSrc, u32)>,
    chat: HashMap<String, ChatAnim>,
    playing: Vec<String>,
    paused: bool,
    chat_next: Instant,
    last_tick: Instant,
}

fn src_json(src: PosterSrc) -> Option<String> {
    match src {
        PosterSrc::Noto(code) | PosterSrc::Glyph { code, .. } => noto_json(&code),
        PosterSrc::Json(j) => Some(j),
    }
}

impl Engine {
    fn run(mut self, rx: mpsc::Receiver<Cmd>) {
        loop {
            // 下一件到点的事
            let now = Instant::now();
            let mut due: Option<Instant> = None;
            if self.hover.is_some() {
                due = Some(self.hover_next);
            }
            if !self.paused && !self.playing.is_empty() {
                due = Some(due.map_or(self.chat_next, |d| d.min(self.chat_next)));
            }
            let idle_work = !self.prefetch.is_empty() || !self.posters.is_empty();
            let wait = match due {
                Some(d) => d.saturating_duration_since(now),
                None if idle_work => Duration::ZERO,
                None => Duration::from_secs(3600),
            };
            let first = if wait.is_zero() {
                match rx.try_recv() {
                    Ok(c) => Some(c),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            } else {
                match rx.recv_timeout(wait) {
                    Ok(c) => Some(c),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            };
            let mut cmds: Vec<Cmd> = first.into_iter().collect();
            while let Ok(c) = rx.try_recv() {
                cmds.push(c);
            }
            // 悬停只理会最后一条（快速扫过时中间的格子直接跳过）
            let last_hover = cmds.iter().rposition(|c| matches!(c, Cmd::HoverStart { .. } | Cmd::HoverStop));
            for (i, c) in cmds.into_iter().enumerate() {
                match c {
                    Cmd::HoverStart { .. } | Cmd::HoverStop if Some(i) != last_hover => {}
                    c => self.apply(c),
                }
            }
            let now = Instant::now();
            let mut busy = false;
            if self.hover.is_some() && now >= self.hover_next {
                self.hover_tick(now);
                busy = true;
            }
            if !self.paused && !self.playing.is_empty() && now >= self.chat_next {
                self.chat_tick(now);
                busy = true;
            }
            if busy {
                continue;
            }
            // 空闲：一次只做一件低优先级的事，做完回头看命令和帧
            if let Some((key, src, px)) = self.prefetch.pop() {
                if !self.hover_cache.iter().any(|(k, p, _)| *k == key && *p == px) && self.hover_cache.len() < HOVER_KEEP {
                    if let Some(p) = src_json(src).and_then(|j| Player::new(&j, px)) {
                        self.hover_cache.push((key, px, p));
                    }
                }
            } else if let Some(job) = self.posters.pop_front() {
                poster_job(job);
            }
        }
    }

    fn apply(&mut self, c: Cmd) {
        match c {
            Cmd::Poster(j) => self.posters.push_back(j),
            Cmd::HoverStop => self.hover = None,
            Cmd::HoverStart { key, src, px } => {
                let ok = match self.hover_cache.iter().position(|(k, p, _)| *k == key && *p == px) {
                    Some(i) => {
                        let e = self.hover_cache.remove(i);
                        self.hover_cache.insert(0, e);
                        true
                    }
                    None => match src_json(src).and_then(|j| Player::new(&j, px)) {
                        Some(p) => {
                            self.hover_cache.insert(0, (key.clone(), px, p));
                            self.hover_cache.truncate(HOVER_KEEP);
                            true
                        }
                        None => false,
                    },
                };
                let now = Instant::now();
                self.hover = if ok { Some((key, now)) } else { None };
                self.hover_next = now;
            }
            Cmd::Prefetch(v) => {
                // pop() 从尾部取：最想要的放最后
                self.prefetch = v.into_iter().rev().collect();
            }
            Cmd::Chat { id, e, px, ev } => self.chat_event(id, e, px, ev),
            Cmd::ChatClear => {
                self.chat.clear();
                self.playing.clear();
            }
            Cmd::Pause => {
                self.paused = true;
                self.hover = None;
            }
            Cmd::Resume => {
                self.paused = false;
                self.last_tick = Instant::now();
                self.chat_next = self.last_tick;
            }
            Cmd::Release => {
                self.chat.clear();
                self.playing.clear();
                self.hover = None;
                self.hover_cache.clear();
                self.prefetch.clear();
            }
        }
    }

    fn hover_tick(&mut self, now: Instant) {
        let Some((key, t0)) = self.hover.as_ref() else { return };
        let Some((_, px, p)) = self.hover_cache.first_mut() else { return };
        let total = p.total.max(1.0);
        let f = (now.duration_since(*t0).as_secs_f32() * p.fps()) % total;
        let buf = p.render_raw(f);
        let (key, px) = (key.clone(), *px);
        post(move |app| app.emoji.hover_frame(&key, px, buf));
        self.hover_next += FRAME;
        if self.hover_next < now {
            self.hover_next = now + FRAME;
        }
    }

    fn chat_event(&mut self, id: String, e: EmojiPayload, px: u32, ev: ChatEv) {
        if !self.chat.contains_key(&id) {
            if ev == ChatEv::Hidden || ev == ChatEv::HoverOut {
                return;
            }
            let player = lottie_data(&self.node, &e).and_then(|j| Player::new(&j, px));
            let Some(player) = player else {
                post(move |app| {
                    app.emoji.chat_live.remove(&id);
                });
                return;
            };
            self.chat.insert(id.clone(), ChatAnim { player, frame: 0.0, loops: 0, hover: false, visible: false, bumped: false });
        }
        let a = self.chat.get_mut(&id).unwrap();
        match ev {
            ChatEv::Visible => a.visible = true,
            // 离开视口：pause（停在当前帧，不回首帧）
            ChatEv::Hidden => a.visible = false,
            ChatEv::HoverIn => {
                a.hover = true;
                a.loops = 0;
                // 悬停算一次「进入」：旧版 mouseenter 时 IntersectionObserver 早已报告可见
                a.visible = true;
            }
            ChatEv::HoverOut => a.hover = false,
        }
        // sync()：要播就（重新）排进 playing，否则移出
        a.bumped = false;
        let wants = a.wants();
        let was_idle = self.playing.is_empty();
        self.playing.retain(|x| *x != id);
        if wants {
            self.playing.push(id);
            while self.playing.len() > MAX_PLAYING {
                let old = self.playing.remove(0);
                if let Some(o) = self.chat.get_mut(&old) {
                    o.bumped = true;
                }
            }
            if was_idle {
                self.last_tick = Instant::now();
                self.chat_next = self.last_tick;
            }
        }
    }

    fn chat_tick(&mut self, now: Instant) {
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(0.2);
        self.last_tick = now;
        let mut frames: Vec<(String, u32, Vec<u8>)> = Vec::new();
        let mut done: Vec<String> = Vec::new();
        for id in self.playing.clone() {
            let Some(a) = self.chat.get_mut(&id) else { continue };
            if !a.wants() {
                continue;
            }
            a.frame += a.player.fps() * dt;
            let px = a.player.px();
            if a.frame >= a.player.total {
                a.frame = 0.0;
                a.loops += 1;
                if !a.hover && a.loops >= 2 {
                    // 播满 2 遍：停在首帧（= 海报）
                    done.push(id.clone());
                    frames.push((id, px, a.player.render_raw(0.0)));
                    continue;
                }
            }
            frames.push((id, px, a.player.render_raw(a.frame)));
        }
        self.playing.retain(|x| !done.contains(x));
        if !frames.is_empty() {
            post(move |app| {
                for (id, px, buf) in frames {
                    app.set_sticker_frame(&id, image_from(&buf, px));
                }
            });
        }
        self.chat_next += FRAME;
        if self.chat_next < now {
            self.chat_next = now + FRAME;
        }
    }
}

fn poster_job(job: Job) {
    let px = job.px;
    let want = (px * px * 4) as usize;
    let mut buf = job.cache.as_ref().and_then(|p| std::fs::read(p).ok()).filter(|b| b.len() == want);
    if buf.is_none() {
        let json = match job.src {
            PosterSrc::Noto(code) => noto_json(&code),
            PosterSrc::Json(j) => Some(j),
            PosterSrc::Glyph { ch, code, font_px } => match crate::glyph::render(&ch, px, font_px) {
                Some(b) => {
                    buf = Some(b);
                    None
                }
                None => noto_json(&code),
            },
        };
        if buf.is_none() {
            buf = json.and_then(|j| Player::new(&j, px)).map(|mut p| p.render_raw(0.0));
        }
        if let (Some(b), Some(path)) = (buf.as_ref(), job.cache.as_ref()) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, b);
        }
    }
    let key = job.key;
    post(move |app| app.emoji.poster_done(key, px, buf));
}

fn spawn_engine(node: Arc<Node>) -> mpsc::Sender<Cmd> {
    let (tx, rx) = mpsc::channel::<Cmd>();
    std::thread::Builder::new()
        .name("lancat-lottie".into())
        .spawn(move || {
            // Player 持有 ThorVG 指针（!Send）：引擎在线程里建
            let now = Instant::now();
            let eng = Engine {
                node,
                posters: Default::default(),
                hover_cache: Vec::new(),
                hover: None,
                hover_next: now,
                prefetch: Vec::new(),
                chat: HashMap::new(),
                playing: Vec::new(),
                paused: false,
                chat_next: now,
                last_tick: now,
            };
            eng.run(rx)
        })
        .expect("lottie thread");
    tx
}

// ------------------------------------------------------------------ state

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ChatEv {
    Visible,
    Hidden,
    HoverIn,
    HoverOut,
}

pub struct EmojiState {
    node: Arc<Node>,
    posters: HashMap<String, slint::Image>,
    pending: HashSet<String>,
    /// Lottie 线程（海报 / 悬停 / 聊天贴纸）
    eng: mpsc::Sender<Cmd>,
    /// 交给 Lottie 线程在播的聊天贴纸（这些行的图由帧更新，不再刷海报）
    pub(crate) chat_live: HashSet<String>,
    /// 正在悬停播放的格子（帧由 Lottie 线程送来）
    hover: Option<String>,
    /// 格子 key → (分区, 行)：首帧到了直接改那一格
    index: HashMap<String, Vec<(usize, usize)>>,
    recent: Vec<String>,
    stickers: Vec<StickerMeta>,
    open: bool,
    query: String,
    importing: bool,
    rebuild_queued: bool,
    /// 表情网格的列数（Slint 按面板宽度算好报过来）
    cols: usize,
    prewarmed: bool,
    released: bool,
    /// 上次建好的网格的签名（列数 / 最近使用 / 贴纸 / 搜索词）：没变就不重建模型（重建 = 一百多个格子重新实例化）
    built: String,
}

const GRID_PX: f32 = 30.0;
/// 旧版 .glyph { font-size: 25px }
const GLYPH_FONT: f32 = 25.0;

impl EmojiState {
    pub fn new(node: Arc<Node>, s: &Settings) -> Self {
        EmojiState {
            node: node.clone(),
            posters: HashMap::new(),
            pending: HashSet::new(),
            eng: spawn_engine(node.clone()),
            chat_live: HashSet::new(),
            hover: None,
            index: HashMap::new(),
            recent: s.recent_emoji.clone(),
            stickers: Vec::new(),
            open: false,
            query: String::new(),
            importing: false,
            rebuild_queued: false,
            cols: 7,
            prewarmed: false,
            released: false,
            built: String::new(),
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
            let safe = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect::<String>();
            let cache = Some(poster_cache_dir().join(format!("{}-{}-{px}.rgba", safe(&e.kind), safe(&e.code))));
            let src = if e.kind == "noto" && e.data.is_none() { Some(PosterSrc::Noto(e.code.clone())) } else { lottie_data(&self.node, e).map(PosterSrc::Json) };
            match src {
                Some(src) => {
                    let _ = self.eng.send(Cmd::Poster(Job { key, src, px, cache }));
                }
                None => {
                    self.pending.remove(&key);
                }
            }
        }
        None
    }

    /// 表情面板静止格：彩色字形（Windows），其他平台退回 Lottie 首帧
    fn glyph(&mut self, em: &crate::emoji_catalog::Emoji, px: u32) -> Option<slint::Image> {
        let key = format!("glyph:{}:{px}", em.code);
        if let Some(i) = self.posters.get(&key) {
            return Some(i.clone());
        }
        if self.pending.insert(key.clone()) {
            let safe = em.code.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect::<String>();
            let cache = Some(poster_cache_dir().join(format!("glyph{}-{safe}-{px}.rgba", if cfg!(windows) { "w" } else { "" })));
            let src = PosterSrc::Glyph { ch: em.ch.to_string(), code: em.code.to_string(), font_px: GLYPH_FONT * scale() };
            let _ = self.eng.send(Cmd::Poster(Job { key, src, px, cache }));
        }
        None
    }

    /// 面板第一次弹出后在后台把表情面板的首帧都备好（旧版是彩色字形，打开即满；这里首帧要渲染）
    pub fn prewarm(&mut self) {
        if self.prewarmed {
            return;
        }
        self.prewarmed = true;
        let px = (GRID_PX * scale()) as u32;
        let mut codes: Vec<String> = self.recent.iter().filter(|k| !k.starts_with("s:")).cloned().collect();
        codes.extend(EMOJI.iter().map(|e| e.code.to_string()));
        for code in codes {
            if let Some(em) = by_code(&code) {
                let _ = self.glyph(em, px);
            }
        }
        // 网格模型也先建好：第一次打开表情面板时不用在弹出动画的第一帧里建
        if let Some(ui) = ui() {
            self.cols = ui.get_emoji_cols().max(1) as usize;
        }
        self.stickers = list_stickers(&self.node);
        if self.query.is_empty() && self.sig() != self.built {
            self.rebuild_sections();
        }
    }

    fn sig(&self) -> String {
        let st: Vec<&str> = self.stickers.iter().map(|s| s.id.as_str()).collect();
        let rc: Vec<&String> = self.recent.iter().take(21).collect();
        format!("{}|{}|{:?}|{:?}", self.query.trim().to_lowercase(), self.cols, rc, st)
    }

    /// 首帧到了：只换用到它的格子的图（不重建整个网格）
    fn patch_cells(&self, key: &str, img: &slint::Image) {
        let cell_key = if let Some(r) = key.strip_prefix("glyph:") {
            r.rsplit_once(':').map(|(c, _)| c.to_string())
        } else if let Some(r) = key.strip_prefix("sticker:") {
            r.rsplit_once(':').map(|(c, _)| format!("s:{c}"))
        } else {
            None
        };
        let Some(cell_key) = cell_key else { return };
        let Some(spots) = self.index.get(&cell_key) else { return };
        let Some(ui) = ui() else { return };
        let secs = ui.get_emoji_sections();
        for &(si, ri) in spots {
            let Some(sec) = secs.row_data(si) else { continue };
            if let Some(mut c) = sec.cells.row_data(ri) {
                if c.key == cell_key.as_str() {
                    c.img = img.clone();
                    sec.cells.set_row_data(ri, c);
                }
            }
        }
    }

    fn poster_done(&mut self, key: String, px: u32, buf: Option<Vec<u8>>) {
        self.pending.remove(&key);
        let Some(buf) = buf else { return };
        let img = image_from(&buf, px);
        self.posters.insert(key.clone(), img.clone());
        // chat rows waiting for it / the grid
        if key.ends_with(&format!(":{}", (160.0 * scale()) as u32)) {
            with_app_later(|app| app.refresh_stickers());
        }
        if !self.built.is_empty() {
            self.patch_cells(&key, &img);
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
        if let Some(id) = key.strip_prefix("s:") {
            // 贴纸格：Sticker size 34，格高 44
            let px = (34.0 * scale()) as u32;
            let name = self.stickers.iter().find(|s| s.id == id)?.name.clone();
            let e = EmojiPayload { kind: "sticker".into(), code: id.into(), alt: name.clone(), data: None };
            let img = self.poster(&e, px).unwrap_or_default();
            return Some(EmojiCell { key: key.into(), img, title: format!("{name}（右键删除）").into(), sticker: true, h: 44.0, ..Default::default() });
        }
        let px = (GRID_PX * scale()) as u32;
        let em = by_code(key)?;
        let img = self.glyph(em, px).unwrap_or_default();
        Some(EmojiCell { key: key.into(), img, title: em.kw.split(' ').next().unwrap_or("").into(), h: 38.0, ..Default::default() })
    }

    /// CSS grid auto-fill：按列数排行，行高 = 行内最高的格（贴纸 44 / 表情 38）
    fn layout(cells: &mut [EmojiCell], cols: usize) -> f32 {
        let cols = cols.max(1);
        let mut y = 0.0f32;
        for row in cells.chunks_mut(cols) {
            let h = row.iter().map(|c| c.h).fold(0.0f32, f32::max);
            for (j, c) in row.iter_mut().enumerate() {
                c.col = j as i32;
                c.top = y;
                c.h = h;
            }
            y += h;
        }
        y
    }

    fn section(&self, id: &str, prev: &str, label: &str, mut cells: Vec<EmojiCell>, stickers: bool, note: &str) -> EmojiSection {
        let gh = Self::layout(&mut cells, self.cols);
        EmojiSection { id: id.into(), prev: prev.into(), label: label.into(), cells: ModelRc::new(VecModel::from(cells)), grid_h: gh, stickers, note: note.into() }
    }

    fn rebuild_sections(&mut self) {
        let Some(ui) = ui() else { return };
        let mut secs: Vec<EmojiSection> = Vec::new();
        let q = self.query.trim().to_lowercase();
        if !q.is_empty() {
            let keys: Vec<&str> = EMOJI.iter().filter(|e| e.kw.to_lowercase().contains(&q) || e.ch == q).map(|e| e.code).collect();
            let cells: Vec<EmojiCell> = keys.iter().filter_map(|k| self.cell(k)).collect();
            let note = if cells.is_empty() { format!("没有找到「{}」", self.query.trim()) } else { String::new() };
            secs.push(self.section("search", "", "", cells, false, &note));
        } else {
            let mut prev = String::new();
            for c in CATEGORIES {
                match c.id {
                    "recent" => {
                        // 最近使用：表情和贴纸混排（贴纸格高 44，所在行一起变高）
                        let keys: Vec<String> = self.recent.iter().take(21).cloned().collect();
                        let cells: Vec<EmojiCell> = keys.iter().filter_map(|k| self.cell(k)).collect();
                        if !cells.is_empty() {
                            secs.push(self.section("recent", &prev, "最近使用", cells, false, ""));
                            prev = "recent".into();
                        }
                    }
                    "stickers" => {
                        let keys: Vec<String> = self.stickers.iter().map(|s| format!("s:{}", s.id)).collect();
                        let cells: Vec<EmojiCell> = keys.iter().filter_map(|k| self.cell(k)).collect();
                        let note = if cells.is_empty() { "导入 Telegram 的 .tgs 动态贴纸（或 Lottie .json），即可在这里发送。" } else { "" };
                        secs.push(self.section("stickers", &prev, "贴纸", cells, true, note));
                        prev = "stickers".into();
                    }
                    id => {
                        let keys: Vec<&str> = EMOJI.iter().filter(|e| e.cat == id).map(|e| e.code).collect();
                        let cells: Vec<EmojiCell> = keys.iter().filter_map(|k| self.cell(k)).collect();
                        secs.push(self.section(id, &prev, c.label, cells, false, ""));
                        prev = id.into();
                    }
                }
            }
        }
        self.index.clear();
        for (si, sec) in secs.iter().enumerate() {
            for ri in 0..sec.cells.row_count() {
                if let Some(c) = sec.cells.row_data(ri) {
                    self.index.entry(c.key.to_string()).or_default().push((si, ri));
                }
            }
        }
        ui.set_emoji_sections(ModelRc::new(VecModel::from(secs)));
        ui.set_importing(self.importing);
        self.built = self.sig();
    }

    pub fn opened_pub(&mut self, open: bool) {
        self.opened(open);
    }

    fn opened(&mut self, open: bool) {
        self.open = open;
        if open {
            self.stickers = list_stickers(&self.node);
            self.query.clear();
            let mut empty = true;
            if let Some(ui) = ui() {
                ui.set_emoji_query("".into());
                ui.set_emoji_scroll(0.0);
                ui.set_emoji_tab("recent".into());
                self.cols = ui.get_emoji_cols().max(1) as usize;
                empty = ui.get_emoji_sections().row_count() == 0;
            }
            // 内容没变就沿用上次的网格（旧版组件常驻，打开即显示）
            if empty || self.sig() != self.built {
                self.rebuild_sections();
            }
            if let Some(ui) = ui() {
                ui.invoke_focus_emoji_search();
            }
        } else {
            self.stop_hover();
            crate::compose::stop(2);
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

    // ---------------- animation（实际工作都在 Lottie 线程） ----------------

    /// 聊天贴纸事件（视口进出 / 悬停）
    pub fn chat_event(&mut self, msg_id: &str, e: &EmojiPayload, px: u32, ev: ChatEv, _panel_visible: bool) {
        if !self.chat_live.contains(msg_id) {
            if ev == ChatEv::Hidden || ev == ChatEv::HoverOut {
                return;
            }
            self.chat_live.insert(msg_id.to_string());
        }
        let _ = self.eng.send(Cmd::Chat { id: msg_id.to_string(), e: e.clone(), px, ev });
    }

    pub fn stop_chat(&mut self) {
        self.chat_live.clear();
        let _ = self.eng.send(Cmd::ChatClear);
    }

    /// 收起超过 keep-alive：丢掉解码好的首帧 / 字形、悬停缓存和正在播的贴纸（磁盘缓存还在，下次打开表情面板几乎零成本重建）
    pub fn release(&mut self) {
        self.posters.clear();
        self.pending.clear();
        self.chat_live.clear();
        self.stop_hover();
        let _ = self.eng.send(Cmd::Release);
        self.prewarmed = false;
        self.released = true;
        // 网格里的格子也引用着首帧图：一起放掉，下次打开重建
        self.built.clear();
        self.index.clear();
        if let Some(ui) = ui() {
            ui.set_emoji_sections(ModelRc::new(VecModel::<EmojiSection>::default()));
        }
    }

    /// 释放过：下次弹出时让聊天里的贴纸重新取首帧
    pub fn take_released(&mut self) -> bool {
        std::mem::take(&mut self.released)
    }

    /// 面板收起：全部暂停（状态保留，旧版 setLottieActive(false)）
    pub fn pause_all(&mut self) {
        self.stop_hover();
        let _ = self.eng.send(Cmd::Pause);
    }

    /// 面板弹出：继续 playing 里的
    pub fn resume_all(&mut self) {
        let _ = self.eng.send(Cmd::Resume);
    }

    fn stop_hover(&mut self) {
        if self.hover.take().is_some() {
            let _ = self.eng.send(Cmd::HoverStop);
            if let Some(ui) = ui() {
                ui.set_emoji_hovered("".into());
            }
        }
    }

    fn hover_src(&self, key: &str) -> Option<PosterSrc> {
        if by_code(key).is_some() {
            return Some(PosterSrc::Noto(key.to_string()));
        }
        let e = self.payload(key)?;
        lottie_data(&self.node, &e).map(PosterSrc::Json)
    }

    pub fn grid_hover_pub(&mut self, key: &str, on: bool) {
        self.grid_hover(key, on);
    }

    pub fn hovered_key(&self) -> String {
        self.hover.clone().unwrap_or_default()
    }

    fn grid_hover(&mut self, key: &str, on: bool) {
        if !on {
            if self.hover.as_deref() == Some(key) {
                self.stop_hover();
            }
            return;
        }
        let Some(src) = self.hover_src(key) else { return };
        let px = (GRID_PX * scale()) as u32;
        if let Some(ui) = ui() {
            // 新格子先显示静态首帧，上一格立即回到海报
            ui.set_emoji_hovered("".into());
        }
        self.hover = Some(key.to_string());
        let _ = self.eng.send(Cmd::HoverStart { key: key.to_string(), src, px });
        // 预取相邻格（同一分区：左右、上下两行）：扫过时下一格多半已解析好
        let Some(&(si, ri)) = self.index.get(key).and_then(|v| v.first()) else { return };
        let Some(ui) = ui() else { return };
        let Some(sec) = ui.get_emoji_sections().row_data(si) else { return };
        let n = sec.cells.row_count() as i64;
        let c = self.cols.max(1) as i64;
        let r = ri as i64;
        let mut want: Vec<(String, PosterSrc, u32)> = Vec::new();
        // 越靠后越先做
        for d in [c + 1, c - 1, -c, c, -1, 1] {
            let j = r + d;
            if j < 0 || j >= n || (d.abs() == 1 && j / c != r / c) {
                continue;
            }
            if let Some(cell) = sec.cells.row_data(j as usize) {
                // 贴纸格要读文件 + 解压：只预取表情
                if cell.sticker {
                    continue;
                }
                if let Some(src) = self.hover_src(cell.key.as_str()) {
                    want.push((cell.key.to_string(), src, px));
                }
            }
        }
        let _ = self.eng.send(Cmd::Prefetch(want));
    }

    /// Lottie 线程送来的一帧：还停在这格才换图（第一帧到之前一直显示静态首帧）
    fn hover_frame(&mut self, key: &str, px: u32, buf: Vec<u8>) {
        if self.hover.as_deref() != Some(key) || !self.open {
            return;
        }
        if let Some(ui) = ui() {
            ui.set_emoji_hover_img(image_from(&buf, px));
            if ui.get_emoji_hovered().as_str() != key {
                ui.set_emoji_hovered(key.into());
            }
        }
    }
}

impl crate::App {
    /// 海报到了：刷新还没有图的贴纸行
    pub(crate) fn refresh_stickers(&mut self) {
        let ids: Vec<usize> = (0..self.msgs.len()).filter(|&i| self.sticker_of(&self.msgs[i]).is_some()).collect();
        for i in ids {
            if self.emoji.chat_live.contains(&self.msgs[i].id) {
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

fn ui_weak_get() -> Option<PanelWindow> {
    ui()
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
        if open {
            crate::compose::start(2);
        }
        with_app(|app| app.emoji.opened(open));
    });
    w.on_emoji_cols_changed(|cols| {
        with_app(|app| {
            app.emoji.cols = cols.max(1) as usize;
            if app.emoji.open {
                app.emoji.rebuild_sections();
            }
        });
    });
    w.on_emoji_jump_to(|y| {
        // scroller.scrollTo({ top: offsetTop - 4, behavior: "smooth" })
        let Some(ui) = ui() else { return };
        let from = ui.get_emoji_scroll();
        let to = -y.max(0.0);
        crate::smooth_scroll("emoji", from, to, move |v| {
            if let Some(ui) = ui_weak_get() {
                ui.set_emoji_scroll(v);
            }
        });
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
            let had_query = !app.emoji.query.is_empty();
            app.emoji.query.clear();
            let Some(ui) = ui() else { return };
            ui.set_emoji_query("".into());
            ui.set_emoji_tab(tab.clone());
            if had_query {
                app.emoji.rebuild_sections();
            }
        });
    });
    w.on_emoji_import(|| {
        with_app(|app| {
            app.emoji.importing = true;
            app.dialog_open_pub(true);
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
