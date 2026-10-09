//! SQLite persistence: devices, one conversation per device, messages, settings.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::protocol::DeviceInfo;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    pub peer_id: String,
    pub outgoing: bool,
    /// "text" | "emoji" | "file"
    pub kind: String,
    /// Kind specific JSON (text: {text}, emoji: EmojiPayload, file: FileBody)
    pub body: serde_json::Value,
    /// outgoing: pending | sent | delivered | transferring | done | failed
    /// incoming: received | receiving | done | failed
    pub status: String,
    pub ts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FileBody {
    pub name: String,
    pub size: u64,
    pub transfer_id: String,
    /// Source path (outgoing) or final saved path (incoming).
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceRow {
    pub id: String,
    pub name: String,
    pub os: String,
    pub ver: String,
    pub last_addr: String,
    pub port: u16,
    pub last_seen: i64,
    pub blocked: bool,
    pub unread: i64,
    pub last_ts: i64,
    pub last_preview: String,
}

/// Persisted user settings (stored as one JSON blob).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Display name; empty = host name.
    pub device_name: String,
    /// Receive directory; empty = system Downloads.
    pub receive_dir: String,
    /// "left" | "right" | "top" | "bottom"
    pub dock_edge: String,
    /// 0.0..=1.0 position of the strip's centre along the edge.
    pub dock_offset: f64,
    /// Strip length in logical px.
    pub dock_length: f64,
    /// Monitor index (0 = primary).
    pub dock_monitor: usize,
    pub hover_delay_ms: u64,
    pub collapse_delay_ms: u64,
    /// 0 = unlimited.
    pub max_file_mb: u64,
    pub blocked_ips: Vec<String>,
    /// "ip[:port]" peers probed every heartbeat (networks that block multicast/broadcast).
    pub manual_peers: Vec<String>,
    pub theme: String,
    pub recent_emoji: Vec<String>,
    /// Seconds a collapsed chat panel (its WebView) is kept alive before it is destroyed to free
    /// memory; 0 = keep it resident (instant re-open, ~100 MB more on Linux).
    pub panel_keep_alive_s: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device_name: String::new(),
            receive_dir: String::new(),
            dock_edge: "right".into(),
            dock_offset: 0.5,
            dock_length: 160.0,
            dock_monitor: 0,
            hover_delay_ms: 150,
            collapse_delay_ms: 800,
            max_file_mb: 0,
            blocked_ips: vec![],
            manual_peers: vec![],
            theme: "system".into(),
            recent_emoji: vec![],
            panel_keep_alive_s: 60,
        }
    }
}

pub struct Store {
    conn: Mutex<Connection>,
    pub path: PathBuf,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn, path.to_path_buf())
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, PathBuf::from(":memory:"))
    }

    fn init(conn: Connection, path: PathBuf) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS devices(
                id TEXT PRIMARY KEY, name TEXT NOT NULL, os TEXT NOT NULL DEFAULT '', ver TEXT NOT NULL DEFAULT '',
                last_addr TEXT NOT NULL DEFAULT '', port INTEGER NOT NULL DEFAULT 0, last_seen INTEGER NOT NULL DEFAULT 0,
                blocked INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS conversations(
                peer_id TEXT PRIMARY KEY, unread INTEGER NOT NULL DEFAULT 0, last_ts INTEGER NOT NULL DEFAULT 0,
                last_preview TEXT NOT NULL DEFAULT '');
            CREATE TABLE IF NOT EXISTS messages(
                id TEXT PRIMARY KEY, peer_id TEXT NOT NULL, outgoing INTEGER NOT NULL, kind TEXT NOT NULL,
                body TEXT NOT NULL, status TEXT NOT NULL, ts INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS idx_messages_peer_ts ON messages(peer_id, ts);
            CREATE TABLE IF NOT EXISTS kv(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            "#,
        )?;
        Ok(Self { conn: Mutex::new(conn), path })
    }

    fn c(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---------- kv / settings ----------
    pub fn kv_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.c().query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0)).optional()?)
    }
    pub fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        self.c().execute(
            "INSERT INTO kv(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
    pub fn settings(&self) -> Settings {
        self.kv_get("settings").ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        self.kv_set("settings", &serde_json::to_string(s)?)
    }

    // ---------- devices ----------
    pub fn upsert_device(&self, d: &DeviceInfo, addr: &str, seen: i64) -> Result<()> {
        self.c().execute(
            "INSERT INTO devices(id,name,os,ver,last_addr,port,last_seen) VALUES(?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, os=excluded.os, ver=excluded.ver,
               last_addr=CASE WHEN excluded.last_addr='' THEN devices.last_addr ELSE excluded.last_addr END,
               port=excluded.port, last_seen=excluded.last_seen",
            params![d.id, d.name, d.os, d.ver, addr, d.port, seen],
        )?;
        Ok(())
    }

    pub fn set_blocked(&self, id: &str, blocked: bool) -> Result<()> {
        self.c().execute("UPDATE devices SET blocked=?2 WHERE id=?1", params![id, blocked as i64])?;
        Ok(())
    }

    pub fn is_blocked(&self, id: &str) -> bool {
        self.c()
            .query_row("SELECT blocked FROM devices WHERE id=?1", [id], |r| r.get::<_, i64>(0))
            .map(|b| b != 0)
            .unwrap_or(false)
    }

    pub fn blocked_addrs(&self) -> Vec<String> {
        let c = self.c();
        let mut st = match c.prepare("SELECT last_addr FROM devices WHERE blocked=1 AND last_addr<>''") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        st.query_map([], |r| r.get(0)).map(|it| it.flatten().collect()).unwrap_or_default()
    }

    pub fn devices(&self) -> Result<Vec<DeviceRow>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT d.id,d.name,d.os,d.ver,d.last_addr,d.port,d.last_seen,d.blocked,
                    COALESCE(c.unread,0),COALESCE(c.last_ts,0),COALESCE(c.last_preview,'')
             FROM devices d LEFT JOIN conversations c ON c.peer_id=d.id
             ORDER BY COALESCE(c.last_ts,0) DESC, d.name",
        )?;
        let rows = st.query_map([], |r| {
            Ok(DeviceRow {
                id: r.get(0)?,
                name: r.get(1)?,
                os: r.get(2)?,
                ver: r.get(3)?,
                last_addr: r.get(4)?,
                port: r.get::<_, i64>(5)? as u16,
                last_seen: r.get(6)?,
                blocked: r.get::<_, i64>(7)? != 0,
                unread: r.get(8)?,
                last_ts: r.get(9)?,
                last_preview: r.get(10)?,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn device(&self, id: &str) -> Option<DeviceRow> {
        self.devices().ok()?.into_iter().find(|d| d.id == id)
    }

    pub fn forget_device(&self, id: &str) -> Result<()> {
        let c = self.c();
        c.execute("DELETE FROM messages WHERE peer_id=?1", [id])?;
        c.execute("DELETE FROM conversations WHERE peer_id=?1", [id])?;
        c.execute("DELETE FROM devices WHERE id=?1", [id])?;
        Ok(())
    }

    // ---------- messages ----------
    /// Inserts a message (idempotent on id). Returns true when it was new.
    pub fn insert_message(&self, m: &Message, count_unread: bool) -> Result<bool> {
        let c = self.c();
        let n = c.execute(
            "INSERT OR IGNORE INTO messages(id,peer_id,outgoing,kind,body,status,ts) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![m.id, m.peer_id, m.outgoing as i64, m.kind, m.body.to_string(), m.status, m.ts],
        )?;
        if n > 0 {
            c.execute(
                "INSERT INTO conversations(peer_id,unread,last_ts,last_preview) VALUES(?1,?2,?3,?4)
                 ON CONFLICT(peer_id) DO UPDATE SET unread=conversations.unread+excluded.unread,
                   last_ts=MAX(conversations.last_ts, excluded.last_ts), last_preview=excluded.last_preview",
                params![m.peer_id, count_unread as i64, m.ts, preview(m)],
            )?;
        }
        Ok(n > 0)
    }

    pub fn update_message(&self, id: &str, status: &str, body: Option<&serde_json::Value>) -> Result<Option<Message>> {
        {
            let c = self.c();
            match body {
                Some(b) => c.execute("UPDATE messages SET status=?2, body=?3 WHERE id=?1", params![id, status, b.to_string()])?,
                None => c.execute("UPDATE messages SET status=?2 WHERE id=?1", params![id, status])?,
            };
        }
        self.message(id)
    }

    pub fn message(&self, id: &str) -> Result<Option<Message>> {
        Ok(self
            .c()
            .query_row("SELECT id,peer_id,outgoing,kind,body,status,ts FROM messages WHERE id=?1", [id], row_to_msg)
            .optional()?)
    }

    /// Messages of one conversation, newest `limit` before `before_ts` (exclusive), returned oldest first.
    pub fn messages(&self, peer_id: &str, before_ts: Option<i64>, limit: usize) -> Result<Vec<Message>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT id,peer_id,outgoing,kind,body,status,ts FROM messages
             WHERE peer_id=?1 AND ts < ?2 ORDER BY ts DESC LIMIT ?3",
        )?;
        let mut v: Vec<Message> = st
            .query_map(params![peer_id, before_ts.unwrap_or(i64::MAX), limit as i64], row_to_msg)?
            .flatten()
            .collect();
        v.reverse();
        Ok(v)
    }

    /// Outgoing messages that never got acked (to resend when the link comes up).
    pub fn unacked_outgoing(&self, peer_id: &str) -> Result<Vec<Message>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT id,peer_id,outgoing,kind,body,status,ts FROM messages
             WHERE peer_id=?1 AND outgoing=1 AND kind IN ('text','emoji') AND status IN ('pending','sent') ORDER BY ts",
        )?;
        let v = st.query_map([peer_id], row_to_msg)?.flatten().collect();
        Ok(v)
    }

    /// Outgoing file transfers that failed / were interrupted.
    pub fn unfinished_files(&self, peer_id: &str) -> Result<Vec<Message>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT id,peer_id,outgoing,kind,body,status,ts FROM messages
             WHERE peer_id=?1 AND outgoing=1 AND kind='file' AND status IN ('transferring','failed','pending') ORDER BY ts",
        )?;
        let v = st.query_map([peer_id], row_to_msg)?.flatten().collect();
        Ok(v)
    }

    /// On startup, anything "in flight" is really interrupted.
    pub fn mark_interrupted(&self) -> Result<()> {
        let c = self.c();
        c.execute("UPDATE messages SET status='failed' WHERE kind='file' AND status IN ('transferring','receiving')", [])?;
        Ok(())
    }

    pub fn mark_read(&self, peer_id: &str) -> Result<()> {
        self.c().execute("UPDATE conversations SET unread=0 WHERE peer_id=?1", [peer_id])?;
        Ok(())
    }

    pub fn clear_history(&self, peer_id: Option<&str>) -> Result<()> {
        let c = self.c();
        match peer_id {
            Some(p) => {
                c.execute("DELETE FROM messages WHERE peer_id=?1", [p])?;
                c.execute("UPDATE conversations SET unread=0,last_preview='' WHERE peer_id=?1", [p])?;
            }
            None => {
                c.execute("DELETE FROM messages", [])?;
                c.execute("UPDATE conversations SET unread=0,last_preview=''", [])?;
            }
        }
        Ok(())
    }

    pub fn delete_message(&self, id: &str) -> Result<()> {
        self.c().execute("DELETE FROM messages WHERE id=?1", [id])?;
        Ok(())
    }
}

fn row_to_msg(r: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let body: String = r.get(4)?;
    Ok(Message {
        id: r.get(0)?,
        peer_id: r.get(1)?,
        outgoing: r.get::<_, i64>(2)? != 0,
        kind: r.get(3)?,
        body: serde_json::from_str(&body).unwrap_or(serde_json::Value::Null),
        status: r.get(5)?,
        ts: r.get(6)?,
    })
}

pub fn preview(m: &Message) -> String {
    match m.kind.as_str() {
        "text" => m.body.get("text").and_then(|t| t.as_str()).unwrap_or("").chars().take(60).collect(),
        "emoji" => m.body.get("alt").and_then(|t| t.as_str()).filter(|s| !s.is_empty()).unwrap_or("[表情]").to_string(),
        "file" => format!("[文件] {}", m.body.get("name").and_then(|t| t.as_str()).unwrap_or("")),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str) -> DeviceInfo {
        DeviceInfo { id: id.into(), name: format!("dev-{id}"), os: "linux".into(), ver: "0.1.0".into(), port: 47820 }
    }

    fn text(id: &str, peer: &str, ts: i64) -> Message {
        Message {
            id: id.into(),
            peer_id: peer.into(),
            outgoing: false,
            kind: "text".into(),
            body: serde_json::json!({"text": format!("hi {id}")}),
            status: "received".into(),
            ts,
        }
    }

    #[test]
    fn devices_and_conversations() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_device(&dev("a"), "192.168.1.2", 1).unwrap();
        s.upsert_device(&dev("b"), "192.168.1.3", 1).unwrap();
        s.upsert_device(&dev("a"), "", 2).unwrap(); // keeps address
        assert_eq!(s.devices().unwrap().len(), 2);
        assert_eq!(s.device("a").unwrap().last_addr, "192.168.1.2");

        assert!(s.insert_message(&text("m1", "a", 10), true).unwrap());
        assert!(!s.insert_message(&text("m1", "a", 10), true).unwrap()); // idempotent
        assert!(s.insert_message(&text("m2", "a", 20), true).unwrap());
        let a = s.device("a").unwrap();
        assert_eq!(a.unread, 2);
        assert_eq!(a.last_preview, "hi m2");
        assert_eq!(s.devices().unwrap()[0].id, "a"); // most recent conversation first
        s.mark_read("a").unwrap();
        assert_eq!(s.device("a").unwrap().unread, 0);

        let msgs = s.messages("a", None, 10).unwrap();
        assert_eq!(msgs.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["m1", "m2"]);
        assert_eq!(s.messages("a", Some(20), 10).unwrap().len(), 1);

        let m = s.update_message("m1", "done", None).unwrap().unwrap();
        assert_eq!(m.status, "done");

        s.clear_history(None).unwrap();
        assert!(s.messages("a", None, 10).unwrap().is_empty());
    }

    #[test]
    fn blocking_and_settings() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_device(&dev("x"), "10.0.0.9", 1).unwrap();
        assert!(!s.is_blocked("x"));
        s.set_blocked("x", true).unwrap();
        assert!(s.is_blocked("x"));
        assert_eq!(s.blocked_addrs(), vec!["10.0.0.9".to_string()]);

        let mut st = s.settings();
        assert_eq!(st, Settings::default());
        st.dock_edge = "left".into();
        st.collapse_delay_ms = 1200;
        s.save_settings(&st).unwrap();
        assert_eq!(s.settings().dock_edge, "left");
        assert_eq!(s.settings().collapse_delay_ms, 1200);
    }

    #[test]
    fn unacked() {
        let s = Store::open_in_memory().unwrap();
        let mut m = text("o1", "p", 1);
        m.outgoing = true;
        m.status = "pending".into();
        s.insert_message(&m, false).unwrap();
        assert_eq!(s.unacked_outgoing("p").unwrap().len(), 1);
        s.update_message("o1", "delivered", None).unwrap();
        assert!(s.unacked_outgoing("p").unwrap().is_empty());
    }
}
