//! 新消息提示音：程序里合成的一声短促“叮咚”（两个正弦音 A5 → E6，带一点泛音，指数衰减，约 0.45 s），
//! 不带任何音频文件，也没有版权问题。
//! - Windows：PlaySoundW(SND_MEMORY | SND_ASYNC)，直接播内存里的 WAV；
//! - Linux：WAV 写到 $XDG_RUNTIME_DIR 一次，之后用 pw-play / paplay / aplay 里找到的第一个异步播放。
//! 1.2 s 内只响一次（一连串消息不会响成一片）。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const RATE: u32 = 44_100;

fn wav() -> &'static [u8] {
    static W: OnceLock<Vec<u8>> = OnceLock::new();
    W.get_or_init(|| {
        let n = (RATE as f32 * 0.48) as usize;
        let mut pcm = vec![0f32; n];
        // (起始秒, 基频, 音量)
        for (t0, f, amp) in [(0.0f32, 880.0f32, 0.30f32), (0.085, 1318.51, 0.26)] {
            let s0 = (t0 * RATE as f32) as usize;
            for i in s0..n {
                let t = (i - s0) as f32 / RATE as f32;
                let attack = (t / 0.004).min(1.0);
                let env = attack * (-t / 0.11).exp();
                let w = std::f32::consts::TAU * f * t;
                pcm[i] += amp * env * (w.sin() + 0.18 * (2.0 * w).sin() + 0.05 * (3.0 * w).sin());
            }
        }
        // 结尾 20 ms 淡出，避免“咔”一声
        let tail = (RATE as f32 * 0.02) as usize;
        for k in 0..tail {
            pcm[n - 1 - k] *= k as f32 / tail as f32;
        }
        let data_len = (n * 2) as u32;
        let mut v = Vec::with_capacity(44 + n * 2);
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&1u16.to_le_bytes()); // mono
        v.extend_from_slice(&RATE.to_le_bytes());
        v.extend_from_slice(&(RATE * 2).to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        for s in pcm {
            v.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
        v
    })
}

/// 播放提示音（不阻塞；1.2 s 内重复调用会被忽略）。
pub fn message() {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    {
        let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if l.is_some_and(|t| t.elapsed() < Duration::from_millis(1200)) {
            return;
        }
        *l = Some(Instant::now());
    }
    if std::env::var("LANCAT_SLINT_MUTE").is_ok_and(|v| v == "1") {
        return;
    }
    play(wav());
}

#[cfg(windows)]
fn play(w: &'static [u8]) {
    use windows::core::PCWSTR;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
    // SND_MEMORY：第一个参数其实是指向 WAV 数据的指针（数据是 'static，异步播放期间一直有效）
    let ok = unsafe { PlaySoundW(PCWSTR(w.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT) };
    if !ok.as_bool() {
        log::debug!("PlaySoundW failed");
    }
}

#[cfg(target_os = "android")]
fn play(_w: &'static [u8]) {
    std::thread::spawn(crate::android::jvm::play_chime);
}

#[cfg(not(any(windows, target_os = "android")))]
fn play(w: &'static [u8]) {
    std::thread::spawn(move || {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let path = dir.join("lancat-message.wav");
        if std::fs::metadata(&path).map(|m| m.len() as usize != w.len()).unwrap_or(true) && std::fs::write(&path, w).is_err() {
            return;
        }
        for (bin, args) in [("pw-play", &["--media-role=Notification"][..]), ("paplay", &[][..]), ("aplay", &["-q"][..])] {
            let r = std::process::Command::new(bin).args(args).arg(&path).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
            match r {
                Ok(s) if s.success() => return,
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        log::debug!("no player for the message sound (pw-play / paplay / aplay)");
    });
}
