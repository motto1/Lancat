//! macOS dock strip: a borderless, non-activating NSPanel whose content view draws the strip and
//! acts as hover (NSTrackingArea) and drag (NSDraggingDestination) target. No WKWebView.
//!
//! TODO-verify: written against objc2 0.6 / objc2-app-kit 0.3, not compiled or run on macOS yet.
//! Things to check on a Mac: coordinate flip on multi-monitor setups with mixed scale factors,
//! that the panel stays above full-screen apps (collection behaviour), and that file drags from
//! Finder reach `draggingEntered:` on a 6 px wide view.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSDragOperation, NSDraggingInfo, NSEvent, NSPanel, NSPasteboardTypeFileURL, NSScreen, NSStatusWindowLevel, NSTrackingArea,
    NSTrackingAreaOptions, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize};
use tauri::AppHandle;

use super::{DockVisual, Rect};

static APP: OnceLock<AppHandle> = OnceLock::new();
static PULSING: AtomicBool = AtomicBool::new(false);

struct State {
    panel: Retained<NSPanel>,
    view: Retained<DockView>,
    visual: DockVisual,
    hovering: bool,
    pulse_t0: Instant,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.borrow_mut().as_mut().map(f))
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LancatDockView"]
    struct DockView;

    unsafe impl NSObjectProtocol for DockView {}

    impl DockView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            let (vis, hovering, t) = STATE
                .with(|s| s.borrow().as_ref().map(|s| (s.visual.clone(), s.hovering, s.pulse_t0.elapsed().as_secs_f64())))
                .unwrap_or_default();
            let (r, g, b, mut a) = if vis.unread {
                let (r, g, b) = vis.accent;
                (r, g, b, 0.775 + 0.225 * (t * std::f64::consts::TAU / 1.8).cos())
            } else {
                (128.0 / 255.0, 128.0 / 255.0, 140.0 / 255.0, 0.42)
            };
            if !vis.unread && !(hovering || vis.open) {
                a *= 0.75;
            }
            let rad = (bounds.size.width.min(bounds.size.height) / 2.0).min(3.0);
            let color = NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a);
            color.setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bounds, rad, rad).fill();
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _e: &NSEvent) {
            with(|s| s.hovering = true);
            self.setNeedsDisplay(true);
            if let Some(a) = APP.get() {
                crate::dock::on_hover_start(a);
                crate::dock::schedule_hover_open(a);
            }
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _e: &NSEvent) {
            with(|s| s.hovering = false);
            self.setNeedsDisplay(true);
            if let Some(a) = APP.get() {
                crate::dock::on_hover_end(a);
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _e: &NSEvent) {
            if let Some(a) = APP.get() {
                crate::dock::open_panel(a, "hover");
            }
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            if let Some(a) = APP.get() {
                crate::dock::open_panel(a, "drag");
            }
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            false // drops go to the panel, not the strip
        }
    }
);

pub fn create(app: &AppHandle) {
    let _ = APP.set(app.clone());
    let Some(mtm) = MainThreadMarker::new() else { return };
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(6.0, 160.0));
    let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    let panel = unsafe { NSPanel::initWithContentRect_styleMask_backing_defer(NSPanel::alloc(mtm), frame, style, NSBackingStoreType::Buffered, false) };
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(false);
    panel.setLevel(NSStatusWindowLevel);
    panel.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::Stationary | NSWindowCollectionBehavior::FullScreenAuxiliary);
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    unsafe { panel.setReleasedWhenClosed(false) };

    let view: Retained<DockView> = {
        let this = DockView::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: frame] }
    };
    let opts = NSTrackingAreaOptions::MouseEnteredAndExited | NSTrackingAreaOptions::ActiveAlways | NSTrackingAreaOptions::InVisibleRect;
    let area = unsafe { NSTrackingArea::initWithRect_options_owner_userInfo(NSTrackingArea::alloc(), NSRect::ZERO, opts, Some(&view), None) };
    view.addTrackingArea(&area);
    let types = NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]);
    view.registerForDraggedTypes(&types);
    panel.setContentView(Some(&view));

    STATE.with(|s| *s.borrow_mut() = Some(State { panel, view, visual: DockVisual::default(), hovering: false, pulse_t0: Instant::now() }));
}

/// `r` is physical, top-left origin (tauri); Cocoa wants points with a bottom-left origin.
pub fn place(r: Rect, _origin: (f64, f64), scale: f64) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let main_h = NSScreen::screens(mtm).firstObject().map(|s| s.frame().size.height).unwrap_or(0.0);
    with(|s| {
        let (x, y, w, h) = (r.x / scale, r.y / scale, r.w / scale, r.h / scale);
        let frame = NSRect::new(NSPoint::new(x, main_h - y - h), NSSize::new(w, h));
        s.panel.setFrame_display(frame, true);
    });
}

pub fn show() {
    with(|s| s.panel.orderFrontRegardless());
}

pub fn set_visual(v: DockVisual) {
    let start_pulse = with(|s| {
        if s.visual == v {
            return false;
        }
        let start = v.unread && !PULSING.load(Ordering::SeqCst);
        if start {
            s.pulse_t0 = Instant::now();
        }
        PULSING.store(v.unread, Ordering::SeqCst);
        s.visual = v;
        s.view.setNeedsDisplay(true);
        start
    })
    .unwrap_or(false);
    if start_pulse {
        if let Some(app) = APP.get().cloned() {
            // ~12 fps redraw while something is unread; the thread exits when PULSING drops
            std::thread::spawn(move || {
                while PULSING.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(80));
                    let _ = app.run_on_main_thread(|| {
                        with(|s| s.view.setNeedsDisplay(true));
                    });
                }
            });
        }
    }
}
