//! 动态表情 / 贴纸消息的 toast：胶囊里的头像位换成一个大号动态表情，它在胶囊鼓起后「蹦」出来
//! （弹簧 0.5 s、回弹 0.45，比胶囊本身更有弹性），然后一直循环播放到 toast 收回。
//! 两个平台共用：Windows 的 Direct2D toast（dock/toast.rs）和 Linux 的 Slint toast（toast_wl.rs）。

use std::time::Instant;

use crate::lottie::Player;
use crate::spring::Spring;

/// 表情的逻辑尺寸（胶囊高 48，表情上下各探出一点）
pub const SIZE: f64 = 56.0;
/// 表情在胶囊里占的槽宽（比头像 32 宽，表情左右也略探出）
pub const SLOT: f64 = 44.0;
/// 胶囊开始鼓起之后多久表情才蹦出来（秒）
const DELAY: f64 = 0.12;
const POP: Spring = Spring::new(0.5, 0.45);

pub struct Pop {
    player: Player,
    t0: Instant,
}

impl Pop {
    /// `px`：渲染像素（逻辑尺寸 × 缩放，再留一点给回弹时的放大）
    pub fn new(json: &str, px: u32) -> Option<Pop> {
        let player = Player::new(json, px)?;
        Some(Pop { player, t0: Instant::now() })
    }

    pub fn px(&self) -> u32 {
        self.player.px()
    }

    /// 当前缩放（0 → 1，带回弹）
    pub fn scale(&self) -> f64 {
        POP.at(self.t0.elapsed().as_secs_f64() - DELAY).max(0.0)
    }

    /// 当前帧（预乘 RGBA，px × px × 4），按动画自身帧率循环
    pub fn frame_rgba(&mut self) -> Vec<u8> {
        let total = self.player.total.max(1.0);
        let f = (self.t0.elapsed().as_secs_f32() * self.player.fps()) % total;
        self.player.render_raw(f)
    }
}

/// 收到的消息 → 要在 toast 里播的 Lottie（动态表情、贴纸；整条文字就是一个表情时也算）
pub fn lottie_for(node: &lancat_core::Node, m: &lancat_core::Message) -> Option<String> {
    match m.kind.as_str() {
        "emoji" => {
            let e: lancat_core::EmojiPayload = serde_json::from_value(m.body.clone()).ok()?;
            crate::emoji::lottie_data(node, &e)
        }
        "text" => {
            let t = m.body.get("text").and_then(|v| v.as_str())?.trim();
            let e = crate::emoji::by_char(t)?;
            crate::emoji::lottie_data(node, &lancat_core::EmojiPayload { kind: "noto".into(), code: e.code.into(), alt: e.ch.into(), data: None })
        }
        _ => None,
    }
}
