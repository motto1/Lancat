//! Tauri IPC commands: thin wrappers around `lancat_core::Node`.

use std::path::PathBuf;
use std::sync::Arc;

use lancat_core::{DeviceInfo, EmojiPayload, Message, Node, PeerView, Settings};
use serde::Serialize;
use tauri::{AppHandle, State};

pub struct Core(pub Arc<Node>);

type R<T> = Result<T, String>;

fn e<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}

/// Frontend diagnostics end up in the app log (RUST_LOG=debug to see them).
#[tauri::command]
pub fn ui_log(level: String, msg: String) {
    if level == "warn" {
        log::warn!("[ui] {msg}");
    } else {
        log::debug!("[ui] {msg}");
    }
}

#[tauri::command]
pub fn get_me(core: State<'_, Core>) -> DeviceInfo {
    core.0.me()
}

#[tauri::command]
pub fn list_peers(core: State<'_, Core>) -> Vec<PeerView> {
    core.0.peers()
}

/// Manual refresh: one discovery probe burst on every interface (LAN + virtual LANs).
#[tauri::command]
pub fn refresh_peers(core: State<'_, Core>) {
    core.0.refresh_peers()
}

#[tauri::command]
pub fn get_messages(core: State<'_, Core>, peer_id: String, before_ts: Option<i64>, limit: Option<usize>) -> R<Vec<Message>> {
    core.0.messages(&peer_id, before_ts, limit.unwrap_or(80)).map_err(e)
}

#[tauri::command]
pub async fn send_text(core: State<'_, Core>, peer_ids: Vec<String>, text: String) -> R<Vec<Message>> {
    let mut out = vec![];
    for p in peer_ids {
        out.push(core.0.send_text(&p, &text).await.map_err(e)?);
    }
    Ok(out)
}

#[tauri::command]
pub async fn send_emoji(core: State<'_, Core>, peer_ids: Vec<String>, emoji: EmojiPayload) -> R<Vec<Message>> {
    let mut out = vec![];
    for p in peer_ids {
        out.push(core.0.send_emoji(&p, emoji.clone()).await.map_err(e)?);
    }
    Ok(out)
}

#[tauri::command]
pub async fn send_files(core: State<'_, Core>, peer_ids: Vec<String>, paths: Vec<String>) -> R<Vec<Message>> {
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    core.0.send_files(&peer_ids, &paths).await.map_err(e)
}

#[tauri::command]
pub fn send_typing(core: State<'_, Core>, peer_id: String, typing: bool) {
    core.0.send_typing(&peer_id, typing)
}

#[tauri::command]
pub fn mark_read(core: State<'_, Core>, peer_id: String) -> R<()> {
    core.0.mark_read(&peer_id).map_err(e)
}

#[tauri::command]
pub async fn retry_transfer(core: State<'_, Core>, msg_id: String) -> R<()> {
    core.0.retry_transfer(&msg_id).await.map_err(e)
}

#[tauri::command]
pub fn clear_history(core: State<'_, Core>, peer_id: Option<String>) -> R<()> {
    core.0.clear_history(peer_id.as_deref()).map_err(e)
}

#[tauri::command]
pub fn delete_message(core: State<'_, Core>, id: String) -> R<()> {
    core.0.delete_message(&id).map_err(e)
}

#[tauri::command]
pub fn get_settings(core: State<'_, Core>) -> Settings {
    core.0.settings()
}

#[tauri::command]
pub fn set_settings(app: AppHandle, core: State<'_, Core>, settings: Settings) -> R<Settings> {
    let s = core.0.set_settings(settings).map_err(e)?;
    crate::dock::apply_layout(&app);
    // the native material's tint follows the theme setting
    #[cfg(windows)]
    if let Some(p) = tauri::Manager::get_webview_window(&app, "panel") {
        crate::platform::refresh_material(&p);
    }
    Ok(s)
}

#[tauri::command]
pub fn set_blocked(core: State<'_, Core>, peer_id: String, blocked: bool) -> R<()> {
    core.0.set_blocked(&peer_id, blocked).map_err(e)
}

#[tauri::command]
pub fn forget_peer(core: State<'_, Core>, peer_id: String) -> R<()> {
    core.0.forget_peer(&peer_id).map_err(e)
}

#[tauri::command]
pub async fn add_peer(core: State<'_, Core>, addr: String) -> R<PeerView> {
    core.0.add_peer_addr(&addr).await.map_err(e)
}

#[tauri::command]
pub fn receive_dir(core: State<'_, Core>) -> String {
    core.0.receive_dir().to_string_lossy().to_string()
}

#[tauri::command]
pub fn is_transferring(core: State<'_, Core>) -> bool {
    core.0.is_transferring()
}

/// Reveals a received file in the system file manager. Received files are never opened/executed.
#[tauri::command]
pub fn reveal_file(path: String) -> R<()> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err("文件不存在".into());
    }
    tauri_plugin_opener::reveal_item_in_dir(p).map_err(e)
}

// ------------------------------------------------------------------ stickers (.tgs / Lottie)

#[derive(Serialize)]
pub struct StickerMeta {
    pub id: String,
    pub name: String,
}

fn sticker_dir(core: &Core) -> PathBuf {
    core.0.data_dir().join("stickers")
}

#[tauri::command]
pub fn list_stickers(core: State<'_, Core>) -> Vec<StickerMeta> {
    let dir = sticker_dir(&core);
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

/// Saves a Lottie JSON (already gunzipped from .tgs by the frontend). Max 1 MiB.
#[tauri::command]
pub fn import_sticker(core: State<'_, Core>, name: String, lottie: String) -> R<StickerMeta> {
    if lottie.len() > 1 << 20 {
        return Err("贴纸太大（>1MB）".into());
    }
    let v: serde_json::Value = serde_json::from_str(&lottie).map_err(|_| "不是有效的 Lottie JSON".to_string())?;
    if v.get("layers").is_none() {
        return Err("不是有效的 Lottie 动画".into());
    }
    let dir = sticker_dir(&core);
    std::fs::create_dir_all(&dir).map_err(e)?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    std::fs::write(dir.join(format!("{id}.json")), &lottie).map_err(e)?;
    let name: String = name.chars().take(64).collect();
    std::fs::write(dir.join(format!("{id}.name")), &name).map_err(e)?;
    Ok(StickerMeta { id, name })
}

#[tauri::command]
pub fn get_sticker(core: State<'_, Core>, id: String) -> R<String> {
    if !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("bad id".into());
    }
    std::fs::read_to_string(sticker_dir(&core).join(format!("{id}.json"))).map_err(e)
}

#[tauri::command]
pub fn delete_sticker(core: State<'_, Core>, id: String) -> R<()> {
    if !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("bad id".into());
    }
    let dir = sticker_dir(&core);
    let _ = std::fs::remove_file(dir.join(format!("{id}.name")));
    std::fs::remove_file(dir.join(format!("{id}.json"))).map_err(e)
}
