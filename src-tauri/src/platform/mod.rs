//! Platform-specific window material, drag sensing and first-run tweaks, isolated by `cfg`.
//! Only the Linux path is verified on CI/dev machines; Windows and macOS are marked TODO-verify.

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use self::windows::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::*;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod linux;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub use self::linux::*;
