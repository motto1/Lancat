//! The dock strip as a native window (no WebView): a few pixels of always-on-top chrome that
//! senses hover / file drags and shows the unread state. Replacing the old WebView strip removed
//! a whole WebKitWebProcess (~96 MB PSS on Linux) from the idle footprint — see
//! docs/postmortem-memory.md.
//!
//! Every backend exposes the same four functions, all called on the main (UI) thread:
//! `create`, `place`, `show`, `set_visual`. Input is reported back through
//! [`crate::dock::open_panel`], [`crate::dock::on_hover_start`] and [`crate::dock::on_hover_end`].
//!
//! * Linux: GTK3 window + cairo (layer-shell on Wayland, Dock type hint on X11) — verified on niri.
//! * Windows: Win32 layered tool window, per-pixel alpha — verified on Win11 (150 %).
//! * macOS: borderless non-activating NSPanel + NSView — TODO-verify (not compiled on macOS yet).

use tauri::{AppHandle, Manager};

pub use crate::dock::Rect;

/// What the strip shows.
#[derive(Clone, Debug, PartialEq)]
pub struct DockVisual {
    /// "left" | "right" | "top" | "bottom"
    pub edge: String,
    /// At least one unread message: accent colour + slow pulse.
    pub unread: bool,
    /// The panel is on screen (full opacity, no idle inset).
    pub open: bool,
    /// Accent colour, 0..1 sRGB.
    pub accent: (f64, f64, f64),
    /// Dark app theme (Windows strip colours; the other backends ignore it).
    pub dark: bool,
}

impl Default for DockVisual {
    fn default() -> Self {
        // macOS system blue, same as the CSS --accent default
        Self { edge: "right".into(), unread: false, open: false, accent: (10.0 / 255.0, 132.0 / 255.0, 1.0), dark: false }
    }
}

pub(crate) fn hover_delay(app: &AppHandle) -> u64 {
    app.state::<crate::commands::Core>().0.settings().hover_delay_ms
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod linux;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub use self::linux::{create, place, set_visual, show};
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod linux_toast;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub use self::linux_toast::{dismiss as toast_dismiss, show as toast_show, shown as toast_shown, theme_is_dark, ToastSpec};

#[cfg(target_os = "windows")]
mod win_drop;
#[cfg(target_os = "windows")]
mod win_toast;
#[cfg(target_os = "windows")]
pub use self::win_toast::{dismiss as toast_dismiss, show as toast_show, shown as toast_shown, ToastSpec};
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use self::windows::{create, place, set_visual, show};
#[cfg(target_os = "windows")]
pub use self::win_drop::{overlay_hide, overlay_show, overlay_shown};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::{create, place, set_visual, show};
