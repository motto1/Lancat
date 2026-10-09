//! File helpers: safe receive paths, unique names, positional IO, hashing, resumable incoming transfers.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

use crate::protocol::FileManifest;

pub const PART_DIR: &str = ".lancat-part";

/// Turns an untrusted relative path ("a/b/c.txt") into a safe relative PathBuf.
/// Drops "..", roots, drive prefixes and characters that are invalid on Windows.
pub fn sanitize_rel_path(name: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for part in name.split(['/', '\\']) {
        let p = Path::new(part);
        let ok = matches!(p.components().next(), Some(Component::Normal(_)));
        if !ok || part == "." || part == ".." {
            continue;
        }
        let cleaned: String = part
            .chars()
            .map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c })
            .collect();
        let cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
        if cleaned.is_empty() {
            continue;
        }
        out.push(cleaned);
    }
    if out.as_os_str().is_empty() {
        out.push("unnamed");
    }
    out
}

/// "file.txt" -> "file (1).txt" until free.
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for i in 1.. {
        let cand = parent.join(format!("{stem} ({i}){ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    unreachable!()
}

pub fn write_at(f: &File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.write_all_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0usize;
        while done < buf.len() {
            let n = f.seek_write(&buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "write_at"));
            }
            done += n;
        }
        Ok(())
    }
}

pub fn read_at(f: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0usize;
        while done < buf.len() {
            let n = f.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "read_at"));
            }
            done += n;
        }
        Ok(())
    }
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_hex().to_string())
}

/// Makes sure a received file is never executable (unix); no-op elsewhere.
pub fn strip_exec(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o644));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Walks files and folders, producing (absolute path, relative name) pairs.
pub fn expand_paths(paths: &[PathBuf]) -> Vec<(PathBuf, String)> {
    fn walk(base: &Path, dir: &Path, prefix: &str, out: &mut Vec<(PathBuf, String)>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let name = format!("{prefix}/{}", e.file_name().to_string_lossy());
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                walk(base, &p, &name, out);
            } else if ft.is_file() {
                out.push((p, name));
            }
        }
    }
    let mut out = Vec::new();
    for p in paths {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "unnamed".into());
        if p.is_dir() {
            walk(p, p, &name, &mut out);
        } else if p.is_file() {
            out.push((p.clone(), name));
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Sidecar {
    manifest: FileManifest,
    have: BTreeSet<u64>,
}

/// Receiver side of a resumable transfer. Data goes to `<recv>/.lancat-part/<tid>.part`
/// and progress to `<tid>.json`; both survive restarts so the sender can resume.
pub struct IncomingTransfer {
    pub manifest: FileManifest,
    part_path: PathBuf,
    sidecar_path: PathBuf,
    file: File,
    have: Mutex<BTreeSet<u64>>,
}

impl IncomingTransfer {
    pub fn open(recv_dir: &Path, manifest: FileManifest) -> Result<Self> {
        if manifest.chunk_size == 0 || manifest.chunk_size > 64 * 1024 * 1024 {
            bail!("bad chunk size");
        }
        if manifest.transfer_id.is_empty() || !manifest.transfer_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            bail!("bad transfer id");
        }
        let dir = recv_dir.join(PART_DIR);
        fs::create_dir_all(&dir)?;
        let part_path = dir.join(format!("{}.part", manifest.transfer_id));
        let sidecar_path = dir.join(format!("{}.json", manifest.transfer_id));
        let mut have = BTreeSet::new();
        if let Ok(s) = fs::read_to_string(&sidecar_path) {
            if let Ok(sc) = serde_json::from_str::<Sidecar>(&s) {
                if sc.manifest.hash == manifest.hash && sc.manifest.size == manifest.size && sc.manifest.chunk_size == manifest.chunk_size && part_path.exists() {
                    have = sc.have;
                }
            }
        }
        let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&part_path)?;
        file.set_len(manifest.size)?;
        let t = Self { manifest, part_path, sidecar_path, file, have: Mutex::new(have) };
        t.persist()?;
        Ok(t)
    }

    fn persist(&self) -> Result<()> {
        let have = self.have.lock().unwrap().clone();
        let sc = Sidecar { manifest: self.manifest.clone(), have };
        fs::write(&self.sidecar_path, serde_json::to_vec(&sc)?)?;
        Ok(())
    }

    pub fn have(&self) -> Vec<u64> {
        self.have.lock().unwrap().iter().copied().collect()
    }

    pub fn received_bytes(&self) -> u64 {
        let have = self.have.lock().unwrap();
        have.iter().map(|&i| self.chunk_len(i)).sum()
    }

    pub fn chunk_len(&self, idx: u64) -> u64 {
        let start = idx * self.manifest.chunk_size;
        self.manifest.chunk_size.min(self.manifest.size.saturating_sub(start))
    }

    /// Writes one chunk (blocking). `hash` is the optional BLAKE3 hex of this chunk.
    pub fn write_chunk(&self, idx: u64, data: &[u8], hash: Option<&str>) -> Result<()> {
        if idx >= self.manifest.chunk_count() {
            bail!("chunk index out of range");
        }
        if data.len() as u64 != self.chunk_len(idx) {
            bail!("chunk {idx} has wrong length {} (expected {})", data.len(), self.chunk_len(idx));
        }
        if let Some(h) = hash {
            let got = blake3::hash(data).to_hex();
            if got.as_str() != h {
                bail!("chunk {idx} hash mismatch");
            }
        }
        write_at(&self.file, data, idx * self.manifest.chunk_size)?;
        self.have.lock().unwrap().insert(idx);
        self.persist()?;
        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        self.have.lock().unwrap().len() as u64 == self.manifest.chunk_count()
    }

    /// Verifies the BLAKE3 of the whole file and moves it into `recv_dir` under a unique name.
    pub fn finish(&self, recv_dir: &Path) -> Result<PathBuf> {
        if !self.is_complete() {
            bail!("transfer incomplete");
        }
        self.file.sync_all().ok();
        let got = hash_file(&self.part_path)?;
        if got != self.manifest.hash {
            // corrupt: forget progress so a retry starts over
            self.have.lock().unwrap().clear();
            self.persist().ok();
            return Err(anyhow!("file hash mismatch"));
        }
        let rel = sanitize_rel_path(&self.manifest.name);
        let target = unique_path(&recv_dir.join(rel));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&self.part_path, &target).or_else(|_| fs::copy(&self.part_path, &target).map(|_| ()).and_then(|_| fs::remove_file(&self.part_path)))?;
        strip_exec(&target);
        let _ = fs::remove_file(&self.sidecar_path);
        if let Some(dir) = self.sidecar_path.parent() {
            let _ = fs::remove_dir(dir); // only succeeds when no other transfer is pending
        }
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_rel_path("a/b.txt"), PathBuf::from("a").join("b.txt"));
        assert_eq!(sanitize_rel_path("../../etc/passwd"), PathBuf::from("etc").join("passwd"));
        assert_eq!(sanitize_rel_path("/abs/x"), PathBuf::from("abs").join("x"));
        assert_eq!(sanitize_rel_path("..\\..\\win.ini"), PathBuf::from("win.ini"));
        assert_eq!(sanitize_rel_path("a:b?.txt"), PathBuf::from("a_b_.txt"));
        assert_eq!(sanitize_rel_path(""), PathBuf::from("unnamed"));
        assert_eq!(sanitize_rel_path("trail. "), PathBuf::from("trail"));
    }

    #[test]
    fn unique() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.txt");
        assert_eq!(unique_path(&p), p);
        fs::write(&p, "1").unwrap();
        assert_eq!(unique_path(&p), d.path().join("x (1).txt"));
        fs::write(d.path().join("x (1).txt"), "1").unwrap();
        assert_eq!(unique_path(&p), d.path().join("x (2).txt"));
        let n = d.path().join("noext");
        fs::write(&n, "1").unwrap();
        assert_eq!(unique_path(&n), d.path().join("noext (1)"));
    }

    #[test]
    fn incoming_resume_and_finish() {
        let d = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let m = FileManifest {
            transfer_id: "t-1".into(),
            msg_id: "m".into(),
            from: "peer".into(),
            name: "dir/out.bin".into(),
            size: data.len() as u64,
            chunk_size: 4096,
            hash: blake3::hash(&data).to_hex().to_string(),
            ts: 0,
        };
        {
            let t = IncomingTransfer::open(d.path(), m.clone()).unwrap();
            assert_eq!(m.chunk_count(), 3);
            let h0 = blake3::hash(&data[..4096]).to_hex().to_string();
            assert!(t.write_chunk(0, &data[..4096], Some("bad")).is_err());
            t.write_chunk(0, &data[..4096], Some(&h0)).unwrap();
            assert!(t.write_chunk(1, &data[..10], None).is_err());
            assert!(!t.is_complete());
        }
        // "restart": progress survives
        let t = IncomingTransfer::open(d.path(), m.clone()).unwrap();
        assert_eq!(t.have(), vec![0]);
        t.write_chunk(2, &data[8192..], None).unwrap();
        t.write_chunk(1, &data[4096..8192], None).unwrap();
        assert_eq!(t.received_bytes(), data.len() as u64);
        let out = t.finish(d.path()).unwrap();
        assert_eq!(out, d.path().join("dir").join("out.bin"));
        assert_eq!(fs::read(&out).unwrap(), data);
    }

    #[test]
    fn expand() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("folder");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::write(root.join("sub/b.txt"), "b").unwrap();
        fs::write(d.path().join("single.txt"), "s").unwrap();
        let v = expand_paths(&[root.clone(), d.path().join("single.txt")]);
        let names: Vec<_> = v.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, vec!["folder/a.txt", "folder/sub/b.txt", "single.txt"]);
    }
}
