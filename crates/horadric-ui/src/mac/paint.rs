//! Core Graphics drawing for the Mac's windows, with the same primitives
//! the Direct2D painter has, so the look ports across a call at a time.
//!
//! Every view is flipped, so `y` grows downwards as in the layout. Text is
//! drawn by AppKit's string drawing, which knows a flipped view and falls
//! back across fonts for any character by itself.

use std::cell::RefCell;
use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSColor, NSFont, NSFontAttributeName, NSFontWeightMedium, NSFontWeightRegular,
    NSFontWeightSemibold, NSForegroundColorAttributeName, NSGraphicsContext, NSImage,
    NSImageSymbolConfiguration, NSKernAttributeName, NSLineBreakMode, NSMutableParagraphStyle,
    NSParagraphStyleAttributeName, NSStringDrawing, NSTextAlignment,
};
use objc2_core_foundation::{CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColorSpace, CGContext, CGGradient, CGGradientDrawingOptions, CGPath};
use objc2_foundation::{NSDictionary, NSNumber, NSString};

use crate::layout::Rect;
use crate::theme::Color;

/// How many shapes make one soft edge, as on Windows.
const BLUR_STEPS: usize = 8;

/// The kinds of text the windows draw, as the Windows painter's formats.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Font {
    /// The project's name at the top of its cluster.
    Display,
    /// A session's name on its tile.
    Name,
    Body,
    Small,
    SmallRight,
    /// The counts in a cluster's header.
    Chip,
    /// A pane's header.
    Header,
}

impl Font {
    fn spec(self) -> (CGFloat, Weight, NSTextAlignment) {
        match self {
            Font::Display => (15.0, Weight::Semibold, NSTextAlignment::Left),
            Font::Name => (13.0, Weight::Semibold, NSTextAlignment::Left),
            Font::Body => (13.5, Weight::Regular, NSTextAlignment::Left),
            Font::Small => (12.0, Weight::Regular, NSTextAlignment::Left),
            Font::SmallRight => (12.0, Weight::Regular, NSTextAlignment::Right),
            Font::Chip => (11.0, Weight::Semibold, NSTextAlignment::Left),
            Font::Header => (12.0, Weight::Medium, NSTextAlignment::Left),
        }
    }
}

#[derive(Clone, Copy)]
enum Weight {
    Regular,
    Medium,
    Semibold,
}

/// SF Symbols by name and size, a quarter point apart.
type Symbols = HashMap<(&'static str, u32), Option<Retained<NSImage>>>;

thread_local! {
    static FONTS: RefCell<HashMap<Font, Retained<NSFont>>> = RefCell::new(HashMap::new());
    static SYMBOLS: RefCell<Symbols> = RefCell::new(HashMap::new());
}

pub fn ns_font(f: Font) -> Retained<NSFont> {
    FONTS.with(|fonts| {
        fonts
            .borrow_mut()
            .entry(f)
            .or_insert_with(|| {
                let (size, weight, _) = f.spec();
                let weight = unsafe {
                    match weight {
                        Weight::Regular => NSFontWeightRegular,
                        Weight::Medium => NSFontWeightMedium,
                        Weight::Semibold => NSFontWeightSemibold,
                    }
                };
                NSFont::systemFontOfSize_weight(size, weight)
            })
            .clone()
    })
}

pub fn ns_color(c: Color) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        c.r as CGFloat,
        c.g as CGFloat,
        c.b as CGFloat,
        c.a as CGFloat,
    )
}

pub fn cg_rect(r: &Rect) -> CGRect {
    CGRect::new(
        CGPoint::new(r.x as CGFloat, r.y as CGFloat),
        CGSize::new(r.w.max(0.0) as CGFloat, r.h.max(0.0) as CGFloat),
    )
}

/// The attributes for text in `font` and `c`, cut off with an ellipsis.
fn attributes(
    font: &NSFont,
    c: Color,
    align: NSTextAlignment,
    kern: Option<f32>,
) -> Retained<NSDictionary<NSString, AnyObject>> {
    let style = NSMutableParagraphStyle::new();
    style.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    style.setAlignment(align);
    let color = ns_color(c);
    let mut keys: Vec<&NSString> = unsafe {
        vec![
            NSFontAttributeName,
            NSForegroundColorAttributeName,
            NSParagraphStyleAttributeName,
        ]
    };
    let mut values: Vec<&AnyObject> = vec![font.as_ref(), color.as_ref(), style.as_ref()];
    let kern = kern.map(|k| NSNumber::new_f64(k as f64));
    if let Some(k) = &kern {
        keys.push(unsafe { NSKernAttributeName });
        values.push(k.as_ref());
    }
    NSDictionary::from_slices(&keys, &values)
}

/// The height of one line of `font`.
pub fn line_height(font: &NSFont) -> f32 {
    (font.ascender() - font.descender() + font.leading()) as f32
}

/// How wide `s` is in `f`.
pub fn measure(f: Font, s: &str) -> f32 {
    let font = ns_font(f);
    let attrs = attributes(&font, Color::rgb(0), NSTextAlignment::Left, None);
    let size = unsafe { NSString::from_str(s).sizeWithAttributes(Some(&attrs)) };
    size.width as f32
}

/// Draws on one view's context for one frame.
pub struct Painter {
    cx: Retained<CGContext>,
    space: Option<objc2_core_foundation::CFRetained<CGColorSpace>>,
}

impl Painter {
    /// A painter on the current AppKit context, as in a view's
    /// `drawRect:`. None when there is no context to draw on.
    pub fn current() -> Option<Painter> {
        let ns = NSGraphicsContext::currentContext()?;
        Some(Painter {
            cx: ns.CGContext(),
            space: CGColorSpace::new_device_rgb(),
        })
    }

    pub fn cx(&self) -> &CGContext {
        &self.cx
    }

    fn set_fill(&self, c: Color) {
        CGContext::set_rgb_fill_color(
            Some(&self.cx),
            c.r as CGFloat,
            c.g as CGFloat,
            c.b as CGFloat,
            c.a as CGFloat,
        );
    }

    fn set_stroke(&self, c: Color) {
        CGContext::set_rgb_stroke_color(
            Some(&self.cx),
            c.r as CGFloat,
            c.g as CGFloat,
            c.b as CGFloat,
            c.a as CGFloat,
        );
    }

    fn rounded_path(r: &Rect, radius: f32) -> objc2_core_foundation::CFRetained<CGPath> {
        let radius = radius.max(0.0).min(r.w / 2.0).min(r.h / 2.0) as CGFloat;
        unsafe { CGPath::with_rounded_rect(cg_rect(r), radius, radius, std::ptr::null()) }
    }

    pub fn fill_rect(&self, r: &Rect, c: Color) {
        if r.w <= 0.0 || r.h <= 0.0 || c.a <= 0.0 {
            return;
        }
        self.set_fill(c);
        CGContext::fill_rect(Some(&self.cx), cg_rect(r));
    }

    pub fn fill_rounded(&self, r: &Rect, radius: f32, c: Color) {
        if r.w <= 0.0 || r.h <= 0.0 || c.a <= 0.0 {
            return;
        }
        self.set_fill(c);
        let path = Self::rounded_path(r, radius);
        CGContext::add_path(Some(&self.cx), Some(&path));
        CGContext::fill_path(Some(&self.cx));
    }

    pub fn stroke_rounded(&self, r: &Rect, radius: f32, c: Color, width: f32) {
        if r.w <= 0.0 || r.h <= 0.0 || c.a <= 0.0 {
            return;
        }
        self.set_stroke(c);
        CGContext::set_line_width(Some(&self.cx), width as CGFloat);
        let path = Self::rounded_path(r, radius);
        CGContext::add_path(Some(&self.cx), Some(&path));
        CGContext::stroke_path(Some(&self.cx));
    }

    pub fn fill_ellipse(&self, x: f32, y: f32, rx: f32, ry: f32, c: Color) {
        self.set_fill(c);
        let r = Rect::new(x - rx, y - ry, 2.0 * rx, 2.0 * ry);
        CGContext::fill_ellipse_in_rect(Some(&self.cx), cg_rect(&r));
    }

    /// A dashed rounded outline, `on` and `off` long.
    pub fn dashed_rounded(&self, r: &Rect, radius: f32, c: Color, on: f32, off: f32) {
        let dash = [on as CGFloat, off as CGFloat];
        self.save();
        unsafe { CGContext::set_line_dash(Some(&self.cx), 0.0, dash.as_ptr(), 2) };
        CGContext::set_line_cap(Some(&self.cx), objc2_core_graphics::CGLineCap::Round);
        self.stroke_rounded(r, radius, c, 1.0);
        self.restore();
    }

    pub fn save(&self) {
        CGContext::save_g_state(Some(&self.cx));
    }

    pub fn restore(&self) {
        CGContext::restore_g_state(Some(&self.cx));
    }

    /// Draws `paint` clipped to the rounded rectangle `r`, at `opacity`.
    pub fn masked(&self, r: &Rect, radius: f32, opacity: f32, paint: impl FnOnce()) {
        self.save();
        let path = Self::rounded_path(r, radius);
        CGContext::add_path(Some(&self.cx), Some(&path));
        CGContext::clip(Some(&self.cx));
        let layered = opacity < 1.0;
        if layered {
            CGContext::set_alpha(Some(&self.cx), opacity.clamp(0.0, 1.0) as CGFloat);
            unsafe { CGContext::begin_transparency_layer(Some(&self.cx), None) };
        }
        paint();
        if layered {
            CGContext::end_transparency_layer(Some(&self.cx));
        }
        self.restore();
    }

    pub fn clip_rect(&self, r: &Rect) {
        CGContext::clip_to_rect(Some(&self.cx), cg_rect(r));
    }

    fn gradient(
        &self,
        stops: &[(f32, Color)],
    ) -> Option<objc2_core_foundation::CFRetained<CGGradient>> {
        let mut components = Vec::with_capacity(stops.len() * 4);
        let mut locations = Vec::with_capacity(stops.len());
        for (at, c) in stops {
            components.extend([c.r, c.g, c.b, c.a].map(|v| v as CGFloat));
            locations.push(*at as CGFloat);
        }
        unsafe {
            CGGradient::with_color_components(
                self.space.as_deref(),
                components.as_ptr(),
                locations.as_ptr(),
                stops.len(),
            )
        }
    }

    /// A vertical gradient across `r`, from `from_y` to `to_y`.
    pub fn fill_gradient(&self, r: &Rect, (from_y, to_y): (f32, f32), stops: &[(f32, Color)]) {
        let Some(g) = self.gradient(stops) else {
            return;
        };
        self.save();
        self.clip_rect(r);
        CGContext::draw_linear_gradient(
            Some(&self.cx),
            Some(&g),
            CGPoint::new(r.x as CGFloat, from_y as CGFloat),
            CGPoint::new(r.x as CGFloat, to_y as CGFloat),
            CGGradientDrawingOptions::DrawsBeforeStartLocation
                | CGGradientDrawingOptions::DrawsAfterEndLocation,
        );
        self.restore();
    }

    /// A rounded rectangle filled top to bottom through `stops`.
    pub fn fill_rounded_gradient(
        &self,
        r: &Rect,
        radius: f32,
        stops: &[(f32, Color)],
        opacity: f32,
    ) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let Some(g) = self.gradient(stops) else {
            return;
        };
        self.save();
        let path = Self::rounded_path(r, radius);
        CGContext::add_path(Some(&self.cx), Some(&path));
        CGContext::clip(Some(&self.cx));
        CGContext::set_alpha(Some(&self.cx), opacity.clamp(0.0, 1.0) as CGFloat);
        CGContext::draw_linear_gradient(
            Some(&self.cx),
            Some(&g),
            CGPoint::new(r.x as CGFloat, r.y as CGFloat),
            CGPoint::new(r.x as CGFloat, r.bottom() as CGFloat),
            CGGradientDrawingOptions::DrawsBeforeStartLocation
                | CGGradientDrawingOptions::DrawsAfterEndLocation,
        );
        self.restore();
    }

    /// A soft round glow, strongest at its centre.
    pub fn glow_dot(&self, x: f32, y: f32, radius: f32, c: Color, strength: f32) {
        if strength <= 0.0 {
            return;
        }
        let c = c.with_alpha(1.0);
        let Some(g) = self.gradient(&[
            (0.0, c),
            (0.45, c.with_alpha(0.4)),
            (1.0, c.with_alpha(0.0)),
        ]) else {
            return;
        };
        self.save();
        CGContext::set_alpha(Some(&self.cx), strength.min(1.0) as CGFloat);
        let at = CGPoint::new(x as CGFloat, y as CGFloat);
        CGContext::draw_radial_gradient(
            Some(&self.cx),
            Some(&g),
            at,
            0.0,
            at,
            radius as CGFloat,
            CGGradientDrawingOptions::empty(),
        );
        self.restore();
    }

    /// `r`'s shadow, moved by `(dx, dy)` and blurred `blur` wide: the same
    /// shape again and again, each a little bigger and fainter.
    pub fn cast(&self, r: &Rect, radius: f32, (dx, dy): (f32, f32), blur: f32, c: Color) {
        for s in blur_steps(blur) {
            let e = Rect::new(r.x + dx, r.y + dy, r.w, r.h).inset(-s);
            if e.w > 0.0 && e.h > 0.0 {
                self.fill_rounded(&e, radius + s, c.fade(1.0 / BLUR_STEPS as f32));
            }
        }
    }

    /// Light and shade inside `r`, along its edges: `near` on the top
    /// left rim, `far` on the bottom right, each `offset` deep and `blur`
    /// soft.
    #[allow(clippy::too_many_arguments)]
    pub fn inner(
        &self,
        r: &Rect,
        radius: f32,
        (offset, blur): (f32, f32),
        near: Color,
        far: Color,
        opacity: f32,
    ) {
        if opacity <= 0.0 {
            return;
        }
        self.masked(r, radius, opacity, || {
            self.hollow(r, radius, (offset, offset), blur, near);
            self.hollow(r, radius, (-offset, -offset), blur, far);
        });
    }

    /// Everything outside `r` moved by `(dx, dy)`, blurred: inside a clip
    /// to `r` itself, the rim the moved copy leaves uncovered.
    pub fn hollow(&self, r: &Rect, radius: f32, (dx, dy): (f32, f32), blur: f32, c: Color) {
        let reach = 2.0 * (dx.abs().max(dy.abs()) + blur + 2.0);
        for s in blur_steps(blur) {
            let e = Rect::new(r.x + dx, r.y + dy, r.w, r.h).inset(-s - reach / 2.0);
            let corner = (radius + s).max(0.0) + reach / 2.0;
            self.stroke_rounded(&e, corner, c.fade(1.0 / BLUR_STEPS as f32), reach);
        }
    }

    /// One line of text in `f`, vertically centred in `r`, cut off with an
    /// ellipsis when it does not fit.
    pub fn text(&self, f: Font, c: Color, s: &str, r: Rect) {
        self.text_kerned(f, c, s, r, None);
    }

    /// [`Painter::text`] with the letters `kern` apart.
    pub fn text_kerned(&self, f: Font, c: Color, s: &str, r: Rect, kern: Option<f32>) {
        if s.is_empty() || r.w <= 0.0 || c.a <= 0.0 {
            return;
        }
        let font = ns_font(f);
        let (_, _, align) = f.spec();
        let attrs = attributes(&font, c, align, kern);
        let h = line_height(&font);
        let at = Rect::new(r.x, r.y + (r.h - h) / 2.0, r.w, h + 1.0);
        let one_line: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
        unsafe {
            NSString::from_str(&one_line).drawInRect_withAttributes(cg_rect(&at), Some(&attrs))
        };
    }

    /// The SF Symbol for `glyph`, a Segoe Fluent Icons code point, in `c`,
    /// `size` points tall, centred in `r`.
    pub fn icon(&self, glyph: char, c: Color, size: f32, r: Rect) {
        self.symbol(crate::symbols::sf_symbol(glyph), c, size, r);
    }

    /// The SF Symbol `name` in `c`, centred in `r`.
    pub fn symbol(&self, name: &'static str, c: Color, size: f32, r: Rect) {
        let Some(image) = symbol_image(name, size) else {
            return;
        };
        let s = image.size();
        let (w, h) = (s.width as f32, s.height as f32);
        let at = Rect::new(r.x + (r.w - w) / 2.0, r.y + (r.h - h) / 2.0, w, h);
        let Some(cg) = (unsafe {
            image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None)
        }) else {
            return;
        };
        // A mask drawn flipped, since the view is: move to its bottom edge
        // and turn y round, then fill the colour through it.
        self.save();
        CGContext::translate_ctm(Some(&self.cx), at.x as CGFloat, (at.y + at.h) as CGFloat);
        CGContext::scale_ctm(Some(&self.cx), 1.0, -1.0);
        let local = Rect::new(0.0, 0.0, at.w, at.h);
        CGContext::clip_to_mask(Some(&self.cx), cg_rect(&local), Some(&cg));
        self.set_fill(c);
        CGContext::fill_rect(Some(&self.cx), cg_rect(&local));
        self.restore();
    }
}

fn symbol_image(name: &'static str, size: f32) -> Option<Retained<NSImage>> {
    let key = (name, (size * 4.0) as u32);
    SYMBOLS.with(|cache| {
        cache
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| {
                let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str(name),
                    None,
                )?;
                let config = unsafe {
                    NSImageSymbolConfiguration::configurationWithPointSize_weight(
                        size as CGFloat,
                        NSFontWeightRegular,
                    )
                };
                image.imageWithSymbolConfiguration(&config).or(Some(image))
            })
            .clone()
    })
}

/// How far each of the shapes that make a `blur` wide soft edge grows past
/// the sharp one, evenly from `-blur / 2` to `blur / 2`.
fn blur_steps(blur: f32) -> impl Iterator<Item = f32> {
    (0..BLUR_STEPS).map(move |i| blur * ((i as f32 + 0.5) / BLUR_STEPS as f32 - 0.5))
}
