//! One project's tiles in a window of their own: a borderless panel that
//! never takes the keyboard from the app the user types in, as the
//! Windows cluster never activates.

use std::cell::{Cell, RefCell};
use std::time::{Instant, SystemTime};

use objc2::rc::Retained;
use objc2::{
    define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSEvent, NSPanel, NSResponder, NSScreen, NSTrackingArea,
    NSTrackingAreaOptions, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::anim::{TileIn, Tiles};
use crate::layout::{self, Hit, Metrics};
use crate::theme;

use super::app::{self, Input};
use super::look::{self, Scene};
use super::paint::Painter;
use super::queue;

pub struct Ivars {
    key: RefCell<String>,
    scene: RefCell<Option<Scene>>,
    tiles: RefCell<Tiles>,
    /// A redraw is booked for the light that moves.
    booked: Cell<bool>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements, and this view does
    // not implement Drop.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct ClusterView;

    impl ClusterView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.paint();
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            for a in self.trackingAreas().iter() {
                self.removeTrackingArea(&a);
            }
            let options = NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect;
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(),
                    self.bounds(),
                    options,
                    Some(self),
                    None,
                )
            };
            self.addTrackingArea(&area);
            unsafe { msg_send![super(self), updateTrackingAreas] }
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let hit = self.hit_of(event);
            self.set_hot(hit);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.set_hot(Hit::Nothing);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let hit = self.hit_of(event);
            if let Some(s) = self.ivars().scene.borrow_mut().as_mut() {
                s.pressed = Some(hit);
                s.hot = hit;
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let hit = self.hit_of(event);
            let pressed = self
                .ivars()
                .scene
                .borrow_mut()
                .as_mut()
                .and_then(|s| s.pressed.take());
            self.setNeedsDisplay(true);
            if pressed == Some(hit) && hit != Hit::Nothing {
                let key = self.ivars().key.borrow().clone();
                app::input(Input::Cluster(key, hit, self.screen_point(event)));
            }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let hit = self.hit_of(event);
            let key = self.ivars().key.borrow().clone();
            app::input(Input::ClusterMenu(key, hit, self.screen_point(event)));
        }
    }
);

impl ClusterView {
    fn new(mtm: MainThreadMarker, key: &str, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            key: RefCell::new(key.to_string()),
            scene: RefCell::new(None),
            tiles: RefCell::new(Tiles::default()),
            booked: Cell::new(false),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn hit_of(&self, event: &NSEvent) -> Hit {
        let at = self.convertPoint_fromView(event.locationInWindow(), None);
        self.ivars()
            .scene
            .borrow()
            .as_ref()
            .map_or(Hit::Nothing, |s| {
                layout::hit(&s.layout, at.x as f32, at.y as f32)
            })
    }

    /// Where the event happened, in screen points with the origin at the
    /// bottom left, as menus are placed.
    fn screen_point(&self, event: &NSEvent) -> NSPoint {
        let in_window = event.locationInWindow();
        match self.window() {
            Some(w) => w.convertPointToScreen(in_window),
            None => in_window,
        }
    }

    fn set_hot(&self, hit: Hit) {
        let changed = self.ivars().scene.borrow_mut().as_mut().is_some_and(|s| {
            let hit = if hit.lights() { hit } else { Hit::Nothing };
            std::mem::replace(&mut s.hot, hit) != hit
        });
        if changed {
            self.setNeedsDisplay(true);
        }
    }

    fn paint(&self) {
        let Some(p) = Painter::current() else {
            return;
        };
        let mut scene = self.ivars().scene.borrow_mut();
        let Some(s) = scene.as_mut() else {
            return;
        };
        let now = Instant::now();
        s.now = SystemTime::now();
        let ins: Vec<TileIn> = s
            .layout
            .tiles
            .iter()
            .zip(&s.sessions)
            .enumerate()
            .map(|(i, (r, session))| TileIn {
                id: &session.id,
                phase: &session.phase,
                y: r.y,
                hot: s.hot == Hit::Tile(i),
                held: false,
                icon: theme::tile_icon(session),
                context: session
                    .status
                    .as_ref()
                    .and_then(|st| st.context)
                    .map(|c| c / 100.0),
            })
            .collect();
        let looks = self.ivars().tiles.borrow_mut().step(now, &ins);
        let phases: Vec<_> = s.sessions.iter().map(|x| &x.phase).collect();
        let targets: Vec<f32> = s.layout.tiles.iter().map(|r| r.y).collect();
        let next = Tiles::next_frame(&looks, &phases, &targets, s.ambient);
        s.looks = looks;
        look::draw(&p, &Metrics::default(), s);
        drop(scene);
        if let Some(wait) = next {
            self.book(wait);
        }
    }

    /// A redraw `wait` from now, once.
    fn book(&self, wait: std::time::Duration) {
        if self.ivars().booked.replace(true) {
            return;
        }
        let me = self.retain();
        queue::after_main(wait, move || {
            me.ivars().booked.set(false);
            me.setNeedsDisplay(true);
        });
    }
}

/// A project's window and its view.
pub struct Cluster {
    pub key: String,
    panel: Retained<NSPanel>,
    view: Retained<ClusterView>,
    pub collapsed: bool,
    /// The height the layout wants, in points.
    pub height: f32,
}

impl Cluster {
    pub fn new(mtm: MainThreadMarker, key: &str) -> Cluster {
        let m = Metrics::default();
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(m.width as f64, 100.0));
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(true);
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setFloatingPanel(false);
        panel.setAcceptsMouseMovedEvents(true);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        let view = ClusterView::new(mtm, key, frame);
        panel.setContentView(Some(&view));
        Cluster {
            key: key.to_string(),
            panel,
            view,
            collapsed: false,
            height: 100.0,
        }
    }

    /// Shows `scene`, the window sized to its layout.
    pub fn set_scene(&mut self, scene: Scene) {
        self.height = scene.layout.size.1;
        *self.view.ivars().scene.borrow_mut() = Some(scene);
        self.view.setNeedsDisplay(true);
    }

    /// The scene shown now, with the hover kept.
    pub fn hover(&self) -> (Hit, Option<Hit>) {
        self.view
            .ivars()
            .scene
            .borrow()
            .as_ref()
            .map_or((Hit::Nothing, None), |s| (s.hot, s.pressed))
    }

    /// Puts the window's top left corner at `(x, y)` in screen points
    /// measured from the top of the main screen.
    pub fn place(&self, x: f32, top: f32) {
        let (w, h) = (Metrics::default().width as f64, self.height as f64);
        let screen_h = main_screen_height(self.panel.mtm());
        let frame = NSRect::new(
            NSPoint::new(x as f64, screen_h - top as f64 - h),
            NSSize::new(w, h),
        );
        self.panel.setFrame_display(frame, true);
        if !self.panel.isVisible() {
            self.panel.orderFront(None);
        }
    }

    pub fn raise(&self) {
        self.panel.orderFront(None);
    }

    pub fn view(&self) -> &NSView {
        &self.view
    }

    pub fn redraw(&self) {
        self.view.setNeedsDisplay(true);
    }

    pub fn close(&self) {
        self.panel.orderOut(None);
        self.panel.close();
    }
}

/// The main screen's full height, which turns the top-down coordinates the
/// layout uses into AppKit's bottom-up ones.
pub fn main_screen_height(mtm: MainThreadMarker) -> f64 {
    NSScreen::screens(mtm)
        .firstObject()
        .map_or(900.0, |s| s.frame().size.height)
}
