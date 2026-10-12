//! Message toast on Linux, the same design as Windows (native_dock/win_toast.rs): when a peer
//! sends something while the panel is closed, the dock strip inflates into a two-line pill
//! (avatar, name over message) for a few seconds. Hovering holds it, a click opens the panel on
//! that conversation, otherwise it shrinks back into the strip.
//!
//! A GTK3 window drawn with cairo + pango (no web renderer is woken): on Wayland a wlr-layer-shell
//! overlay surface placed over the strip by margins (niri, Sway, Hyprland, KDE), on X11 an
//! undecorated keep-above notification window. Wayland gives clients no way to read the screen,
//! so unlike Windows there is no captured, blurred backdrop: the glass is a tinted translucent
//! body with a sheen and a specular rim (compositors that blur layer surfaces blur behind it).
//! The input region is the card only, so the transparent margin never eats clicks.
//! Main thread only.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{cairo, gdk, glib, pango};
use tauri::AppHandle;

use super::Rect;

const ANIM_MS: f64 = 280.0;
/// Logical sizes, same as Windows.
const H: f64 = 48.0;
const PAD: f64 = 8.0;
const AVATAR: f64 = 32.0;
const GAP: f64 = 9.0;
const NAME_MAX: f64 = 196.0;
const TEXT_MAX: f64 = 196.0;
const RADIUS: f64 = 17.0;
const LINE_GAP: f64 = 1.0;
const SHADOW: f64 = 10.0;
/// distance of the pill from the screen edge
const INSET: f64 = 4.0;

pub struct ToastSpec {
    pub peer_id: String,
    pub name: String,
    pub text: String,
    pub edge: String,
    /// the strip's rect (physical px) and its monitor (origin physical px, scale)
    pub strip: Rect,
    pub origin: (f64, f64),
    pub scale: f64,
    pub dark: bool,
    pub hold_ms: u32,
}

struct State {
    app: AppHandle,
    win: gtk::Window,
    layer: bool,
    spec: Option<ToastSpec>,
    /// the strip in window-local logical px
    strip: (f64, f64),
    /// window size, logical
    size: (f64, f64),
    name_w: f64,
    text_w: f64,
    t: f64,
    from: f64,
    to: f64,
    t0: Instant,
    anim: Option<glib::SourceId>,
    hide: Option<glib::SourceId>,
    hovering: bool,
}

thread_local! {
    static S: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    S.with(|s| s.borrow_mut().as_mut().map(f))
}

pub fn shown() -> bool {
    with(|s| s.spec.is_some() && s.to > 0.5).unwrap_or(false)
}

fn ensure_window(app: &AppHandle) -> bool {
    if with(|_| ()).is_some() {
        return true;
    }
    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title("Lancat Toast");
    win.set_decorated(false);
    win.set_resizable(false);
    win.set_skip_taskbar_hint(true);
    win.set_skip_pager_hint(true);
    win.set_accept_focus(false);
    win.set_focus_on_map(false);
    win.set_app_paintable(true);
    if let Some(v) = WidgetExt::screen(&win).and_then(|s| s.rgba_visual()) {
        win.set_visual(Some(&v));
    }
    win.add_events(gdk::EventMask::ENTER_NOTIFY_MASK | gdk::EventMask::LEAVE_NOTIFY_MASK | gdk::EventMask::BUTTON_PRESS_MASK | gdk::EventMask::BUTTON_RELEASE_MASK);
    let layer = init_layer(&win);
    if !layer {
        win.set_type_hint(gdk::WindowTypeHint::Notification);
        win.set_keep_above(true);
        win.stick();
    }
    win.connect_draw(|_, cr| {
        with(|s| render(s, cr));
        glib::Propagation::Stop
    });
    win.connect_enter_notify_event(|_, _| {
        with(|s| {
            s.hovering = true;
            if let Some(id) = s.hide.take() {
                id.remove();
            }
        });
        glib::Propagation::Proceed
    });
    win.connect_leave_notify_event(|_, e| {
        // pointer moving onto a child surface is not leaving
        if e.detail() == gdk::NotifyType::Inferior {
            return glib::Propagation::Proceed;
        }
        with(|s| {
            s.hovering = false;
            if s.to > 0.5 {
                arm_hide(s, 1500);
            }
        });
        glib::Propagation::Proceed
    });
    win.connect_button_release_event(|_, e| {
        if e.button() == 1 {
            let target = with(|s| (s.app.clone(), s.spec.as_ref().map(|p| p.peer_id.clone())));
            if let Some((app, Some(peer))) = target {
                with(|s| animate(s, 0.0));
                crate::dock::open_panel(&app, &format!("toast:{peer}"));
            }
        }
        glib::Propagation::Stop
    });
    S.with(|s| {
        *s.borrow_mut() = Some(State {
            app: app.clone(),
            win,
            layer,
            spec: None,
            strip: (0.0, 0.0),
            size: (0.0, 0.0),
            name_w: 0.0,
            text_w: 0.0,
            t: 0.0,
            from: 0.0,
            to: 0.0,
            t0: Instant::now(),
            anim: None,
            hide: None,
            hovering: false,
        })
    });
    true
}

#[cfg(feature = "layer-shell")]
fn init_layer(win: &gtk::Window) -> bool {
    use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
    if !crate::platform::use_layer_shell() || !gtk_layer_shell::is_supported() {
        return false;
    }
    win.init_layer_shell();
    win.set_namespace("lancat-toast");
    win.set_layer(Layer::Overlay);
    win.set_keyboard_mode(KeyboardMode::None);
    win.set_exclusive_zone(-1);
    win.set_anchor(Edge::Left, true);
    win.set_anchor(Edge::Top, true);
    true
}

#[cfg(not(feature = "layer-shell"))]
fn init_layer(_win: &gtk::Window) -> bool {
    false
}

fn font(size: f64, bold: bool) -> pango::FontDescription {
    let mut f = gtk::Settings::default()
        .and_then(|s| s.gtk_font_name())
        .map(|n| pango::FontDescription::from_string(&n))
        .unwrap_or_else(|| pango::FontDescription::from_string("Sans"));
    f.set_absolute_size(size * pango::SCALE as f64);
    f.set_weight(if bold { pango::Weight::Semibold } else { pango::Weight::Normal });
    f
}

fn layout(cr: &cairo::Context, s: &str, size: f64, bold: bool, max_w: f64) -> pango::Layout {
    let l = pangocairo::functions::create_layout(cr);
    l.set_font_description(Some(&font(size, bold)));
    l.set_text(s);
    l.set_width((max_w * pango::SCALE as f64) as i32);
    l.set_ellipsize(pango::EllipsizeMode::End);
    l.set_single_paragraph_mode(true);
    l
}

fn text_width(s: &str, size: f64, bold: bool, max_w: f64) -> f64 {
    let Ok(surf) = cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1) else { return max_w };
    let Ok(cr) = cairo::Context::new(&surf) else { return max_w };
    let l = layout(&cr, s, size, bold, max_w);
    let (_, logical) = l.pixel_extents();
    (logical.width() as f64).min(max_w)
}

pub fn show(app: &AppHandle, spec: ToastSpec) {
    if !ensure_window(app) {
        return;
    }
    let hold = spec.hold_ms;
    with(|s| {
        s.name_w = text_width(&spec.name, 12.5, true, NAME_MAX);
        s.text_w = text_width(&spec.text, 12.0, false, TEXT_MAX);
        let (pw, ph) = pill_size(s);
        // logical, output-relative strip
        let sc = spec.scale.max(0.1);
        let st = Rect { x: (spec.strip.x - spec.origin.0) / sc, y: (spec.strip.y - spec.origin.1) / sc, w: spec.strip.w / sc, h: spec.strip.h / sc };
        let m = SHADOW;
        let (cx, cy) = (st.x + st.w / 2.0, st.y + st.h / 2.0);
        let bw = pw.max(st.w) + INSET + m;
        let bh = ph.max(st.h) + 2.0 * m;
        let (bw2, bh2) = (pw.max(st.w) + 2.0 * m, ph.max(st.h) + INSET + m);
        let (x, y, w, h) = match spec.edge.as_str() {
            "left" => (st.x, cy - bh / 2.0, bw, bh),
            "top" => (cx - bw2 / 2.0, st.y, bw2, bh2),
            "bottom" => (cx - bw2 / 2.0, st.y + st.h - bh2, bw2, bh2),
            _ => (st.x + st.w - bw, cy - bh / 2.0, bw, bh),
        };
        let (x, y) = (x.max(0.0), y.max(0.0));
        s.size = (w.round(), h.round());
        s.strip = (st.w, st.h);
        let replacing = s.spec.is_some() && s.to > 0.5;
        s.spec = Some(spec);
        s.win.set_size_request(s.size.0 as i32, s.size.1 as i32);
        s.win.resize(s.size.0 as i32, s.size.1 as i32);
        place(s, x, y);
        if !replacing {
            s.t = 0.0;
        }
        animate(s, 1.0);
        s.win.show_all();
        set_input_region(s);
        s.win.queue_draw();
        if !s.hovering {
            arm_hide(s, hold);
        }
    });
}

#[allow(unused_variables)]
fn place(s: &State, x: f64, y: f64) {
    #[cfg(feature = "layer-shell")]
    if s.layer {
        use gtk_layer_shell::{Edge, LayerShell};
        s.win.set_layer_shell_margin(Edge::Left, x.round() as i32);
        s.win.set_layer_shell_margin(Edge::Top, y.round() as i32);
        return;
    }
    // X11: absolute screen position (logical); the monitor origin is added back
    let (ox, oy, sc) = s.spec.as_ref().map(|p| (p.origin.0, p.origin.1, p.scale.max(0.1))).unwrap_or((0.0, 0.0, 1.0));
    s.win.move_((x + ox / sc).round() as i32, (y + oy / sc).round() as i32);
}

/// Only the card takes the pointer; the shadow margin stays click-through.
fn set_input_region(s: &State) {
    let Some(gw) = s.win.window() else { return };
    let (x, y, w, h) = card_rect(s);
    let r = cairo::Region::create_rectangle(&cairo::RectangleInt::new(x.floor() as i32, y.floor() as i32, w.ceil() as i32, h.ceil() as i32));
    gw.input_shape_combine_region(&r, 0, 0);
}

fn arm_hide(s: &mut State, ms: u32) {
    if let Some(id) = s.hide.take() {
        id.remove();
    }
    s.hide = Some(glib::timeout_add_local_once(Duration::from_millis(ms as u64), || {
        with(|s| {
            s.hide = None;
            animate(s, 0.0);
        });
    }));
}

/// Hides the toast (the panel opened, the dock moved …). Main thread only.
pub fn dismiss() {
    with(|s| {
        if s.spec.is_some() && s.to > 0.0 {
            animate(s, 0.0);
        }
    });
}

fn pill_size(s: &State) -> (f64, f64) {
    let w = PAD + (AVATAR + GAP) + s.name_w.max(s.text_w) + PAD * 1.7;
    (w.max(H * 2.4), H)
}

/// Final card rect inside the window (window-local), as `render` places it at t = 1.
fn card_rect(s: &State) -> (f64, f64, f64, f64) {
    let (pw, ph) = pill_size(s);
    let (fw, fh) = s.size;
    match s.spec.as_ref().map(|p| p.edge.as_str()).unwrap_or("right") {
        "left" => (INSET, fh / 2.0 - ph / 2.0, pw, ph),
        "top" => (fw / 2.0 - pw / 2.0, INSET, pw, ph),
        "bottom" => (fw / 2.0 - pw / 2.0, fh - ph - INSET, pw, ph),
        _ => (fw - pw - INSET, fh / 2.0 - ph / 2.0, pw, ph),
    }
}

fn animate(s: &mut State, to: f64) {
    s.from = s.t;
    s.to = to;
    s.t0 = Instant::now();
    if to <= 0.0 {
        if let Some(id) = s.hide.take() {
            id.remove();
        }
    }
    if s.anim.is_none() {
        s.anim = Some(glib::timeout_add_local(Duration::from_millis(16), || {
            let go = with(|s| {
                let done = step(s);
                s.win.queue_draw();
                if done {
                    s.anim = None;
                    if s.to <= 0.0 {
                        s.win.hide();
                        s.spec = None;
                        s.hovering = false;
                    }
                }
                !done
            })
            .unwrap_or(false);
            if go { glib::ControlFlow::Continue } else { glib::ControlFlow::Break }
        }));
    }
}

fn ease(t: f64) -> f64 {
    // easeOutBack-ish: a little overshoot reads as "inflating" out of the strip
    let c1 = 1.2;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

fn step(s: &mut State) -> bool {
    let k = (s.t0.elapsed().as_secs_f64() * 1000.0 / ANIM_MS).min(1.0);
    let e = if s.to > s.from { ease(k) } else { k * k };
    s.t = s.from + (s.to - s.from) * e;
    k >= 1.0
}

/// Same as the page's `initials` (api.ts).
fn initials(name: &str) -> String {
    let n = name.trim();
    if n.is_empty() {
        return "?".into();
    }
    if let Some(c) = n.chars().find(|c| ('\u{3400}'..='\u{9fff}').contains(c)) {
        return c.to_string();
    }
    let parts: Vec<&str> = n.split(|c: char| c.is_whitespace() || "-_.".contains(c)).filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2 {
        return parts[..2].iter().filter_map(|p| p.chars().next()).collect::<String>().to_uppercase();
    }
    n.chars().take(2).collect::<String>().to_uppercase()
}

/// Same colour as the page's `avatarColor` (api.ts): hsl(hue 70% 55%).
fn avatar_color(id: &str) -> (f64, f64, f64) {
    const HUES: [f64; 9] = [211.0, 262.0, 330.0, 14.0, 32.0, 145.0, 175.0, 195.0, 290.0];
    let h = id.encode_utf16().fold(0u32, |a, c| a.wrapping_mul(31).wrapping_add(c as u32));
    let hue = HUES[(h % 9) as usize];
    let (s, l) = (0.70, 0.55);
    let c = (1.0 - (2.0 * l - 1.0f64).abs()) * s;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0f64).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}

fn render(s: &State, cr: &cairo::Context) {
    cr.set_operator(cairo::Operator::Source);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
    let _ = cr.paint();
    cr.set_operator(cairo::Operator::Over);
    let Some(spec) = &s.spec else { return };
    let (pw, ph) = pill_size(s);
    let (fw, fh) = s.size;
    let t = s.t.max(0.0);
    let vertical = spec.edge == "left" || spec.edge == "right";
    let (sw, sh) = s.strip;
    let dark = spec.dark;
    let lerp = |a: f64, b: f64| a + (b - a) * t;
    let (cw, ch) = (lerp(sw, pw), lerp(sh, ph));
    let gap = lerp(0.0, INSET);
    let (x0, y0) = if vertical {
        let x = if spec.edge == "right" { fw - cw - gap } else { gap };
        (x, fh / 2.0 - ch / 2.0)
    } else {
        let y = if spec.edge == "bottom" { fh - ch - gap } else { gap };
        (fw / 2.0 - cw / 2.0, y)
    };
    let radius = (1.0 - t) * (cw.min(ch) / 2.0) + t * RADIUS.min(ch / 2.0);
    let content = ((t - 0.45) / 0.55).clamp(0.0, 1.0);
    // soft shadow: a few widening, fading rounded rects
    for i in 1..=4 {
        let d = i as f64 * 2.0 * t;
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.05 * content.max(0.3 * t) * (5 - i) as f64 / 4.0);
        rounded(cr, x0 - d, y0 - d + 1.0, cw + 2.0 * d, ch + 2.0 * d, radius + d);
        let _ = cr.fill();
    }
    // body: accent while it is still the strip, a translucent glass tint once inflated
    let body = if dark { (0.12, 0.12, 0.14, 0.86) } else { (0.97, 0.97, 0.98, 0.88) };
    let accent = (0.04, 0.52, 1.0);
    let k = (t * 2.0).min(1.0);
    let mix = |a: f64, b: f64| a + (b - a) * k;
    rounded(cr, x0, y0, cw, ch, radius);
    cr.set_source_rgba(mix(accent.0, body.0), mix(accent.1, body.1), mix(accent.2, body.2), mix(0.8, body.3));
    let _ = cr.fill_preserve();
    // top sheen
    let g = cairo::LinearGradient::new(x0, y0, x0, y0 + ch);
    g.add_color_stop_rgba(0.0, 1.0, 1.0, 1.0, (if dark { 0.16 } else { 0.42 }) * content);
    g.add_color_stop_rgba(0.55, 1.0, 1.0, 1.0, 0.0);
    let _ = cr.set_source(&g);
    let _ = cr.fill_preserve();
    // specular rim
    cr.set_source_rgba(1.0, 1.0, 1.0, (if dark { 0.24 } else { 0.7 }) * content);
    cr.set_line_width(0.8);
    let _ = cr.stroke();
    if dark {
        // a dark card needs an outer hairline to separate from dark wallpapers
        rounded(cr, x0 - 0.5, y0 - 0.5, cw + 1.0, ch + 1.0, radius + 0.5);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.35 * content);
        cr.set_line_width(0.6);
        let _ = cr.stroke();
    }
    if content <= 0.0 {
        return;
    }
    let cx = x0 + PAD + AVATAR / 2.0;
    let cy = y0 + ch / 2.0;
    let (r, g, b) = avatar_color(&spec.peer_id);
    cr.set_source_rgba(r, g, b, content);
    cr.arc(cx, cy, AVATAR / 2.0, 0.0, std::f64::consts::TAU);
    let _ = cr.fill();
    let il = layout(cr, &initials(&spec.name), 14.0, true, AVATAR);
    let (_, ie) = il.pixel_extents();
    cr.set_source_rgba(1.0, 1.0, 1.0, content);
    cr.move_to(cx - ie.width() as f64 / 2.0 - ie.x() as f64, cy - ie.height() as f64 / 2.0 - ie.y() as f64);
    pangocairo::functions::show_layout(cr, &il);
    let nl = layout(cr, &spec.name, 12.5, true, NAME_MAX);
    let tl = layout(cr, &spec.text, 12.0, false, TEXT_MAX);
    let (_, ne) = nl.pixel_extents();
    let (_, te) = tl.pixel_extents();
    let (nh, th) = (ne.height() as f64, te.height() as f64);
    let x = x0 + PAD + AVATAR + GAP;
    let top = cy - (nh + th + LINE_GAP) / 2.0;
    // clip the text to the card while it inflates
    cr.save().ok();
    rounded(cr, x0, y0, cw, ch, radius);
    cr.clip();
    if dark {
        cr.set_source_rgba(1.0, 1.0, 1.0, content);
    } else {
        cr.set_source_rgba(0.07, 0.07, 0.08, content);
    }
    cr.move_to(x, top);
    pangocairo::functions::show_layout(cr, &nl);
    if dark {
        cr.set_source_rgba(0.92, 0.92, 0.95, 0.82 * content);
    } else {
        cr.set_source_rgba(0.2, 0.2, 0.24, 0.85 * content);
    }
    cr.move_to(x, top + nh + LINE_GAP);
    pangocairo::functions::show_layout(cr, &tl);
    cr.restore().ok();
}

/// A theme setting ("dark" | "light" | "system") resolved against the desktop's colour scheme.
pub fn theme_is_dark(theme: &str) -> bool {
    match theme {
        "dark" => true,
        "light" => false,
        _ => {
            use gtk::gio;
            use gtk::gio::prelude::*;
            let schema = "org.gnome.desktop.interface";
            if gio::SettingsSchemaSource::default().and_then(|src| src.lookup(schema, true)).is_some_and(|sc| sc.has_key("color-scheme")) {
                return gio::Settings::new(schema).string("color-scheme").as_str() == "prefer-dark";
            }
            gtk::Settings::default().is_some_and(|s| s.is_gtk_application_prefer_dark_theme() || s.gtk_theme_name().is_some_and(|n| n.to_lowercase().contains("dark")))
        }
    }
}
