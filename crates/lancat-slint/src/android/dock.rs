//! Android 没有停靠条 / 托盘 / 开机自启：与 dock（Windows）/ dock_wl（Linux）同一套对外接口，大多是空操作。
//! 收起面板 = 回到桌面（Activity 退到后台，节点和前台服务继续运行）；提醒走系统通知（见 notify.rs）。

pub const DIAG_TOAST: &str = "__diag__";
pub const UPDATE_TOAST: &str = "__update__";

pub fn refresh_visual() {}
pub fn on_incoming(_m: &lancat_core::Message) {}
pub fn apply_layout() {}
pub fn note_typed() {}
pub fn pinned() -> bool {
    false
}
pub fn set_pinned(_p: bool) {}
pub fn begin_move(_from_strip: bool) {}
#[allow(dead_code)]
pub fn open_panel(_reason: &str) {}
pub fn hide_panel() {
    super::jvm::move_to_back();
}
pub fn monitor_labels() -> Vec<String> {
    Vec::new()
}

/// 诊断发现问题：应用不在前台时发一条通知（点开进设置的「权限与诊断」）
pub fn on_diag_alert(title: &str, detail: &str) {
    if !super::jvm::is_foreground() {
        super::jvm::notify_message(DIAG_TOAST, title, detail);
    }
}

pub fn on_update_alert(_title: &str) {}

pub mod tray {
    pub fn test_instance() -> bool {
        true
    }
    pub fn autostart_enabled() -> bool {
        false
    }
    pub fn set_autostart(_on: bool) {}
    #[allow(dead_code)]
    pub fn remove() {}
}
