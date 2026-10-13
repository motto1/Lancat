//! 彩色 emoji 字形 → 预乘 RGBA（表情面板静止格）。
//!
//! 旧版表情格静止时画的是系统彩色字形（font-size 25px，"Segoe UI Emoji"），悬停才换 Lottie。
//! Slint 软件渲染器画不了彩色字形，所以在 Windows 上用 Direct2D 软件光栅 + DirectWrite
//! （D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT）把字形画进 px×px 的位图，和 toast 同一套做法。
//! 其他平台返回 None，调用方退回 Lottie 首帧。

/// `ch` 画在 px×px（物理像素）的方块里，字号 `font_px`（物理像素），水平垂直居中。
#[cfg(windows)]
pub fn render(ch: &str, px: u32, font_px: f32) -> Option<Vec<u8>> {
    use std::cell::RefCell;
    use windows::core::w;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT};
    use windows::Win32::Graphics::Direct2D::{
        D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
        D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    };
    use windows::Win32::Graphics::DirectWrite::{
        DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_WORD_WRAPPING_NO_WRAP,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Gdi::{CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};

    struct G {
        _d2d: ID2D1Factory,
        dw: IDWriteFactory,
        rt: ID2D1DCRenderTarget,
    }
    thread_local! {
        static G_: RefCell<Option<G>> = const { RefCell::new(None) };
    }
    if px == 0 || px > 1024 {
        return None;
    }
    G_.with(|cell| unsafe {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let g = (|| -> windows::core::Result<G> {
                let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
                let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
                let rt = d2d.CreateDCRenderTarget(&props)?;
                Ok(G { _d2d: d2d, dw, rt })
            })()
            .map_err(|e| log::warn!("glyph: Direct2D init failed: {e}"))
            .ok()?;
            *slot = Some(g);
        }
        let g = slot.as_ref()?;
        let fmt = g
            .dw
            .CreateTextFormat(w!("Segoe UI Emoji"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, font_px, w!("en-us"))
            .ok()?;
        let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        let wide: Vec<u16> = ch.encode_utf16().collect();
        let big = font_px * 4.0;
        let layout = g.dw.CreateTextLayout(&wide, &fmt, big, big).ok()?;
        // 按墨迹（overhang 反推的黑框）居中：CSS 的 line-height:1 + place-items:center 对 emoji 来说就是墨迹居中
        let oh = layout.GetOverhangMetrics().ok()?;
        // 墨迹框（相对布局原点）：left = -oh.left，right = big + oh.right，top = -oh.top，bottom = big + oh.bottom
        let (il, ir, it, ib) = (-oh.left, big + oh.right, -oh.top, big + oh.bottom);
        let (iw, ih) = (ir - il, ib - it);
        let s = px as f32;
        let ox = (s - iw) / 2.0 - il;
        let oy = (s - ih) / 2.0 - it;

        let (w, h) = (px as i32, px as i32);
        let mem = CreateCompatibleDC(None);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut out = None;
        if let Ok(bmp) = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            let old = SelectObject(mem, bmp.into());
            let rc = RECT { left: 0, top: 0, right: w, bottom: h };
            if g.rt.BindDC(mem, &rc).is_ok() {
                g.rt.BeginDraw();
                g.rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                g.rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
                if let Ok(b) = g.rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }, None) {
                    g.rt.DrawTextLayout(windows_numerics::Vector2 { X: ox, Y: oy }, &layout, &b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                }
                if g.rt.EndDraw(None, None).is_ok() && !bits.is_null() {
                    let n = (w * h * 4) as usize;
                    let mut v = vec![0u8; n];
                    std::ptr::copy_nonoverlapping(bits as *const u8, v.as_mut_ptr(), n);
                    // BGRA → RGBA（都已预乘）
                    for p in v.chunks_exact_mut(4) {
                        p.swap(0, 2);
                    }
                    if v.chunks_exact(4).any(|p| p[3] != 0) {
                        out = Some(v);
                    }
                }
            }
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteDC(mem);
        out
    })
}

#[cfg(not(windows))]
pub fn render(_ch: &str, _px: u32, _font_px: f32) -> Option<Vec<u8>> {
    None
}
