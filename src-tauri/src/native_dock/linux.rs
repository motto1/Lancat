//! Linux dock strip: a plain GTK3 window drawn with cairo (no WebView, no WebKit process).
//!
//! * Wayland + `layer-shell` feature: a wlr-layer-shell overlay surface (niri, Sway, Hyprland, KDE).
//! * X11 / XWayland: an undecorated always-on-top window with the `Dock` type hint.
//!
//! Hover (enter-notify + `hover_delay_ms` timer), file drags (GTK drag destination for
//! `text/uri-list`) and the unread pulse are handled here, on the GTK main thread.
//! Everything GTK lives in a thread-local because GTK objects are not `Send`.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{cairo, gdk, glib};
use tauri::AppHandle;

use super::{DockVisual, Rect};

struct Strip {
    win: gtk::Window,
    visual: DockVisual,
    hovering: bool,
    hover_timer: Option<glib::SourceId>,
    pulse: Option<(glib::SourceId, Instant)>,
    layer: bool,
}

thread_local! {
    static STRIP: RefCell<Option<Strip>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Strip) -> R) -> Option<R> {
    STRIP.with(|s| s.borrow_mut().as_mut().map(f))
}

/// Must run on the GTK main thread (tauri's `setup` does).
pub fn create(app: &AppHandle) {
    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title("Lancat Dock");
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
    win.add_events(gdk::EventMask::ENTER_NOTIFY_MASK | gdk::EventMask::LEAVE_NOTIFY_MASK | gdk::EventMask::BUTTON_PRESS_MASK);

    // memprobe runs on a desktop that may be in use: a real pointer resting on the strip must not
    // open/close the panel behind the probe's back
    let probe = std::env::var_os("LANCAT_PROBE").is_some();
    let layer = init_layer(&win);
    if !layer {
        win.set_type_hint(gdk::WindowTypeHint::Dock);
        win.set_keep_above(true);
        win.stick();
    }

    win.connect_draw(|w, cr| {
        let (vis, hovering, phase) = STRIP.with(|s| {
            let s = s.borrow();
            match s.as_ref() {
                Some(s) => (s.visual.clone(), s.hovering, s.pulse.as_ref().map(|p| p.1.elapsed().as_secs_f64())),
                None => (DockVisual::default(), false, None),
            }
        });
        draw(w, cr, &vis, hovering, phase);
        glib::Propagation::Stop
    });

    let a = app.clone();
    win.connect_enter_notify_event(move |_, _| {
        if probe {
            return glib::Propagation::Proceed;
        }
        let delay = super::hover_delay(&a);
        let a2 = a.clone();
        with(|s| {
            s.hovering = true;
            if let Some(t) = s.hover_timer.take() {
                t.remove();
            }
            s.hover_timer = Some(glib::timeout_add_local_once(Duration::from_millis(delay), move || {
                with(|s| s.hover_timer = None);
                crate::dock::open_panel(&a2, "hover");
            }));
            s.win.queue_draw();
        });
        crate::dock::on_hover_start(&a);
        glib::Propagation::Proceed
    });
    let a = app.clone();
    win.connect_leave_notify_event(move |_, ev| {
        if probe {
            return glib::Propagation::Proceed;
        }
        // GTK sends a leave with detail Inferior when the pointer moves onto a child; we have none,
        // but ignore it anyway for safety.
        if ev.detail() == gdk::NotifyType::Inferior {
            return glib::Propagation::Proceed;
        }
        with(|s| {
            s.hovering = false;
            if let Some(t) = s.hover_timer.take() {
                t.remove();
            }
            s.win.queue_draw();
        });
        crate::dock::on_hover_end(&a);
        glib::Propagation::Proceed
    });
    // a click on the strip opens at once (useful on touchpads with tap-to-click)
    let a = app.clone();
    win.connect_button_press_event(move |_, _| {
        if !probe {
            crate::dock::open_panel(&a, "hover");
        }
        glib::Propagation::Stop
    });

    // file drags: accept text/uri-list so GTK reports the motion, open immediately, never take the drop
    let targets = [gtk::TargetEntry::new("text/uri-list", gtk::TargetFlags::OTHER_APP, 0)];
    win.drag_dest_set(gtk::DestDefaults::empty(), &targets, gdk::DragAction::COPY);
    let a = app.clone();
    win.connect_drag_motion(move |_, ctx, _, _, time| {
        if probe {
            return false;
        }
        crate::dock::open_panel(&a, "drag");
        ctx.drag_status(gdk::DragAction::COPY, time);
        true
    });
    win.connect_drag_drop(|_, _, _, _, _| false);

    STRIP.with(|s| {
        *s.borrow_mut() = Some(Strip { win: win.clone(), visual: DockVisual::default(), hovering: false, hover_timer: None, pulse: None, layer })
    });
}

#[cfg(feature = "layer-shell")]
fn init_layer(win: &gtk::Window) -> bool {
    use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
    if !crate::platform::use_layer_shell() || !gtk_layer_shell::is_supported() {
        return false;
    }
    win.init_layer_shell();
    win.set_namespace("lancat-dock");
    win.set_layer(Layer::Overlay);
    win.set_anchor(Edge::Top, true);
    win.set_anchor(Edge::Left, true);
    win.set_exclusive_zone(-1);
    win.set_keyboard_mode(KeyboardMode::None);
    true
}

#[cfg(not(feature = "layer-shell"))]
fn init_layer(_win: &gtk::Window) -> bool {
    false
}

/// `r` is in physical pixels; `origin`/`scale` describe the monitor.
pub fn place(r: Rect, origin: (f64, f64), scale: f64) {
    with(|s| {
        let (lx, ly) = ((r.x - origin.0) / scale, (r.y - origin.1) / scale);
        let (w, h) = ((r.w / scale).round().max(1.0) as i32, (r.h / scale).round().max(1.0) as i32);
        s.win.set_size_request(w, h);
        s.win.resize(w, h);
        #[cfg(feature = "layer-shell")]
        if s.layer {
            use gtk_layer_shell::{Edge, LayerShell};
            s.win.set_layer_shell_margin(Edge::Left, lx.round() as i32);
            s.win.set_layer_shell_margin(Edge::Top, ly.round() as i32);
            return;
        }
        let _ = (lx, ly);
        // X11: GDK works in logical ("application") pixels of the whole screen
        s.win.move_((r.x / scale).round() as i32, (r.y / scale).round() as i32);
    });
}

pub fn show() {
    with(|s| s.win.show_all());
}

pub fn set_visual(v: DockVisual) {
    with(|s| {
        if s.visual == v {
            return;
        }
        let pulse = v.unread;
        s.visual = v;
        match (pulse, s.pulse.is_some()) {
            (true, false) => {
                // ~12 fps is plenty for a 1.8 s breathing pulse and keeps CPU near zero
                let id = glib::timeout_add_local(Duration::from_millis(80), || {
                    with(|s| s.win.queue_draw());
                    glib::ControlFlow::Continue
                });
                s.pulse = Some((id, Instant::now()));
            }
            (false, true) => {
                if let Some((id, _)) = s.pulse.take() {
                    id.remove();
                }
            }
            _ => {}
        }
        s.win.queue_draw();
    });
}

fn draw(w: &gtk::Window, cr: &cairo::Context, v: &DockVisual, hovering: bool, pulse: Option<f64>) {
    let (aw, ah) = (w.allocated_width() as f64, w.allocated_height() as f64);
    cr.set_operator(cairo::Operator::Source);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
    let _ = cr.paint();
    cr.set_operator(cairo::Operator::Over);

    let vertical = v.edge == "left" || v.edge == "right";
    // hover: slightly thicker towards the screen interior (mirrors the old CSS scale 1.15)
    let (mut x, mut y, mut rw, mut rh) = (0.0, 0.0, aw, ah);
    if !hovering && !v.open {
        if vertical {
            rw = (aw * 0.87).max(1.0);
            if v.edge == "right" {
                x = aw - rw;
            }
        } else {
            rh = (ah * 0.87).max(1.0);
            if v.edge == "bottom" {
                y = ah - rh;
            }
        }
    }
    let r = (rw.min(rh) / 2.0).min(3.0);
    rounded(cr, x, y, rw, rh, r);

    let (red, green, blue, mut alpha) = if v.unread {
        let (r, g, b) = v.accent;
        (r, g, b, 1.0)
    } else {
        (128.0 / 255.0, 128.0 / 255.0, 140.0 / 255.0, 0.42)
    };
    if v.unread {
        // 0.55 ↔ 1.0 over 1.8 s, like the old CSS keyframes
        let t = pulse.unwrap_or(0.0);
        alpha *= 0.775 + 0.225 * (t * std::f64::consts::TAU / 1.8).cos();
    } else if !(hovering || v.open) {
        alpha *= 0.75;
    }
    cr.set_source_rgba(red, green, blue, alpha);
    let _ = cr.fill_preserve();
    // hairline highlight (inset 0.5px white @35%)
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.35 * alpha);
    cr.set_line_width(0.5);
    let _ = cr.stroke();
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}
