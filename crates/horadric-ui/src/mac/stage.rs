//! The stage: one window for every session, showing one project at a time
//! with each of its sessions a pane in a grid. A pane's terminal is the
//! console's grid, built into a [`Frame`] by the same code Windows uses
//! and drawn with Core Text glyph runs pinned to the cells.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::sync::Arc;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{CursorShape, Rgb};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSBackingStoreType, NSEvent, NSEventModifierFlags, NSFont, NSFontWeightRegular, NSResponder,
    NSStringDrawing, NSTextInputClient, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{CGAffineTransform, CGFloat, CGPoint};
use objc2_core_graphics::CGContext;
use objc2_core_text::CTFont;
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSNotFound, NSPoint, NSRange,
    NSRangePointer, NSRect, NSSize, NSString, NSUInteger,
};

use crate::console::{Console, GridSize};
use crate::frame::{self, Decoration, Frame, BOLD, ITALIC};
use crate::keys::{self, FontStep, Kitty};
use crate::layout::{self, Rect};
use crate::mac_keys::{self, Action, Press};
use crate::theme::{self, Color};

use super::app::{self, Input};
use super::paint::{cg_rect, measure, ns_color, Font, Painter};

/// A pane's header, with the session's name.
const HEADER_H: f32 = 26.0;
/// Between the pane's edge and its grid.
const PAD: f32 = 6.0;
/// Between panes.
const GAP: i32 = 6;
const RADIUS: f32 = 8.0;
/// The terminal font on a Mac when nothing else was chosen, in points.
pub const FONT_DEFAULT: f32 = 13.0;

/// One session as the stage shows it.
pub struct Pane {
    pub id: String,
    pub name: String,
    pub console: Arc<Console>,
    /// The phase's colour, while the phase is worth an edge.
    pub phase: Option<Color>,
    pub accent: Color,
    /// Where the pane is in the view, set by the layout.
    rect: Rect,
}

impl Pane {
    pub fn new(id: String, name: String, console: Arc<Console>, accent: Color) -> Pane {
        Pane {
            id,
            name,
            console,
            phase: None,
            accent,
            rect: Rect::default(),
        }
    }

    fn grid_rect(&self) -> Rect {
        let r = self.rect;
        Rect::new(
            r.x + PAD,
            r.y + HEADER_H,
            (r.w - 2.0 * PAD).max(0.0),
            (r.h - HEADER_H - PAD).max(0.0),
        )
    }

    fn close_rect(&self) -> Rect {
        let r = self.rect;
        Rect::new(r.right() - HEADER_H - 2.0, r.y, HEADER_H, HEADER_H)
    }
}

/// The terminal font in its four faces, and the cell it makes.
struct TermFont {
    size: f32,
    faces: [Retained<NSFont>; 4],
    cell_w: f32,
    cell_h: f32,
    ascent: f32,
}

impl TermFont {
    fn new(size: f32) -> TermFont {
        let regular = unsafe {
            NSFont::monospacedSystemFontOfSize_weight(size as CGFloat, NSFontWeightRegular)
        };
        let bold = unsafe {
            NSFont::monospacedSystemFontOfSize_weight(
                size as CGFloat,
                objc2_app_kit::NSFontWeightBold,
            )
        };
        let italic = with_italic(&regular);
        let bold_italic = with_italic(&bold);
        let ascent = regular.ascender() as f32;
        let descent = -regular.descender() as f32;
        let leading = regular.leading() as f32;
        let m = measure_with(&regular, "M");
        // Whole points keep neighbouring backgrounds from leaving seams.
        let cell_w = (m * 2.0).round() / 2.0;
        let cell_h = (ascent + descent + leading + 2.0).ceil();
        TermFont {
            size,
            faces: [regular, bold, italic, bold_italic],
            cell_w,
            cell_h,
            ascent: ascent + 1.0,
        }
    }

    fn face(&self, style: u8) -> &NSFont {
        &self.faces[(style & (BOLD | ITALIC)) as usize]
    }

    /// The glyph for `c` in `style`, zero when the face lacks it and the
    /// character is drawn loose with fallback.
    fn glyph(&self, c: char, style: u8) -> u16 {
        let mut units = [0u16; 2];
        let encoded = c.encode_utf16(&mut units);
        if encoded.len() != 1 {
            return 0;
        }
        let mut glyph = 0u16;
        let font = ct(self.face(style));
        let ok = unsafe {
            font.glyphs_for_characters(NonNull::from(&mut units[0]), NonNull::from(&mut glyph), 1)
        };
        if ok {
            glyph
        } else {
            0
        }
    }
}

fn with_italic(font: &NSFont) -> Retained<NSFont> {
    let desc = font.fontDescriptor();
    let traits = desc.symbolicTraits() | objc2_app_kit::NSFontDescriptorSymbolicTraits::TraitItalic;
    let italic = desc.fontDescriptorWithSymbolicTraits(traits);
    NSFont::fontWithDescriptor_size(&italic, font.pointSize()).unwrap_or_else(|| font.retain())
}

fn measure_with(font: &NSFont, s: &str) -> f32 {
    use objc2_app_kit::NSFontAttributeName;
    let keys: [&NSString; 1] = [unsafe { NSFontAttributeName }];
    let values: [&AnyObject; 1] = [font.as_ref()];
    let attrs = objc2_foundation::NSDictionary::from_slices(&keys, &values);
    unsafe { NSString::from_str(s).sizeWithAttributes(Some(&attrs)).width as f32 }
}

/// An NSFont is a CTFont: the two are toll free bridged.
fn ct(font: &NSFont) -> &CTFont {
    unsafe { &*(font as *const NSFont).cast::<CTFont>() }
}

pub struct Ivars {
    panes: RefCell<Vec<Pane>>,
    focused: Cell<usize>,
    zoom: Cell<bool>,
    font: RefCell<TermFont>,
    /// What an input method is composing, shown at the cursor.
    marked: RefCell<String>,
    /// The pane a drag selects in.
    selecting: Cell<Option<usize>>,
    /// The project shown.
    key: RefCell<Option<String>>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements, and this view does
    // not implement Drop.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct StageView;

    impl StageView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
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

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.relayout();
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.on_key(event);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.on_mouse_down(event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.on_drag(event);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            self.ivars().selecting.set(None);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let at = self.point(event);
            if let Some(i) = self.pane_at(at) {
                self.focus(i);
                let id = self.ivars().panes.borrow()[i].id.clone();
                let screen = self
                    .window()
                    .map_or(event.locationInWindow(), |w| {
                        w.convertPointToScreen(event.locationInWindow())
                    });
                app::input(Input::PaneMenu(id, screen));
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            self.on_wheel(event);
        }

        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            self.copy_selection();
        }

        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            self.paste_clipboard();
        }

        #[unsafe(method(selectAll:))]
        fn select_all(&self, _sender: Option<&AnyObject>) {
            if let Some(c) = self.focused_console() {
                if let Ok(mut s) = c.screen.lock() {
                    let term = &mut s.term;
                    let top = term.topmost_line();
                    let bottom = term.bottommost_line();
                    let last = term.last_column();
                    let mut sel = Selection::new(SelectionType::Simple, Point::new(top, Column(0)), Side::Left);
                    sel.update(Point::new(bottom, last), Side::Right);
                    term.selection = Some(sel);
                }
                self.setNeedsDisplay(true);
            }
        }
    }

    unsafe impl NSObjectProtocol for StageView {}

    unsafe impl NSTextInputClient for StageView {
        #[unsafe(method(insertText:replacementRange:))]
        unsafe fn insert_text(&self, string: &AnyObject, _range: NSRange) {
            self.ivars().marked.borrow_mut().clear();
            let text = text_of(string);
            if !text.is_empty() {
                self.send(mac_keys::text_bytes(&text));
            }
        }

        #[unsafe(method(doCommandBySelector:))]
        unsafe fn do_command_by_selector(&self, _selector: Sel) {}

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        unsafe fn set_marked_text(
            &self,
            string: &AnyObject,
            _selected: NSRange,
            _replacement: NSRange,
        ) {
            *self.ivars().marked.borrow_mut() = text_of(string);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            self.ivars().marked.borrow_mut().clear();
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            NSRange::new(NSNotFound as NSUInteger, 0)
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            let n = self.ivars().marked.borrow().encode_utf16().count();
            if n == 0 {
                NSRange::new(NSNotFound as NSUInteger, 0)
            } else {
                NSRange::new(0, n)
            }
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            !self.ivars().marked.borrow().is_empty()
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        unsafe fn attributed_substring(
            &self,
            _range: NSRange,
            _actual: NSRangePointer,
        ) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        unsafe fn first_rect_for_character_range(
            &self,
            _range: NSRange,
            _actual: NSRangePointer,
        ) -> NSRect {
            let r = self.caret_rect().unwrap_or_default();
            let in_window = self.convertRect_toView(cg_rect(&r), None);
            match self.window() {
                Some(w) => w.convertRectToScreen(in_window),
                None => in_window,
            }
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, _point: NSPoint) -> NSUInteger {
            NSNotFound as NSUInteger
        }
    }
);

/// The text in an NSString or NSAttributedString, which input methods hand
/// over either way.
fn text_of(object: &AnyObject) -> String {
    if let Some(s) = object.downcast_ref::<NSString>() {
        return s.to_string();
    }
    if let Some(a) = object.downcast_ref::<NSAttributedString>() {
        return a.string().to_string();
    }
    String::new()
}

impl StageView {
    fn new(mtm: MainThreadMarker, frame: NSRect, font_size: f32) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            panes: RefCell::new(Vec::new()),
            focused: Cell::new(0),
            zoom: Cell::new(false),
            font: RefCell::new(TermFont::new(font_size)),
            marked: RefCell::new(String::new()),
            selecting: Cell::new(None),
            key: RefCell::new(None),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn point(&self, event: &NSEvent) -> (f32, f32) {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        (p.x as f32, p.y as f32)
    }

    fn pane_at(&self, (x, y): (f32, f32)) -> Option<usize> {
        self.ivars()
            .panes
            .borrow()
            .iter()
            .position(|p| p.rect.w > 0.0 && p.rect.contains(x, y))
    }

    fn focused_console(&self) -> Option<Arc<Console>> {
        let panes = self.ivars().panes.borrow();
        panes
            .get(self.ivars().focused.get())
            .map(|p| Arc::clone(&p.console))
    }

    fn send(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if let Some(c) = self.focused_console() {
            // Typing shows the bottom again, as in any terminal.
            if let Ok(mut s) = c.screen.lock() {
                s.term.scroll_display(Scroll::Bottom);
                s.term.selection = None;
            }
            c.note_typed();
            c.write(bytes);
        }
    }

    fn mode(&self) -> TermMode {
        self.focused_console()
            .and_then(|c| c.screen.lock().ok().map(|s| *s.term.mode()))
            .unwrap_or_else(TermMode::empty)
    }

    fn kitty(&self) -> Kitty {
        let mode = self.mode();
        Kitty {
            disambiguate: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
            events: mode.contains(TermMode::REPORT_EVENT_TYPES),
            alternates: mode.contains(TermMode::REPORT_ALTERNATE_KEYS),
            all_keys: mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC),
            text: mode.contains(TermMode::REPORT_ASSOCIATED_TEXT),
        }
    }

    fn on_key(&self, event: &NSEvent) {
        let flags = event.modifierFlags();
        let chars = event
            .characters()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let bare = event
            .charactersIgnoringModifiers()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let press = Press {
            code: event.keyCode(),
            chars: &chars,
            bare: &bare,
            shift: flags.contains(NSEventModifierFlags::Shift),
            ctrl: flags.contains(NSEventModifierFlags::Control),
            alt: flags.contains(NSEventModifierFlags::Option),
            cmd: flags.contains(NSEventModifierFlags::Command),
        };
        let app_cursor = self.mode().contains(TermMode::APP_CURSOR);
        // An input method mid composition has every key until it is done.
        if !self.ivars().marked.borrow().is_empty() && !press.cmd {
            self.interpret(event);
            return;
        }
        match mac_keys::action(press, self.kitty(), app_cursor) {
            Action::Send(bytes) => self.send(bytes),
            Action::Text => self.interpret(event),
            Action::Copy => self.copy_selection(),
            Action::Paste => self.paste_clipboard(),
            Action::NewShell => {
                if let Some(key) = self.ivars().key.borrow().clone() {
                    app::input(Input::Shell(key));
                }
            }
            Action::Font(step) => app::input(Input::Font(step)),
            Action::Zoom => {
                self.ivars().zoom.set(!self.ivars().zoom.get());
                self.relayout();
            }
            Action::Focus(dir) => {
                let rects: Vec<[i32; 4]> = self
                    .ivars()
                    .panes
                    .borrow()
                    .iter()
                    .map(|p| {
                        let r = p.rect;
                        [r.x as i32, r.y as i32, r.right() as i32, r.bottom() as i32]
                    })
                    .collect();
                if let Some(i) = layout::neighbour(&rects, self.ivars().focused.get(), dir) {
                    self.focus(i);
                }
            }
            Action::Pass => unsafe {
                let _: () = msg_send![super(self), keyDown: event];
            },
        }
    }

    /// Hands the event to AppKit's text input, which calls back on the
    /// NSTextInputClient methods with what it composed.
    fn interpret(&self, event: &NSEvent) {
        let events = NSArray::from_retained_slice(&[event.retain()]);
        self.interpretKeyEvents(&events);
    }

    fn copy_selection(&self) {
        if let Some(text) = self.focused_console().and_then(|c| c.selection_text()) {
            crate::clipboard::set_text(&text);
        }
    }

    fn paste_clipboard(&self) {
        let Some(text) = crate::clipboard::text() else {
            return;
        };
        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
        self.send(keys::paste_bytes(&text, bracketed));
    }

    /// The cell under `(x, y)` in pane `i`, and which half of it.
    fn cell_at(&self, i: usize, (x, y): (f32, f32)) -> Option<(Point, Side)> {
        let panes = self.ivars().panes.borrow();
        let pane = panes.get(i)?;
        let g = pane.grid_rect();
        let font = self.ivars().font.borrow();
        let size = pane.console.size();
        let col_f = ((x - g.x) / font.cell_w).max(0.0);
        let col = (col_f as usize).min(size.cols.saturating_sub(1) as usize);
        let row =
            (((y - g.y) / font.cell_h).max(0.0) as usize).min(size.rows.saturating_sub(1) as usize);
        let side = if col_f.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        let offset = pane
            .console
            .screen
            .lock()
            .map(|s| s.term.grid().display_offset())
            .unwrap_or(0) as i32;
        Some((Point::new(Line(row as i32 - offset), Column(col)), side))
    }

    fn on_mouse_down(&self, event: &NSEvent) {
        let at = self.point(event);
        if self.ivars().panes.borrow().is_empty() {
            app::input(Input::Menu(app::NEW_SESSION_TAG));
            return;
        }
        let Some(i) = self.pane_at(at) else {
            return;
        };
        let close = self.ivars().panes.borrow()[i].close_rect();
        if close.contains(at.0, at.1) {
            let id = self.ivars().panes.borrow()[i].id.clone();
            app::input(Input::PaneClose(id));
            return;
        }
        self.focus(i);
        let Some((point, side)) = self.cell_at(i, at) else {
            return;
        };
        let ty = match event.clickCount() {
            2 => SelectionType::Semantic,
            n if n >= 3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        let console = Arc::clone(&self.ivars().panes.borrow()[i].console);
        if let Ok(mut s) = console.screen.lock() {
            s.term.selection = Some(Selection::new(ty, point, side));
        }
        self.ivars().selecting.set(Some(i));
        self.setNeedsDisplay(true);
    }

    fn on_drag(&self, event: &NSEvent) {
        let Some(i) = self.ivars().selecting.get() else {
            return;
        };
        let Some((point, side)) = self.cell_at(i, self.point(event)) else {
            return;
        };
        let console = Arc::clone(&self.ivars().panes.borrow()[i].console);
        if let Ok(mut s) = console.screen.lock() {
            if let Some(sel) = s.term.selection.as_mut() {
                sel.update(point, side);
            }
        }
        self.setNeedsDisplay(true);
    }

    fn on_wheel(&self, event: &NSEvent) {
        let at = self.point(event);
        let Some(i) = self.pane_at(at) else {
            return;
        };
        let console = Arc::clone(&self.ivars().panes.borrow()[i].console);
        let dy = event.scrollingDeltaY();
        let precise = event.hasPreciseScrollingDeltas();
        let cell_h = self.ivars().font.borrow().cell_h as f64;
        let lines = if precise { dy / cell_h } else { dy * 3.0 };
        let lines = lines.round() as i32;
        if lines == 0 {
            return;
        }
        let mode = console
            .screen
            .lock()
            .map(|s| *s.term.mode())
            .unwrap_or_else(|_| TermMode::empty());
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            let key = if lines > 0 {
                keys::Key::Up
            } else {
                keys::Key::Down
            };
            let one = keys::key_bytes(key, keys::Mods::NONE, mode.contains(TermMode::APP_CURSOR));
            let mut bytes = Vec::new();
            for _ in 0..lines.unsigned_abs() {
                bytes.extend_from_slice(&one);
            }
            console.write(bytes);
        } else if let Ok(mut s) = console.screen.lock() {
            s.term.scroll_display(Scroll::Delta(lines));
        }
        self.setNeedsDisplay(true);
    }

    fn focus(&self, i: usize) {
        if self.ivars().focused.replace(i) != i {
            if let Some(id) = self.ivars().panes.borrow().get(i).map(|p| p.id.clone()) {
                app::input(Input::Focused(id));
            }
        }
        if let Some(w) = self.window() {
            w.makeFirstResponder(Some(self));
        }
        self.setNeedsDisplay(true);
    }

    /// Lays the panes out in the view and sizes every console to its pane.
    fn relayout(&self) {
        let b = self.bounds();
        let (w, h) = (b.size.width as i32, b.size.height as i32);
        let mut panes = self.ivars().panes.borrow_mut();
        let focused = self
            .ivars()
            .focused
            .get()
            .min(panes.len().saturating_sub(1));
        let area = (GAP, GAP, w - GAP, h - GAP);
        if self.ivars().zoom.get() && !panes.is_empty() {
            for (i, p) in panes.iter_mut().enumerate() {
                p.rect = if i == focused {
                    Rect::new(
                        area.0 as f32,
                        area.1 as f32,
                        (area.2 - area.0) as f32,
                        (area.3 - area.1) as f32,
                    )
                } else {
                    Rect::default()
                };
            }
        } else {
            let rects = layout::grid(panes.len(), area, GAP);
            for (p, [l, t, r, bt]) in panes.iter_mut().zip(rects) {
                p.rect = Rect::new(l as f32, t as f32, (r - l) as f32, (bt - t) as f32);
            }
        }
        let font = self.ivars().font.borrow();
        for p in panes.iter().filter(|p| p.rect.w > 0.0) {
            let g = p.grid_rect();
            let size = GridSize {
                cols: ((g.w / font.cell_w).floor() as u16).max(2),
                rows: ((g.h / font.cell_h).floor() as u16).max(1),
            };
            if p.console.size() != size {
                p.console.resize(size);
            }
        }
        drop(font);
        drop(panes);
        self.setNeedsDisplay(true);
    }

    fn caret_rect(&self) -> Option<Rect> {
        let panes = self.ivars().panes.borrow();
        let pane = panes.get(self.ivars().focused.get())?;
        let (row, col) = pane
            .console
            .screen
            .lock()
            .ok()
            .and_then(|s| frame::cursor_cell(&s.term))?;
        let g = pane.grid_rect();
        let font = self.ivars().font.borrow();
        Some(Rect::new(
            g.x + col as f32 * font.cell_w,
            g.y + row as f32 * font.cell_h,
            font.cell_w,
            font.cell_h,
        ))
    }

    fn paint(&self) {
        let Some(p) = Painter::current() else {
            return;
        };
        let b = self.bounds();
        let all = Rect::new(0.0, 0.0, b.size.width as f32, b.size.height as f32);
        p.fill_rect(&all, theme::window_bg());
        let panes = self.ivars().panes.borrow();
        if panes.is_empty() {
            // The first start has no tiles yet, so this is where to begin.
            let mid = all.h / 2.0;
            p.icon(
                '\u{E710}',
                theme::text_dim(),
                22.0,
                Rect::new(all.w / 2.0 - 20.0, mid - 56.0, 40.0, 40.0),
            );
            for (i, line) in [
                "Click to pick a project folder and start Claude Code in it.",
                "Its sessions show here, and as tiles at the left of the screen.",
            ]
            .iter()
            .enumerate()
            {
                let r = Rect::new(all.w / 2.0 - 260.0, mid + i as f32 * 22.0, 520.0, 22.0);
                p.text(Font::BodyCentre, theme::text_dim(), line, r);
            }
            return;
        }
        let focused = self.ivars().focused.get();
        let font = self.ivars().font.borrow();
        for (i, pane) in panes.iter().enumerate() {
            if pane.rect.w <= 0.0 {
                continue;
            }
            self.paint_pane(&p, &font, pane, i == focused);
        }
    }

    fn paint_pane(&self, p: &Painter, font: &TermFont, pane: &Pane, focused: bool) {
        let r = pane.rect;
        if let Some(wait) = pane.console.flush_sync() {
            let me = self.retain();
            super::queue::after_main(wait, move || me.setNeedsDisplay(true));
        }
        let built = pane.console.screen.lock().ok().map(|s| {
            let frame = frame::build(&s.term, focused, true, |c, style| font.glyph(c, style));
            (frame, frame::cursor_cell(&s.term))
        });
        let Some((frame, cursor)) = built else {
            return;
        };
        let bg = rgb(frame.background);
        p.fill_rounded(&r, RADIUS, bg);
        // The header: who it is and what it does.
        let head = Rect::new(r.x, r.y, r.w, HEADER_H);
        p.masked(&r, RADIUS, 1.0, || {
            p.fill_rect(&head, theme::surface());
        });
        let mut x = r.x + 10.0;
        if let Some(c) = pane.phase {
            p.fill_ellipse(x + 3.0, head.y + head.h / 2.0, 3.5, 3.5, c);
            x += 12.0;
        }
        let ink = if focused {
            theme::text()
        } else {
            theme::text_dim()
        };
        let detail = match (pane.console.exit_code(), pane.console.title()) {
            (Some(code), _) => format!("exited {code}"),
            (None, Some(t)) => t.trim().to_string(),
            (None, None) => String::new(),
        };
        let name_w = measure(Font::Header, &pane.name).min(r.w * 0.5);
        p.text(
            Font::Header,
            ink,
            &pane.name,
            Rect::new(x, head.y, name_w + 2.0, head.h),
        );
        let detail_x = x + name_w + 10.0;
        let close = pane.close_rect();
        p.text(
            Font::Small,
            theme::text_dim(),
            &detail,
            Rect::new(
                detail_x,
                head.y,
                (close.x - detail_x - 4.0).max(0.0),
                head.h,
            ),
        );
        p.icon('\u{E711}', theme::text_dim(), 10.0, close);
        let edge = if focused {
            pane.accent.with_alpha(0.8)
        } else {
            pane.phase.unwrap_or(theme::legend()).with_alpha(0.35)
        };
        p.stroke_rounded(&r.inset(0.5), RADIUS, edge, if focused { 1.5 } else { 1.0 });

        let g = pane.grid_rect();
        p.save();
        p.clip_rect(&g);
        self.paint_grid(p, font, &frame, &g);
        if focused {
            let marked = self.ivars().marked.borrow();
            if let (false, Some((row, col))) = (marked.is_empty(), cursor) {
                let at = Rect::new(
                    g.x + col as f32 * font.cell_w,
                    g.y + row as f32 * font.cell_h,
                    g.w,
                    font.cell_h,
                );
                let w = measure_with(font.face(0), &marked);
                p.fill_rect(&Rect::new(at.x, at.y, w, at.h), bg);
                draw_string(&marked, font.face(0), theme::text(), at.x, at.y);
                p.fill_rect(&Rect::new(at.x, at.bottom() - 1.0, w, 1.0), theme::text());
            }
        }
        p.restore();
    }

    fn paint_grid(&self, p: &Painter, font: &TermFont, f: &Frame, g: &Rect) {
        let cw = font.cell_w;
        let ch = font.cell_h;
        let cell = |row: usize, col: usize, cells: usize| {
            Rect::new(
                g.x + col as f32 * cw,
                g.y + row as f32 * ch,
                cells as f32 * cw,
                ch,
            )
        };
        for fill in &f.fills {
            p.fill_rect(&cell(fill.row, fill.col, fill.cells), rgb(fill.color));
        }
        let cx: &CGContext = p.cx();
        // Glyphs are drawn upside down into a flipped view unless the text
        // matrix turns them back. Their positions are in text space, which
        // the matrix turns too, so each one's y is given negated.
        CGContext::set_text_matrix(
            Some(cx),
            CGAffineTransform {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: -1.0,
                tx: 0.0,
                ty: 0.0,
            },
        );
        for run in &f.runs {
            let c = rgb(run.color);
            CGContext::set_rgb_fill_color(
                Some(cx),
                c.r as CGFloat,
                c.g as CGFloat,
                c.b as CGFloat,
                1.0,
            );
            let base = g.y + run.row as f32 * ch + font.ascent;
            let positions: Vec<CGPoint> = (0..run.glyphs.len())
                .map(|i| {
                    CGPoint::new(
                        (g.x + (run.col + i) as f32 * cw) as CGFloat,
                        -base as CGFloat,
                    )
                })
                .collect();
            let ctf = ct(font.face(run.style));
            if let (Some(glyphs), Some(pos)) = (
                NonNull::new(run.glyphs.as_ptr() as *mut u16),
                NonNull::new(positions.as_ptr() as *mut CGPoint),
            ) {
                unsafe { ctf.draw_glyphs(glyphs, pos, run.glyphs.len(), cx) };
            }
        }
        for loose in &f.loose {
            let at = cell(loose.row, loose.col, loose.cells);
            draw_string(
                &loose.text,
                font.face(loose.style),
                rgb(loose.color),
                at.x,
                at.y,
            );
        }
        for s in &f.strokes {
            let at = cell(s.row, s.col, s.cells);
            let y = match s.kind {
                Decoration::Underline => at.y + font.ascent + 2.0,
                Decoration::Strike => at.y + ch / 2.0,
            };
            p.fill_rect(&Rect::new(at.x, y, at.w, 1.0), rgb(s.color));
        }
        if let Some(c) = &f.caret {
            let at = cell(c.row, c.col, c.cells);
            let color = rgb(c.color);
            match c.shape {
                CursorShape::Beam => p.fill_rect(&Rect::new(at.x, at.y, 2.0, at.h), color),
                CursorShape::Underline => {
                    p.fill_rect(&Rect::new(at.x, at.bottom() - 2.0, at.w, 2.0), color)
                }
                CursorShape::Hidden => {}
                _ => p.stroke_rounded(&at.inset(0.5), 0.0, color, 1.0),
            }
        }
    }
}

/// The text on a console's screen, a line a row, for the snapshots: CI
/// reads it to see what a session printed.
pub fn screen_text(console: &Console) -> String {
    let Ok(s) = console.screen.lock() else {
        return String::new();
    };
    let term = &s.term;
    let grid = term.grid();
    let mut out = String::new();
    for line in 0..term.screen_lines() {
        let row = &grid[Line(line as i32)];
        let text: String = (0..term.columns()).map(|c| row[Column(c)].c).collect();
        out.push_str(text.trim_end());
        out.push('\n');
    }
    out
}

fn draw_string(s: &str, font: &NSFont, c: Color, x: f32, y: f32) {
    use objc2_app_kit::{NSFontAttributeName, NSForegroundColorAttributeName};
    let color = ns_color(c);
    let keys: [&NSString; 2] = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values: [&AnyObject; 2] = [font.as_ref(), color.as_ref()];
    let attrs = objc2_foundation::NSDictionary::from_slices(&keys, &values);
    unsafe {
        NSString::from_str(s)
            .drawAtPoint_withAttributes(NSPoint::new(x as CGFloat, y as CGFloat), Some(&attrs));
    }
}

fn rgb(c: Rgb) -> Color {
    Color {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: 1.0,
    }
}

/// The stage window and its view.
pub struct Stage {
    window: Retained<NSWindow>,
    view: Retained<StageView>,
}

impl Stage {
    pub fn new(mtm: MainThreadMarker, frame: NSRect, font_size: f32) -> Stage {
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str("Horadric"));
        window.setBackgroundColor(Some(&ns_color(theme::window_bg())));
        window.setMinSize(NSSize::new(480.0, 320.0));
        let view = StageView::new(mtm, frame, font_size);
        window.setContentView(Some(&view));
        window.makeFirstResponder(Some(&view));
        Stage { window, view }
    }

    pub fn view(&self) -> &NSView {
        &self.view
    }

    pub fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    /// Shows the project `key` with these panes, keeping the focus on the
    /// session it was on when that is still there, or on `focus`.
    pub fn show(&self, key: &str, title: &str, panes: Vec<Pane>, focus: Option<&str>) {
        let iv = self.view.ivars();
        let was = iv
            .panes
            .borrow()
            .get(iv.focused.get())
            .map(|p| p.id.clone());
        let same_project = iv.key.borrow().as_deref() == Some(key);
        let want = focus
            .map(str::to_string)
            .or_else(|| was.filter(|_| same_project));
        let focused = want
            .and_then(|id| panes.iter().position(|p| p.id == id))
            .unwrap_or(0);
        *iv.panes.borrow_mut() = panes;
        iv.focused.set(focused);
        *iv.key.borrow_mut() = Some(key.to_string());
        self.window
            .setTitle(&NSString::from_str(&format!("{title} \u{2014} Horadric")));
        self.view.relayout();
    }

    /// Updates the phase colours and names without laying out again.
    pub fn refresh(&self, update: impl Fn(&mut Pane)) {
        for p in self.view.ivars().panes.borrow_mut().iter_mut() {
            update(p);
        }
        self.view.setNeedsDisplay(true);
    }

    /// Redraws the pane of the console with this serial, if it shows.
    pub fn output(&self, serial: usize) {
        let shows = self
            .view
            .ivars()
            .panes
            .borrow()
            .iter()
            .any(|p| p.console.serial == serial);
        if shows {
            self.view.setNeedsDisplay(true);
        }
    }

    pub fn key(&self) -> Option<String> {
        self.view.ivars().key.borrow().clone()
    }

    pub fn focused_id(&self) -> Option<String> {
        let iv = self.view.ivars();
        iv.panes
            .borrow()
            .get(iv.focused.get())
            .map(|p| p.id.clone())
    }

    pub fn shows(&self, id: &str) -> bool {
        self.view.ivars().panes.borrow().iter().any(|p| p.id == id)
    }

    pub fn set_font(&self, step: FontStep) -> f32 {
        let size = {
            let font = self.view.ivars().font.borrow();
            match step {
                FontStep::Reset => FONT_DEFAULT,
                _ => keys::font_size(font.size, step),
            }
        };
        *self.view.ivars().font.borrow_mut() = TermFont::new(size);
        self.view.relayout();
        size
    }

    pub fn present(&self) {
        self.window.makeKeyAndOrderFront(None);
        self.window.makeFirstResponder(Some(&*self.view));
    }

    pub fn frame(&self) -> NSRect {
        self.window.frame()
    }
}
