//! 旧版 CSS `line-height: normal` 的行框高度。
//!
//! Windows 上的数值是按旧版（Chromium + DirectWrite，CJK 回退到 Microsoft YaHei UI）1.5x 截图逐个量出来、
//! 直接写在 .slint 里的；这里原样返回调用方给的 Windows 值。
//!
//! Linux 上旧版是 WebKitGTK：字体按 `--font` 的族名顺序找第一个 fontconfig 精确命中的（这台机器上通常是
//! Segoe UI Variable 或 Noto Sans），中文字形回退到列表里第一个能显示中文的族（通常是 Noto Sans CJK SC）。
//! 行框 = 各字体的 ascent / descent 分别四舍五入后取最大再相加（WebKit `Font::platformInit` 的取整，
//! 行内有回退字形时行框被回退字体撑高）。Noto Sans CJK 的 ascent/descent 是 1.16 / 0.288 em，比 YaHei 高，
//! 所以同样的布局在 Linux 上每行要高 1~2 px。

#[cfg(target_os = "linux")]
mod imp {
    use std::sync::OnceLock;

    #[derive(Clone, Copy, Debug)]
    struct Fm {
        asc: f32,
        desc: f32,
    }

    struct Fonts {
        prim: Fm,
        cjk: Option<Fm>,
        /// 旧版拉丁字形所用的族（--font 里第一个 fontconfig 精确命中的）
        family: Option<String>,
    }

    /// 旧版 global.css 的 --font（同顺序）
    const FONT: &[&str] = &[
        "SF Pro Text",
        "SF Pro",
        "Segoe UI Variable Text",
        "Segoe UI Variable",
        "Segoe UI",
        "PingFang SC",
        "HarmonyOS Sans SC",
        "MiSans",
        "Noto Sans CJK SC",
        "Microsoft YaHei UI",
        "Inter",
        "Cantarell",
        "Ubuntu",
    ];

    fn fc(pattern: &str) -> Option<(String, String, usize)> {
        let out = std::process::Command::new("fc-match").args(["-f", "%{family}\t%{file}\t%{index}", pattern]).output().ok()?;
        let s = String::from_utf8_lossy(&out.stdout).to_string();
        let mut it = s.split('\t');
        let fam = it.next()?.to_string();
        let file = it.next()?.to_string();
        let idx = it.next().unwrap_or("0").trim().parse().unwrap_or(0);
        Some((fam, file, idx))
    }

    fn exact(fam: &str, extra: &str) -> Option<(String, usize)> {
        let (f, file, idx) = fc(&format!("{fam}{extra}"))?;
        f.split(',').any(|x| x.trim().eq_ignore_ascii_case(fam)).then_some((file, idx))
    }

    fn metrics(file: &str, idx: usize) -> Option<Fm> {
        let data = std::fs::read(file).ok()?;
        let f = swash::FontRef::from_index(&data, idx)?;
        let m = f.metrics(&[]);
        let u = m.units_per_em as f32;
        (u > 0.0).then(|| Fm { asc: m.ascent / u, desc: m.descent.abs() / u })
    }

    fn fonts() -> Option<&'static Fonts> {
        static F: OnceLock<Option<Fonts>> = OnceLock::new();
        F.get_or_init(|| {
            let hit = FONT.iter().find_map(|f| exact(f, "").map(|x| (f.to_string(), x)));
            let family = hit.as_ref().map(|(f, _)| f.clone());
            let prim = hit
                .map(|(_, x)| x)
                .or_else(|| fc("sans-serif").map(|(_, file, i)| (file, i)))
                .and_then(|(file, i)| metrics(&file, i))?;
            // 中文字形：列表里第一个精确命中且含「中」的族；都没有就是 fontconfig 的中文默认
            let cjk = FONT
                .iter()
                .find_map(|f| exact(f, ":charset=4e2d"))
                .or_else(|| fc("sans-serif:lang=zh-cn").map(|(_, file, i)| (file, i)))
                .and_then(|(file, i)| metrics(&file, i));
            log::info!("metrics: primary {family:?} {prim:?}, cjk {cjk:?}");
            Some(Fonts { prim, cjk, family })
        })
        .as_ref()
    }

    fn is_cjk(c: char) -> bool {
        matches!(c as u32,
            0x2E80..=0x2FFF | 0x3000..=0x303F | 0x3040..=0x30FF | 0x3100..=0x31FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
    }

    /// GTK 界面字体（gtk-font-name，例如 "Noto Sans,  10"）：族名 + 字号（pt → px，按 gtk-xft-dpi）
    pub fn gtk_font() -> (String, f32) {
        static G: OnceLock<(String, f32)> = OnceLock::new();
        G.get_or_init(|| {
            let ini = std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
                .and_then(|b| std::fs::read_to_string(b.join("gtk-3.0/settings.ini")).ok())
                .unwrap_or_default();
            let key = |k: &str| ini.lines().find_map(|l| l.trim().strip_prefix(k).map(|v| v.trim_start_matches([' ', '=']).trim().to_string()));
            let name = key("gtk-font-name")
                .or_else(|| {
                    let o = std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.interface", "font-name"]).output().ok()?;
                    Some(String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string()).filter(|s| !s.is_empty())
                })
                .unwrap_or_else(|| "Sans 10".into());
            let pt: f32 = name.rsplit([' ', ',']).find(|t| !t.is_empty()).and_then(|t| t.parse().ok()).unwrap_or(10.0);
            let fam = name.trim().trim_end_matches(|c: char| c.is_ascii_digit() || c == '.').trim().trim_end_matches(',').trim().to_string();
            let dpi = key("gtk-xft-dpi").and_then(|v| v.parse::<f32>().ok()).map(|d| d / 1024.0).filter(|d| *d > 0.0).unwrap_or(96.0);
            let fam = if fam.is_empty() { "Sans".to_string() } else { fam };
            (fam, pt * dpi / 72.0)
        })
        .clone()
    }

    /// pango 的行高：字体 ascent / descent 向上取整（hint metrics），行内有中文时取回退字体的较大值
    pub fn gtk_line(size: f32, text: &str) -> f32 {
        static M: OnceLock<Option<(Fm, Option<Fm>)>> = OnceLock::new();
        let m = M.get_or_init(|| {
            let (fam, _) = gtk_font();
            let prim = fc(&fam).and_then(|(_, file, i)| metrics(&file, i))?;
            let cjk = fc(&format!("{fam}:charset=4e2d")).and_then(|(_, file, i)| metrics(&file, i));
            Some((prim, cjk))
        });
        let Some((p, c)) = m else { return (size * 1.5).round() };
        let mut asc = (p.asc * size).ceil();
        let mut desc = (p.desc * size).ceil();
        if let Some(c) = c.filter(|_| text.chars().any(is_cjk)) {
            asc = asc.max((c.asc * size).ceil());
            desc = desc.max((c.desc * size).ceil());
        }
        asc + desc
    }

    pub fn family() -> Option<String> {
        fonts().and_then(|f| f.family.clone())
    }

    pub fn line_box(size: f32, text: &str, win: f32) -> f32 {
        let Some(f) = fonts() else { return win };
        let mut asc = (f.prim.asc * size).round();
        let mut desc = (f.prim.desc * size).round();
        if let Some(c) = f.cjk.filter(|_| text.chars().any(is_cjk)) {
            asc = asc.max((c.asc * size).round());
            desc = desc.max((c.desc * size).round());
        }
        asc + desc
    }
}

/// `size` 号字、内容 `text` 的一行在旧版里的行框高度（逻辑 px）；Windows 返回 `win`（.slint 里量好的值）。
pub fn line_box(size: f32, text: &str, win: f32) -> f32 {
    #[cfg(target_os = "linux")]
    {
        imp::line_box(size, text, win)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (size, text);
        win
    }
}

/// 数字 line-height（`line-height: 1.38`）的行高（逻辑 px）。
/// Chromium 用浮点（13 × 1.38 = 17.94）；WebKitGTK 的 `RenderStyle::computedLineHeight()` 乘系数后 `toInt()` 截断
/// （13 × 1.38 → 17，12 × 1.55 → 18，11.5 × 1.45 → 16；实测）。
pub fn line_height(size: f32, factor: f32) -> f32 {
    #[cfg(target_os = "linux")]
    {
        (size * factor + 1e-4).floor()
    }
    #[cfg(not(target_os = "linux"))]
    {
        size * factor
    }
}

/// 界面的默认字体族：Windows 是 .slint 里写的 Segoe UI Variable Text；Linux 取旧版 WebKitGTK 实际用的族
/// （`--font` 列表里第一个本机装了的，例如装了 Windows 字体时是 Segoe UI），没有就交给 fontconfig 默认。
pub fn ui_family() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        imp::family()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// GTK 界面字体（族名, px）：Linux 的 title 提示、toast 用；其它平台不调用
#[cfg(target_os = "linux")]
pub fn gtk_font() -> (String, f32) {
    imp::gtk_font()
}

#[cfg(target_os = "linux")]
pub fn gtk_line(size: f32, text: &str) -> f32 {
    imp::gtk_line(size, text)
}
