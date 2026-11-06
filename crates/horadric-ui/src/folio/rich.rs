//! Pieces of the transcript built once and drawn many times. A block is
//! built into a [`Scene`]: what to fill and stroke, its text laid out,
//! where a click goes, and the pieces of text a selection or a find can
//! reach. The scene is kept until the block or the width changes, so a
//! frame only draws. A reply keeps a scene per paragraph too, so while it
//! streams only its last paragraph is laid out again.
//!
//! Here too: markdown paragraphs, code blocks coloured by the file
//! viewer's grammars, tables, and a prompt's images decoded by WIC.

use std::cell::RefCell;
use std::collections::HashMap;

use horadric_core::markdown::{Align, Callout, Inline, Style};
use windows::core::{Interface, Result, HSTRING, PCWSTR};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, ID2D1RenderTarget, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextFormat, IDWriteTextLayout, IDWriteTextLayout1, IDWriteTypography,
    DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG, DWRITE_FONT_FEATURE_TAG_SUBSCRIPT,
    DWRITE_FONT_FEATURE_TAG_SUPERSCRIPT, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_FONT_STYLE_ITALIC,
    DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_MEDIUM,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_LINE_METRICS,
    DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_RANGE,
    DWRITE_WORD_WRAPPING_EMERGENCY_BREAK, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

use super::flow::{self, Font, Kind, Marker, Para};
use super::transcript::Act;
use super::view::{d2d, reading, text_format, Gfx, Hit, Palette, FONT, READ};
use crate::layout::Rect;
use crate::render::{self, color, Gpu};
use crate::theme::Color;

/// One line of a diff or of shell output, the same for every line so
/// rows line up with their backgrounds.
pub(super) const MONO_LINE: f32 = 19.0;

/// Which of the frame's one line formats an [`Item::Line`] is set in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum F {
    Line,
    Small,
    Title,
    Button,
    Icon,
    /// Small and right aligned, for list markers and line numbers.
    Right,
    /// Small, centred.
    Centre,
}

pub(super) enum Item {
    Fill(Rect, f32, Color),
    Stroke(Rect, f32, Color, f32),
    Text(IDWriteTextLayout, f32, f32, Color),
    Line(String, F, Rect, Color),
    Image(ID2D1Bitmap, Rect),
    /// A rune of the rim's: which, its centre x, its top, height, ink, stroke.
    Rune(usize, f32, f32, f32, Color, f32),
    Clip(Rect),
    Unclip,
}

impl Clone for Item {
    fn clone(&self) -> Item {
        match self {
            Item::Fill(r, k, c) => Item::Fill(*r, *k, *c),
            Item::Stroke(r, k, c, w) => Item::Stroke(*r, *k, *c, *w),
            Item::Text(l, x, y, c) => Item::Text(l.clone(), *x, *y, *c),
            Item::Line(s, f, r, c) => Item::Line(s.clone(), *f, *r, *c),
            Item::Image(b, r) => Item::Image(b.clone(), *r),
            Item::Rune(n, x, y, h, c, w) => Item::Rune(*n, *x, *y, *h, *c, *w),
            Item::Clip(r) => Item::Clip(*r),
            Item::Unclip => Item::Unclip,
        }
    }
}

/// A piece of text a selection or a find can reach.
#[derive(Clone)]
pub(super) struct Unit {
    pub(super) layout: IDWriteTextLayout,
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) w: f32,
    pub(super) h: f32,
    pub(super) text: String,
}

/// A block built, at its own origin: x from the column's left, y from the
/// block's top.
#[derive(Clone, Default)]
pub(super) struct Scene {
    pub(super) h: f32,
    pub(super) items: Vec<Item>,
    /// Drawn only while the pointer is over the block, such as a reply's
    /// copy button.
    pub(super) hover: Vec<Item>,
    pub(super) hits: Vec<(Rect, Hit)>,
    pub(super) hover_hits: Vec<(Rect, Hit)>,
    /// In the order they are read; a unit's key is its index.
    pub(super) units: Vec<Unit>,
}

fn shift(r: &Rect, dx: f32, dy: f32) -> Rect {
    Rect::new(r.x + dx, r.y + dy, r.w, r.h)
}

impl Item {
    fn moved(&self, dx: f32, dy: f32) -> Item {
        match self {
            Item::Fill(r, k, c) => Item::Fill(shift(r, dx, dy), *k, *c),
            Item::Stroke(r, k, c, w) => Item::Stroke(shift(r, dx, dy), *k, *c, *w),
            Item::Text(l, x, y, c) => Item::Text(l.clone(), x + dx, y + dy, *c),
            Item::Line(s, f, r, c) => Item::Line(s.clone(), *f, shift(r, dx, dy), *c),
            Item::Image(b, r) => Item::Image(b.clone(), shift(r, dx, dy)),
            Item::Rune(n, x, y, h, c, w) => Item::Rune(*n, x + dx, y + dy, *h, *c, *w),
            Item::Clip(r) => Item::Clip(shift(r, dx, dy)),
            Item::Unclip => Item::Unclip,
        }
    }
}

impl Scene {
    pub(super) fn fill(&mut self, r: Rect, radius: f32, c: Color) {
        self.items.push(Item::Fill(r, radius, c));
    }

    pub(super) fn stroke(&mut self, r: Rect, radius: f32, c: Color, w: f32) {
        self.items.push(Item::Stroke(r, radius, c, w));
    }

    pub(super) fn line(&mut self, s: impl Into<String>, f: F, r: Rect, c: Color) {
        self.items.push(Item::Line(s.into(), f, r, c));
    }

    pub(super) fn text(&mut self, l: &IDWriteTextLayout, x: f32, y: f32, c: Color) {
        self.items.push(Item::Text(l.clone(), x, y, c));
    }

    /// Text that can be selected and found, as well as drawn.
    pub(super) fn unit(&mut self, l: &IDWriteTextLayout, x: f32, y: f32, c: Color, text: &str) {
        self.text(l, x, y, c);
        let (w, h) = render::text_size(l);
        self.units.push(Unit {
            layout: l.clone(),
            x,
            y,
            w: w.max(unsafe { l.GetMaxWidth() }.min(4000.0)),
            h,
            text: text.to_string(),
        });
    }

    pub(super) fn hit(&mut self, r: Rect, h: Hit) {
        self.hits.push((r, h));
    }

    /// Another scene drawn into this one at (`dx`, `dy`).
    pub(super) fn append(&mut self, o: &Scene, dx: f32, dy: f32) {
        self.items.extend(o.items.iter().map(|i| i.moved(dx, dy)));
        self.hover.extend(o.hover.iter().map(|i| i.moved(dx, dy)));
        (self.hits).extend(o.hits.iter().map(|(r, h)| (shift(r, dx, dy), h.clone())));
        (self.hover_hits).extend(
            o.hover_hits
                .iter()
                .map(|(r, h)| (shift(r, dx, dy), h.clone())),
        );
        self.units.extend(o.units.iter().map(|u| Unit {
            x: u.x + dx,
            y: u.y + dy,
            ..u.clone()
        }));
    }
}

/// The formats the transcript sets text in beyond the frame's own.
pub(super) struct Fonts {
    pub(super) body: IDWriteTextFormat,
    pub(super) h1: IDWriteTextFormat,
    pub(super) h2: IDWriteTextFormat,
    pub(super) h3: IDWriteTextFormat,
    /// Levels 4 to 6: a small label, set in capitals and spaced out.
    pub(super) h4: IDWriteTextFormat,
    /// Every figure the same width, for list numbers that line up.
    pub(super) tnum: Option<IDWriteTypography>,
    /// The font's own raised and lowered glyphs, for `<sup>`, `<sub>`
    /// and footnote marks.
    pub(super) sups: Option<IDWriteTypography>,
    pub(super) subs: Option<IDWriteTypography>,
    /// A code block's text: it breaks long lines rather than hiding them.
    pub(super) code: IDWriteTextFormat,
    /// Diffs and shell output: one line a row, never wrapped.
    pub(super) rows: IDWriteTextFormat,
    pub(super) right: IDWriteTextFormat,
    pub(super) centre: IDWriteTextFormat,
    pub(super) small_wrap: IDWriteTextFormat,
    /// The terminal font's family, which code is set in.
    pub(super) family: HSTRING,
}

impl Fonts {
    pub(super) unsafe fn new(gpu: &Gpu, family: &str) -> Result<Fonts> {
        let n = DWRITE_FONT_WEIGHT_NORMAL;
        let family = HSTRING::from(family);
        let mono = PCWSTR(family.as_ptr());
        let code = text_format(gpu, mono, 13.5, n, false, false)?;
        code.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK)?;
        let rows = text_format(gpu, mono, 13.0, n, false, false)?;
        rows.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        rows.SetLineSpacing(
            DWRITE_LINE_SPACING_METHOD_UNIFORM,
            MONO_LINE,
            MONO_LINE * 0.78,
        )?;
        let right = text_format(gpu, FONT, 12.5, n, false, true)?;
        right.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
        let centre = text_format(gpu, FONT, 12.5, n, false, true)?;
        centre.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        let typography = |tag: DWRITE_FONT_FEATURE_TAG| -> Option<IDWriteTypography> {
            let t = gpu.dw.CreateTypography().ok()?;
            t.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: tag,
                parameter: 1,
            })
            .ok()?;
            Some(t)
        };
        let tnum = typography(DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES);
        let sups = typography(DWRITE_FONT_FEATURE_TAG_SUPERSCRIPT);
        let subs = typography(DWRITE_FONT_FEATURE_TAG_SUBSCRIPT);
        Ok(Fonts {
            body: reading(gpu, READ, n)?,
            h1: heading(gpu, 24.0, 1.3)?,
            h2: heading(gpu, 20.0, 1.35)?,
            h3: heading(gpu, 17.5, 1.45)?,
            h4: heading(gpu, 13.0, 1.6)?,
            tnum,
            sups,
            subs,
            code,
            rows,
            right,
            centre,
            small_wrap: text_format(gpu, FONT, 13.5, n, false, false)?,
            family,
        })
    }

    pub(super) fn of(&self, f: Font) -> &IDWriteTextFormat {
        match f {
            Font::Body => &self.body,
            Font::H1 => &self.h1,
            Font::H2 => &self.h2,
            Font::H3 => &self.h3,
            Font::H4 => &self.h4,
        }
    }
}

/// A heading's format: semibold, its lines closer than prose's so a title
/// that wraps still reads as one, the baseline centred as in [`reading`].
unsafe fn heading(gpu: &Gpu, size: f32, leading: f32) -> Result<IDWriteTextFormat> {
    let f = text_format(gpu, FONT, size, DWRITE_FONT_WEIGHT_SEMI_BOLD, false, false)?;
    // Segoe UI's ascent and descent, as fractions of its size.
    let (ascent, descent) = (1.079, 0.251);
    let line = (size * leading).round();
    let baseline = (line - size * (ascent + descent)) / 2.0 + size * ascent;
    f.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line, baseline)?;
    Ok(f)
}

/// What building a scene needs: the frame, its colours, the formats and
/// a brush per colour for coloured runs of text.
pub(super) struct Ctx<'a> {
    pub(super) gfx: &'a Gfx,
    pub(super) gpu: &'a Gpu,
    pub(super) pal: &'a Palette,
    pub(super) fonts: &'a Fonts,
    pub(super) brushes: &'a RefCell<HashMap<u32, ID2D1SolidColorBrush>>,
    /// The project's folder, for showing paths inside it short.
    pub(super) dir: &'a std::path::Path,
    /// Step result images, decoded once.
    pub(super) thumbs: &'a StepThumbs,
}

/// Decoded thumbnails by key, None where one would not decode.
pub(super) type StepThumbs = RefCell<HashMap<String, Option<(ID2D1Bitmap, u32, u32)>>>;

pub(super) fn range(r: std::ops::Range<u32>) -> DWRITE_TEXT_RANGE {
    DWRITE_TEXT_RANGE {
        startPosition: r.start,
        length: r.end - r.start,
    }
}

impl Ctx<'_> {
    /// Text laid out at `width`, wrapping.
    pub(super) fn layout(
        &self,
        f: &IDWriteTextFormat,
        s: &str,
        width: f32,
    ) -> Option<IDWriteTextLayout> {
        render::wrapped(self.gpu, f, s, width.max(1.0)).ok()
    }

    /// Text laid out on one line, as wide as it is.
    pub(super) fn one_line(&self, f: &IDWriteTextFormat, s: &str) -> Option<IDWriteTextLayout> {
        let l = render::wrapped(self.gpu, f, s, 100_000.0).ok()?;
        unsafe { l.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP).ok()? };
        Some(l)
    }

    pub(super) fn brush(&self, c: Color) -> Option<ID2D1SolidColorBrush> {
        let k = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        let key = k(c.r) << 24 | k(c.g) << 16 | k(c.b) << 8 | k(c.a);
        let mut map = self.brushes.borrow_mut();
        if let Some(b) = map.get(&key) {
            return Some(b.clone());
        }
        let b = unsafe { self.gfx.rt.CreateSolidColorBrush(&color(c), None) }.ok()?;
        map.insert(key, b.clone());
        Some(b)
    }

    /// Colours a run of a layout.
    pub(super) fn paint(&self, l: &IDWriteTextLayout, r: std::ops::Range<u32>, c: Color) {
        if r.is_empty() {
            return;
        }
        if let Some(b) = self
            .brush(c)
            .and_then(|b| b.cast::<windows::core::IUnknown>().ok())
        {
            unsafe {
                let _ = l.SetDrawingEffect(&b, range(r));
            }
        }
    }

    pub(super) fn link_ink(&self) -> Color {
        self.pal.accent.mix(self.pal.fg, 0.45)
    }

    fn code_ink(&self) -> Color {
        Color::rgb(0xE2C08D).mix(self.pal.fg, 0.25)
    }

    /// A run of inline markdown laid out at `width` in `f`, its styles
    /// set, into `s` at (`x`, `y`), with its links as hits. Returns its
    /// height and the height of its first line.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn inline(
        &self,
        s: &mut Scene,
        inl: &Inline,
        f: &IDWriteTextFormat,
        x: f32,
        y: f32,
        width: f32,
        ink: Color,
        align: Align,
    ) -> (f32, f32) {
        self.inline_as(s, inl, f, (x, y, width), ink, align, Look::default())
    }

    /// [`Ctx::inline`], with the text shown as `look` says.
    #[allow(clippy::too_many_arguments)]
    unsafe fn inline_as(
        &self,
        s: &mut Scene,
        inl: &Inline,
        f: &IDWriteTextFormat,
        (x, y, width): (f32, f32, f32),
        ink: Color,
        align: Align,
        look: Look<'_>,
    ) -> (f32, f32) {
        let caps;
        let shown = if look.caps {
            caps = capitals(&inl.text);
            &caps
        } else {
            &inl.text
        };
        let Some(l) = self.layout(f, shown, width) else {
            return (0.0, 0.0);
        };
        let wide: Vec<u16> = shown.encode_utf16().collect();
        let all = range(0..wide.len() as u32);
        let a = match align {
            Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            _ => DWRITE_TEXT_ALIGNMENT_LEADING,
        };
        let _ = l.SetTextAlignment(a);
        if let Some(weight) = look.weight {
            let _ = l.SetFontWeight(weight, all);
        }
        if look.track > 0.0 {
            if let Ok(l1) = l.cast::<IDWriteTextLayout1>() {
                let _ = l1.SetCharacterSpacing(0.0, look.track, 0.0, all);
            }
        }
        let size = f.GetFontSize();
        let mut code_runs = Vec::new();
        let mut keys = Vec::new();
        let mut links = Vec::new();
        let mut images = Vec::new();
        for (r, style) in inl.utf16_spans() {
            let dr = range(r.clone());
            match style {
                Style::Bold => {
                    let _ = l.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, dr);
                }
                Style::Italic => {
                    let _ = l.SetFontStyle(DWRITE_FONT_STYLE_ITALIC, dr);
                }
                Style::Strike => {
                    let _ = l.SetStrikethrough(true, dr);
                }
                Style::Code => {
                    let _ = l.SetFontFamilyName(PCWSTR(self.fonts.family.as_ptr()), dr);
                    let _ = l.SetFontSize(size * 0.88, dr);
                    pad(&l, &wide, r.clone(), CODE_PAD);
                    self.paint(&l, r.clone(), self.code_ink());
                    code_runs.push(r);
                }
                Style::Key => {
                    let _ = l.SetFontFamilyName(FONT, dr);
                    let _ = l.SetFontSize(size * 0.8, dr);
                    let _ = l.SetFontWeight(DWRITE_FONT_WEIGHT_MEDIUM, dr);
                    pad(&l, &wide, r.clone(), KEY_PAD);
                    self.paint(&l, r.clone(), self.pal.fg.with_alpha(0.9));
                    keys.push(r);
                }
                Style::Link(url) => {
                    let _ = l.SetUnderline(true, dr);
                    self.paint(&l, r.clone(), self.link_ink());
                    links.push((r, Hit::Tr(Act::Link(url.clone()))));
                }
                Style::Path { path, line } => {
                    self.paint(&l, r.clone(), self.link_ink());
                    links.push((r, Hit::Tr(Act::Path(path.clone(), *line))));
                }
                Style::Sup | Style::Sub => {
                    let t = if *style == Style::Sup { &self.fonts.sups } else { &self.fonts.subs };
                    if let Some(t) = t {
                        let _ = l.SetTypography(t, dr);
                    }
                }
                Style::Footnote(label) => {
                    if let Some(t) = &self.fonts.sups {
                        let _ = l.SetTypography(t, dr);
                    }
                    let _ = l.SetFontWeight(DWRITE_FONT_WEIGHT_SEMI_BOLD, dr);
                    pad_sides(&l, &wide, r.clone(), 1.5, 0.0);
                    self.paint(&l, r.clone(), self.pal.accent.mix(self.pal.fg, 0.2));
                    // A mark whose note has not streamed in yet is only text.
                    if let Some((_, note)) = look.notes.iter().find(|(n, _)| n == label) {
                        links.push((r, Hit::Note(note.clone())));
                    }
                }
                Style::Image(src) => {
                    let _ = l.SetFontSize(size * 0.9, dr);
                    pad_sides(&l, &wide, r.clone(), IMAGE_ICON, CODE_PAD + 2.0);
                    self.paint(&l, r.clone(), self.link_ink());
                    let act = if horadric_core::markdown::is_web_link(src) {
                        Act::Link(src.clone())
                    } else {
                        Act::Path(src.clone(), None)
                    };
                    images.push(r.clone());
                    links.push((r, Hit::Tr(act)));
                }
            }
        }
        // The "+" between two keys of a shortcut is only a joint.
        for pair in keys.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if b.start == a.end + 1 && wide.get(a.end as usize) == Some(&(b'+' as u16)) {
                self.paint(&l, a.end..b.start, self.pal.faint);
            }
        }
        // Plates sit on the text's own band of the line, not its whole
        // height, so code on two lines running does not merge into a bar.
        let plate = |b: &Rect, inset: f32, h: f32| {
            let cy = b.y + b.h / 2.0 + 0.12 * size;
            Rect::new(x + b.x + inset, y + cy - h / 2.0, b.w - 2.0 * inset, h)
        };
        let code_h = (0.86 * size + 5.0).round();
        for r in code_runs {
            for b in render::range_rects(&l, r.start, r.end - r.start) {
                s.fill(plate(&b, 0.5, code_h), 4.0, self.pal.fg.with_alpha(0.075));
            }
        }
        let key_h = (0.8 * size + 7.0).round();
        for r in keys {
            for b in render::range_rects(&l, r.start, r.end - r.start) {
                let k = plate(&b, KEY_PAD - KEY_INNER, key_h);
                s.fill(k, 4.0, self.pal.fg.with_alpha(0.05));
                s.stroke(k, 4.0, self.pal.fg.with_alpha(0.2), 1.0);
            }
        }
        // An image that cannot be shown here is a chip: a picture icon and
        // its alt text on a plate, opening where it points.
        let chip_h = (0.9 * size + 9.0).round();
        for r in images {
            for (k, b) in render::range_rects(&l, r.start, r.end - r.start).iter().enumerate() {
                let p = plate(b, 0.5, chip_h);
                s.fill(p, 5.0, self.pal.fg.with_alpha(0.06));
                s.stroke(p, 5.0, self.pal.fg.with_alpha(0.12), 1.0);
                if k == 0 {
                    let icon = Rect::new(p.x + 5.0, p.y, IMAGE_ICON - 6.0, p.h);
                    s.line("\u{EB9F}", F::Icon, icon, self.link_ink());
                }
            }
        }
        for (r, hit) in links {
            for b in render::range_rects(&l, r.start, r.end - r.start) {
                s.hit(Rect::new(x + b.x, y + b.y, b.w, b.h), hit.clone());
            }
        }
        // Selection and find read the text as written, not as shown.
        s.unit(&l, x, y, ink, &inl.text);
        let (_, h) = render::text_size(&l);
        (h, first_line(&l).unwrap_or(h))
    }

    /// A reply's image that is a file on this machine, decoded once. A
    /// web address is never fetched, and a path to another machine
    /// (`\\host\share`) never read, as reading it would reach out to it.
    fn local_image(&self, src: &str) -> Option<(ID2D1Bitmap, u32, u32)> {
        let src = src.strip_prefix("file:///").unwrap_or(src);
        let b = src.as_bytes();
        let remote = b.len() >= 2 && matches!(b[0], b'/' | b'\\') && matches!(b[1], b'/' | b'\\');
        if remote || src.contains("://") || (src.contains(':') && !is_drive(src)) {
            return None;
        }
        let ext = src.rsplit_once('.')?.1.to_ascii_lowercase();
        if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp") {
            return None;
        }
        let p = std::path::Path::new(src);
        let p = if p.is_absolute() { p.to_path_buf() } else { self.dir.join(p) };
        let key = format!("file:{}", p.display());
        if let Some(got) = self.thumbs.borrow().get(&key) {
            return got.clone();
        }
        let got = std::fs::metadata(&p)
            .ok()
            .filter(|m| m.is_file() && m.len() <= IMAGE_BYTES)
            .and_then(|_| std::fs::read(&p).ok())
            .and_then(|bytes| decode_bytes(&bytes, IMAGE_PX))
            .and_then(|(px, w, h)| Some((bitmap(&self.gfx.rt, &px, w, h)?, w, h)));
        self.thumbs.borrow_mut().insert(key, got.clone());
        got
    }

    /// The colour a callout of `kind` is drawn in: the theme's colours
    /// for states where one fits, the project's own for what matters.
    fn callout_ink(&self, kind: Callout) -> Color {
        let t = crate::theme::palette();
        let c = match kind {
            Callout::Note => t.working,
            Callout::Tip => t.done,
            Callout::Important => self.pal.accent,
            Callout::Warning => t.waiting,
            Callout::Caution => t.error,
        };
        c.mix(self.pal.fg, 0.15)
    }

    /// One paragraph of a reply, `width` wide from its own indent.
    pub(super) unsafe fn para(&self, p: &Para, width: f32) -> Scene {
        let mut s = Scene::default();
        let x = p.indent;
        let w = (width - x - p.right).max(40.0);
        // The marker's line: its format and the band it is centred in.
        let mut mark_font = &self.fonts.body;
        let mut mark_line = (0.0, 28.0);
        match &p.kind {
            Kind::Text { font, inline, term } => {
                let done = p.marker == Some(Marker::Task(true));
                let ink = match font {
                    Font::Body if done => self.pal.fg.with_alpha(0.5),
                    Font::Body if p.quoted() => self.pal.fg.with_alpha(0.74),
                    Font::Body => self.pal.fg.with_alpha(0.92),
                    Font::H4 => self.pal.fg.with_alpha(0.62),
                    _ => self.pal.fg,
                };
                let f = self.fonts.of(*font);
                let (mut tx, mut ty, mut tw) = (x, 0.0, w);
                let mut term_h = 0.0;
                if let Some(t) = term {
                    let left = x - p.term_w;
                    let look = Look {
                        weight: Some(DWRITE_FONT_WEIGHT_SEMI_BOLD),
                        notes: &p.notes,
                        ..Look::default()
                    };
                    let ink = self.pal.fg.with_alpha(if done { 0.55 } else { 1.0 });
                    if w >= TERM_ROOM {
                        let tw = p.term_w - flow::TERM_GAP + 8.0;
                        (term_h, _) =
                            self.inline_as(&mut s, t, f, (left, 0.0, tw), ink, Align::None, look);
                    } else {
                        // Too narrow for two columns: the term goes above.
                        let (th, _) =
                            self.inline_as(&mut s, t, f, (left, 0.0, w + p.term_w), ink, Align::None, look);
                        (tx, ty, tw) = (left, th, w + p.term_w);
                    }
                }
                let look = Look {
                    caps: *font == Font::H4,
                    track: if *font == Font::H4 { 0.9 } else { 0.0 },
                    notes: &p.notes,
                    ..Look::default()
                };
                let (h, first) = self.inline_as(&mut s, inline, f, (tx, ty, tw), ink, Align::None, look);
                if let Some((key, shut)) = &p.fold {
                    self.fold_mark(&mut s, key, *shut, (tx, ty, first));
                }
                mark_font = f;
                mark_line = (0.0, if term.is_some() && ty > 0.0 { ty } else { first });
                // A term that wraps beside a short text must not run into
                // the next item.
                s.h = flow::hang_height(ty, h, term_h);
                if *font == Font::H1 {
                    s.fill(
                        Rect::new(x, s.h + 6.0, w, 1.0),
                        0.0,
                        self.pal.fg.with_alpha(0.07),
                    );
                    s.h += 7.0;
                }
            }
            Kind::Callout { kind, title } => {
                let c = self.callout_ink(*kind);
                let y = flow::CARD_PAD;
                let lh = 22.0;
                s.line(callout_icon(*kind), F::Icon, Rect::new(x, y, 14.0, lh), c);
                let label = if title.text.trim().is_empty() {
                    kind.label()
                } else {
                    title.text.as_str()
                };
                s.line(label, F::Title, Rect::new(x + 21.0, y, w - 21.0, lh), c);
                s.h = y + lh;
            }
            Kind::Code { info, text } => {
                s = self.code(info, text, w);
                s = moved(s, x);
            }
            Kind::Details { summary, key, open } => {
                let plain;
                let summary = if summary.text.trim().is_empty() {
                    plain = Inline {
                        text: "Details".into(),
                        spans: Vec::new(),
                    };
                    &plain
                } else {
                    summary
                };
                let look = Look {
                    weight: Some(DWRITE_FONT_WEIGHT_SEMI_BOLD),
                    ..Look::default()
                };
                let ink = self.pal.fg.with_alpha(0.92);
                let f = &self.fonts.body;
                let (h, first) =
                    self.inline_as(&mut s, summary, f, (x + 22.0, 0.0, w - 22.0), ink, Align::None, look);
                let chevron = if *open { "\u{E70D}" } else { "\u{E76C}" };
                s.line(chevron, F::Icon, Rect::new(x, 0.0, 14.0, first), self.pal.faint);
                s.h = h;
                s.hit(Rect::new(x, 0.0, w, h), Hit::Tr(Act::All(key.clone())));
            }
            Kind::Footnote { label, text, first } => {
                let top = if *first {
                    let rule = Rect::new(x, 0.0, NOTE_RULE.min(w), 1.0);
                    s.fill(rule, 0.0, self.pal.fg.with_alpha(0.12));
                    12.0
                } else {
                    0.0
                };
                let f = &self.fonts.small_wrap;
                let ink = self.pal.dim;
                // The number, from the parser; a figure more than two
                // gets the room of one more.
                let col = NOTE_COL + label.chars().count().saturating_sub(2) as f32 * 7.0;
                let (h, line) =
                    self.inline(&mut s, text, f, x + col, top, w - col, ink, Align::None);
                let n = Rect::new(x, top, col - 7.0, line);
                s.line(format!("{label}."), F::Right, n, self.pal.faint);
                s.h = top + h;
            }
            Kind::Image { alt, src } => match self.local_image(src) {
                Some((bmp, pw, ph)) => {
                    let k = (0.5f32).min(w / pw as f32);
                    let r = Rect::new(x, 2.0, pw as f32 * k, ph as f32 * k);
                    s.items.push(Item::Image(bmp, r));
                    s.stroke(r, 0.0, self.pal.fg.with_alpha(0.1), 1.0);
                    s.hit(r, Hit::Tr(Act::Path(src.clone(), None)));
                    s.h = r.h + 4.0;
                    if alt.text != "image" && !alt.text.trim().is_empty() {
                        let cap = Rect::new(x, s.h, w, CAPTION);
                        s.line(alt.text.clone(), F::Small, cap, self.pal.dim);
                        s.h += CAPTION;
                    }
                }
                None => {
                    let ink = self.pal.fg.with_alpha(0.92);
                    let (h, _) = self.inline(&mut s, alt, &self.fonts.body, x, 0.0, w, ink, Align::None);
                    s.h = h;
                }
            },
            Kind::Table {
                header,
                align,
                rows,
            } => {
                s = self.table(header, align, rows, w);
                s = moved(s, x);
            }
            Kind::Rule => {
                s.fill(Rect::new(x, 14.0, w, 1.0), 0.0, self.pal.fg.with_alpha(0.1));
                s.h = 28.0;
            }
        }
        if let Some(m) = &p.marker {
            self.marker(&mut s, m, p, mark_font, mark_line);
        }
        let ends = (p.bars.iter())
            .filter(|b| b.end && b.callout.is_some())
            .count();
        s.h += ends as f32 * flow::CARD_PAD;
        let under = self.bars(p, width, s.h);
        s.items.splice(0..0, under);
        s
    }

    /// A heading that folds its section: a click on its words toggles
    /// it. The chevron in the margin shows on hover while it is open, so
    /// a reply with many headings is not lined with them, and always once
    /// it is folded, so a hidden section is never lost.
    fn fold_mark(&self, s: &mut Scene, key: &str, shut: bool, (x, y, line): (f32, f32, f32)) {
        let text_w = (s.units.last()).map_or(0.0, |u| render::text_size(&u.layout).0);
        let hit = Hit::Tr(Act::All(key.to_string()));
        s.hit(Rect::new(x, y, text_w.max(8.0), line), hit.clone());
        let mark = Rect::new(x - FOLD_MARGIN, y, FOLD_MARGIN - 4.0, line);
        if shut {
            s.line("\u{E76C}", F::Icon, mark, self.pal.faint);
            s.hit(mark, hit);
        } else {
            let ink = self.pal.faint.with_alpha(0.6);
            (s.hover).push(Item::Line("\u{E70D}".into(), F::Icon, mark, ink));
            s.hover_hits.push((mark, hit));
        }
    }

    /// A list item's bullet, number or box, in the column left of its
    /// text (and of its term, when it has one), on the line `band` gives
    /// as top and height.
    unsafe fn marker(
        &self,
        s: &mut Scene,
        m: &Marker,
        p: &Para,
        f: &IDWriteTextFormat,
        (top, line): (f32, f32),
    ) {
        let right = p.indent - p.term_w - MARKER_GAP;
        let left = p.indent - p.term_w - p.marker_w;
        let size = f.GetFontSize();
        let (glyph, ink) = match m {
            Marker::Task(done) => {
                let cy = top + line / 2.0 + 0.1 * size;
                let r = Rect::new(right - BOX, cy - BOX / 2.0, BOX, BOX);
                if *done {
                    s.fill(r, 4.0, self.pal.accent.mix(self.pal.fg, 0.25).with_alpha(0.85));
                    s.line("\u{E73E}", F::Icon, r, self.pal.bg);
                } else {
                    s.fill(r, 4.0, self.pal.fg.with_alpha(0.03));
                    s.stroke(r, 4.0, self.pal.fg.with_alpha(0.35), 1.25);
                }
                return;
            }
            Marker::Bullet(c) => (c.to_string(), self.pal.accent.with_alpha(0.7)),
            Marker::Number(n) => (n.clone(), self.pal.accent.mix(self.pal.fg, 0.35).with_alpha(0.85)),
        };
        let Some(l) = self.layout(f, &glyph, (right - left).max(1.0)) else {
            return;
        };
        let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        let _ = l.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
        if let (Marker::Number(_), Some(t)) = (m, &self.fonts.tnum) {
            let _ = l.SetTypography(t, range(0..glyph.encode_utf16().count() as u32));
        }
        // In the text's own format its baseline is the first line's; a
        // block that is not text centres it on its header instead.
        let own = first_line(&l).unwrap_or(line);
        let y = if matches!(p.kind, Kind::Text { .. }) {
            top
        } else {
            top + (line - own) / 2.0
        };
        s.text(&l, left, y, ink);
    }

    /// What is drawn under a paragraph for the quotes and callouts it is
    /// inside, `h` tall: a quote's bar, or its slice of a callout's card.
    /// A card is one rounded plate across many paragraphs, so each draws
    /// the whole card clipped to its own band, and the corners show only
    /// where the card really starts and ends.
    fn bars(&self, p: &Para, width: f32, h: f32) -> Vec<Item> {
        let mut out = Vec::new();
        for (q, bar) in p.bars.iter().enumerate() {
            let Some(kind) = bar.callout else {
                let r = Rect::new(bar.x + 2.0, -p.gap, 3.0, h + p.gap);
                out.push(Item::Fill(r, 1.5, self.pal.accent.with_alpha(0.4)));
                continue;
            };
            let first = q + 1 == p.bars.len() && matches!(p.kind, Kind::Callout { .. });
            let top = if first { 0.0 } else { -p.gap };
            let card_top = if first { 0.0 } else { top - 2.0 * CARD_R };
            let card_bottom = if bar.end { h } else { h + 2.0 * CARD_R };
            let cw = (width - bar.right - bar.x).max(1.0);
            let card = Rect::new(bar.x, card_top, cw, card_bottom - card_top);
            let c = self.callout_ink(kind);
            out.push(Item::Clip(Rect::new(bar.x, top, cw, h - top)));
            out.push(Item::Fill(card, CARD_R, c.with_alpha(0.07)));
            out.push(Item::Clip(Rect::new(bar.x, top, 3.0, h - top)));
            out.push(Item::Fill(card, CARD_R, c.with_alpha(0.8)));
            out.push(Item::Unclip);
            out.push(Item::Unclip);
        }
        out
    }
}

/// How a run of text is shown beyond its own styles.
#[derive(Default, Clone, Copy)]
struct Look<'a> {
    /// In capitals, though selected and copied as written.
    caps: bool,
    /// Extra room after each character.
    track: f32,
    /// A weight for the whole run.
    weight: Option<DWRITE_FONT_WEIGHT>,
    /// The footnotes the run's marks may name, by label, for a mark to
    /// show its note on hover.
    notes: &'a [(String, String)],
}

/// Room each side of inline code, inside its plate.
const CODE_PAD: f32 = 3.5;
/// Room each side of a key, and how much of it is inside the key's cap;
/// the rest keeps two caps and the "+" between them apart.
const KEY_PAD: f32 = 6.0;
const KEY_INNER: f32 = 4.5;
/// A task's box, and the room between a marker and its text.
const BOX: f32 = 15.0;
const MARKER_GAP: f32 = 9.0;
/// A callout card's corner.
const CARD_R: f32 = 6.0;
/// Below this, a hanging term goes above its text rather than beside it.
const TERM_ROOM: f32 = 220.0;
/// Room before an image chip's text for its icon.
const IMAGE_ICON: f32 = 24.0;
/// A reply's image: decoded to this many pixels on its longest side, and
/// drawn at half that, so it stays sharp at 200%.
const IMAGE_PX: u32 = 760;
/// The largest image file a reply may show, so a stray path to a huge
/// one does not stall the frame that reads it.
const IMAGE_BYTES: u64 = 40 << 20;
/// A footnote's number column, and its rule's width.
const NOTE_COL: f32 = 24.0;
const NOTE_RULE: f32 = 140.0;
/// An image's alt text under it, when it has one of its own.
const CAPTION: f32 = 20.0;

/// How far left of a heading its fold chevron sits, in the column's margin.
const FOLD_MARGIN: f32 = 20.0;

/// `s` in capitals, a character kept as it is where its capital would
/// change how many UTF-16 units it takes, so the spans still fit.
fn capitals(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) if u.len_utf16() == c.len_utf16() => u,
                _ => c,
            }
        })
        .collect()
}

/// Room before the first character of `r` and after its last, which
/// DirectWrite counts in the run's own width, so a plate drawn to the
/// run's box clears the text around it.
fn pad(l: &IDWriteTextLayout, wide: &[u16], r: std::ops::Range<u32>, room: f32) {
    pad_sides(l, wide, r, room, room);
}

/// [`pad`] with its own room before and after.
fn pad_sides(l: &IDWriteTextLayout, wide: &[u16], r: std::ops::Range<u32>, before: f32, after: f32) {
    if r.is_empty() {
        return;
    }
    let Ok(l1) = l.cast::<IDWriteTextLayout1>() else {
        return;
    };
    let surrogate = |u: u16| (0xD800..0xE000).contains(&u);
    let first_end = if surrogate(wide[r.start as usize]) { r.start + 2 } else { r.start + 1 };
    let last_start = if surrogate(wide[r.end as usize - 1]) { r.end - 2 } else { r.end - 1 };
    unsafe {
        if last_start <= r.start {
            let _ = l1.SetCharacterSpacing(before, after, 0.0, range(r));
        } else {
            let _ = l1.SetCharacterSpacing(before, 0.0, 0.0, range(r.start..first_end.min(r.end)));
            let _ = l1.SetCharacterSpacing(0.0, after, 0.0, range(last_start..r.end));
        }
    }
}

/// A callout's icon, from Segoe Fluent Icons.
fn callout_icon(kind: Callout) -> &'static str {
    match kind {
        Callout::Note => "\u{E946}",
        Callout::Tip => "\u{EA80}",
        Callout::Important => "\u{E8C9}",
        Callout::Warning => "\u{E7BA}",
        Callout::Caution => "\u{E783}",
    }
}

/// `C:\` or `C:/` at the start, the one colon a local path may have.
fn is_drive(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'/' | b'\\')
        && !s[2..].contains(':')
}

fn moved(s: Scene, dx: f32) -> Scene {
    let mut out = Scene {
        h: s.h,
        ..Scene::default()
    };
    out.append(&s, dx, 0.0);
    out
}

/// The height of a layout's first line.
pub(super) fn first_line(l: &IDWriteTextLayout) -> Option<f32> {
    let mut n = 0u32;
    unsafe {
        let _ = l.GetLineMetrics(None, &mut n);
        let mut lines = vec![DWRITE_LINE_METRICS::default(); n.max(1) as usize];
        l.GetLineMetrics(Some(&mut lines), &mut n).ok()?;
        lines.first().map(|m| m.height)
    }
}

/// Colours a code layout's text with the file viewer's grammars.
pub(super) fn colour_code(cx: &Ctx, l: &IDWriteTextLayout, lang: &str, text: &str) {
    let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let styles = crate::highlight::highlight_code(lang, &lines);
    if styles.is_empty() {
        return;
    }
    let mut at = 0u32;
    for (line, spans) in lines.iter().zip(&styles) {
        let mut byte = 0;
        for sp in spans {
            let piece = &line[byte..(byte + sp.len).min(line.len())];
            let n = piece.encode_utf16().count() as u32;
            let [r, g, b] = sp.style.fg;
            let c = Color::rgb((r as u32) << 16 | (g as u32) << 8 | b as u32);
            cx.paint(l, at..at + n, c);
            if sp.style.bold {
                unsafe {
                    let _ = l.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, range(at..at + n));
                }
            }
            at += n;
            byte += sp.len;
        }
        // What the spans left of the line, and its newline.
        at += line[byte.min(line.len())..].encode_utf16().count() as u32 + 1;
    }
}

/// Draws a scene with its origin at (`ox`, `oy`), and its hover items too
/// when `hovered`.
pub(super) unsafe fn draw(gfx: &Gfx, fonts: &Fonts, s: &Scene, ox: f32, oy: f32, hovered: bool) {
    let items = s.items.iter().chain(s.hover.iter().filter(|_| hovered));
    for item in items {
        match item {
            Item::Fill(r, k, c) => {
                let r = shift(r, ox, oy);
                if *k > 0.0 {
                    gfx.fill_round(&r, *k, *c);
                } else {
                    gfx.fill(&r, *c);
                }
            }
            Item::Stroke(r, k, c, w) => gfx.stroke_round(&shift(r, ox, oy), *k, *c, *w),
            Item::Text(l, x, y, c) => gfx.layout_at(l, ox + x, oy + y, *c),
            Item::Line(t, f, r, c) => {
                let fmt = match f {
                    F::Line => &gfx.line,
                    F::Small => &gfx.small,
                    F::Title => &gfx.title,
                    F::Button => &gfx.button,
                    F::Icon => &gfx.icon,
                    F::Right => &fonts.right,
                    F::Centre => &fonts.centre,
                };
                gfx.text_in(t, fmt, &shift(r, ox, oy), *c);
            }
            Item::Image(b, r) => {
                let r = shift(r, ox, oy);
                gfx.rt.DrawBitmap(
                    b,
                    Some(&d2d(&r)),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
            Item::Rune(n, x, y, h, c, w) => gfx.rune(*n, ox + x, oy + y, *h, *c, *w),
            Item::Clip(r) => gfx
                .rt
                .PushAxisAlignedClip(&d2d(&shift(r, ox, oy)), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE),
            Item::Unclip => gfx.rt.PopAxisAlignedClip(),
        }
    }
}

/// The longest side a prompt's thumbnail is decoded to, in pixels: twice
/// what it is drawn at, so it stays sharp at 200%.
const THUMB_PX: u32 = 360;

/// A prompt's image, base64 PNG, JPEG, GIF or WebP, decoded by WIC and
/// made a bitmap of the frame's, with its size in pixels once scaled.
pub(super) fn thumbnail(gfx: &Gfx, base64: &str) -> Option<(ID2D1Bitmap, u32, u32)> {
    let (pixels, w, h) = decode(base64, THUMB_PX)?;
    Some((bitmap(&gfx.rt, &pixels, w, h)?, w, h))
}

/// An image's premultiplied BGRA pixels and size, scaled down so its
/// longest side is at most `max` pixels.
pub(super) fn decode(base64: &str, max: u32) -> Option<(Vec<u8>, u32, u32)> {
    decode_bytes(&horadric_core::release::base64_decode(base64)?, max)
}

/// [`decode`] from the image file's own bytes.
pub(super) fn decode_bytes(bytes: &[u8], max: u32) -> Option<(Vec<u8>, u32, u32)> {
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = factory.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 {
            return None;
        }
        let k = (max as f32 / w.max(h) as f32).min(1.0);
        let (tw, th) = (
            ((w as f32 * k).round() as u32).max(1),
            ((h as f32 * k).round() as u32).max(1),
        );
        let scaler = factory.CreateBitmapScaler().ok()?;
        scaler
            .Initialize(&frame, tw, th, WICBitmapInterpolationModeFant)
            .ok()?;
        let converter = factory.CreateFormatConverter().ok()?;
        converter
            .Initialize(
                &scaler,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .ok()?;
        let mut pixels = vec![0u8; (tw * th * 4) as usize];
        converter
            .CopyPixels(std::ptr::null(), tw * 4, &mut pixels)
            .ok()?;
        Some((pixels, tw, th))
    }
}

/// Premultiplied BGRA pixels made a bitmap of target `rt`'s.
pub(super) fn bitmap(rt: &ID2D1RenderTarget, pixels: &[u8], w: u32, h: u32) -> Option<ID2D1Bitmap> {
    unsafe {
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let size = D2D_SIZE_U {
            width: w,
            height: h,
        };
        rt.CreateBitmap(
            size,
            Some(pixels.as_ptr() as *const std::ffi::c_void),
            w * 4,
            &props,
        )
        .ok()
    }
}
