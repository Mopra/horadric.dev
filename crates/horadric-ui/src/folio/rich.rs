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

use horadric_core::markdown::{Align, Inline, Style};
use windows::core::{Interface, Result, PCWSTR};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextFormat, IDWriteTextLayout, DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_WEIGHT_BOLD,
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

use super::flow::{self, Font, Kind, Para};
use super::transcript::Act;
use super::view::{d2d, reading, text_format, Gfx, Hit, Palette, FONT, READ};
use crate::layout::Rect;
use crate::render::{self, color, Gpu};
use crate::theme::Color;

const MONO: PCWSTR = windows::core::w!("Cascadia Mono");

/// A code block's padding and its header with the language and Copy.
const CODE_PAD: f32 = 12.0;
const CODE_HEAD: f32 = 28.0;
/// A table cell's padding.
const CELL_X: f32 = 10.0;
const CELL_Y: f32 = 6.0;
/// The narrowest a table column gets.
const CELL_LEAST: f32 = 64.0;
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
    /// A code block's text: it breaks long lines rather than hiding them.
    pub(super) code: IDWriteTextFormat,
    /// Diffs and shell output: one line a row, never wrapped.
    pub(super) rows: IDWriteTextFormat,
    pub(super) right: IDWriteTextFormat,
    pub(super) centre: IDWriteTextFormat,
    pub(super) small_wrap: IDWriteTextFormat,
}

impl Fonts {
    pub(super) unsafe fn new(gpu: &Gpu) -> Result<Fonts> {
        let n = DWRITE_FONT_WEIGHT_NORMAL;
        let semi = DWRITE_FONT_WEIGHT_SEMI_BOLD;
        let code = text_format(gpu, MONO, 13.5, n, false, false)?;
        code.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK)?;
        let rows = text_format(gpu, MONO, 13.0, n, false, false)?;
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
        Ok(Fonts {
            body: reading(gpu, READ, n)?,
            h1: reading(gpu, 23.0, semi)?,
            h2: reading(gpu, 19.5, semi)?,
            h3: reading(gpu, 17.0, semi)?,
            code,
            rows,
            right,
            centre,
            small_wrap: text_format(gpu, FONT, 13.5, n, false, false)?,
        })
    }

    fn of(&self, f: Font) -> &IDWriteTextFormat {
        match f {
            Font::Body => &self.body,
            Font::H1 => &self.h1,
            Font::H2 => &self.h2,
            Font::H3 => &self.h3,
        }
    }
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
}

fn range(r: std::ops::Range<u32>) -> DWRITE_TEXT_RANGE {
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
        let Some(l) = self.layout(f, &inl.text, width) else {
            return (0.0, 0.0);
        };
        let a = match align {
            Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            _ => DWRITE_TEXT_ALIGNMENT_LEADING,
        };
        let _ = l.SetTextAlignment(a);
        let size = f.GetFontSize();
        let mut code_runs = Vec::new();
        let mut links = Vec::new();
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
                    let _ = l.SetFontFamilyName(MONO, dr);
                    let _ = l.SetFontSize(size * 0.88, dr);
                    self.paint(&l, r.clone(), self.code_ink());
                    code_runs.push(r);
                }
                Style::Link(url) => {
                    let _ = l.SetUnderline(true, dr);
                    self.paint(&l, r.clone(), self.link_ink());
                    links.push((r, Act::Link(url.clone())));
                }
                Style::Path { path, line } => {
                    self.paint(&l, r.clone(), self.link_ink());
                    links.push((r, Act::Path(path.clone(), *line)));
                }
            }
        }
        // Inline code sits on a faint plate, drawn before the text.
        for r in code_runs {
            for b in render::range_rects(&l, r.start, r.end - r.start) {
                s.fill(
                    Rect::new(x + b.x - 2.0, y + b.y + 1.0, b.w + 4.0, b.h - 2.0),
                    3.0,
                    self.pal.fg.with_alpha(0.08),
                );
            }
        }
        for (r, act) in links {
            for b in render::range_rects(&l, r.start, r.end - r.start) {
                s.hit(Rect::new(x + b.x, y + b.y, b.w, b.h), Hit::Tr(act.clone()));
            }
        }
        s.unit(&l, x, y, ink, &inl.text);
        let (_, h) = render::text_size(&l);
        (h, first_line(&l).unwrap_or(h))
    }

    /// One paragraph of a reply, `width` wide from its own indent.
    pub(super) unsafe fn para(&self, p: &Para, width: f32) -> Scene {
        let mut s = Scene::default();
        let x = p.indent;
        let w = (width - x).max(40.0);
        match &p.kind {
            Kind::Text {
                font,
                inline,
                marker,
            } => {
                let ink = match font {
                    Font::Body => self.pal.fg.with_alpha(0.92),
                    _ => self.pal.fg,
                };
                let (h, first) = self.inline(
                    &mut s,
                    inline,
                    self.fonts.of(*font),
                    x,
                    0.0,
                    w,
                    ink,
                    Align::None,
                );
                if let Some(m) = marker {
                    let r = Rect::new(x - p.marker_w, 0.0, p.marker_w - 7.0, first);
                    s.line(m.clone(), F::Right, r, self.pal.dim);
                }
                if *font == Font::H1 || *font == Font::H2 {
                    s.fill(
                        Rect::new(x, h + 5.0, w, 1.0),
                        0.0,
                        self.pal.fg.with_alpha(0.08),
                    );
                    s.h = h + 8.0;
                } else {
                    s.h = h;
                }
            }
            Kind::Code { lang, text } => {
                s = self.code(lang, text, w);
                s = moved(s, x);
            }
            Kind::Table {
                header,
                align,
                rows,
            } => {
                s = self.table(header, align, rows, w);
                s = moved(s, x);
            }
            Kind::Rule => {
                s.fill(Rect::new(x, 8.0, w, 1.0), 0.0, self.pal.fg.with_alpha(0.14));
                s.h = 17.0;
            }
        }
        // A quote's bars run the paragraph's height and the gap above it.
        for q in 0..p.quotes {
            let bx = x - flow::QUOTE_INDENT * (p.quotes - q) as f32 + 2.0;
            let r = Rect::new(bx, -p.gap, 3.0, s.h + p.gap);
            s.items
                .insert(0, Item::Fill(r, 1.5, self.pal.accent.with_alpha(0.45)));
        }
        s
    }

    /// A fenced code block: the language and a copy button on a header,
    /// the code coloured as the file viewer colours it.
    pub(super) unsafe fn code(&self, lang: &str, text: &str, w: f32) -> Scene {
        let mut s = Scene::default();
        let inner = w - 2.0 * CODE_PAD;
        let Some(l) = self.layout(&self.fonts.code, text, inner) else {
            return s;
        };
        colour_code(self, &l, lang, text);
        let (_, th) = render::text_size(&l);
        let h = CODE_HEAD + th + CODE_PAD;
        let plate = Rect::new(0.0, 0.0, w, h);
        s.fill(plate, 8.0, Color::rgb(0).with_alpha(0.32));
        s.stroke(plate, 8.0, self.pal.fg.with_alpha(0.07), 1.0);
        s.fill(
            Rect::new(0.0, CODE_HEAD - 1.0, w, 1.0),
            0.0,
            self.pal.fg.with_alpha(0.06),
        );
        let label = if lang.is_empty() { "code" } else { lang };
        s.line(
            label,
            F::Small,
            Rect::new(CODE_PAD, 0.0, w / 2.0, CODE_HEAD),
            self.pal.faint,
        );
        let copy = Rect::new(w - 70.0, 3.0, 64.0, CODE_HEAD - 6.0);
        s.line(
            "\u{E8C8}",
            F::Icon,
            Rect::new(copy.x + 4.0, copy.y, 16.0, copy.h),
            self.pal.dim,
        );
        s.line(
            "Copy",
            F::Small,
            Rect::new(copy.x + 24.0, copy.y, 40.0, copy.h),
            self.pal.dim,
        );
        s.hit(copy, Hit::Tr(Act::Copy(text.to_string())));
        s.unit(
            &l,
            CODE_PAD,
            CODE_HEAD + 6.0,
            self.pal.fg.with_alpha(0.9),
            text,
        );
        s.h = h + 6.0;
        s
    }

    /// A table: columns as wide as their widest cell, the wide ones
    /// wrapping when the table does not fit.
    unsafe fn table(
        &self,
        header: &[Inline],
        align: &[Align],
        rows: &[Vec<Inline>],
        w: f32,
    ) -> Scene {
        let mut s = Scene::default();
        let cols = header.len();
        if cols == 0 {
            return s;
        }
        let all = || std::iter::once(header).chain(rows.iter().map(Vec::as_slice));
        let mut natural = vec![0.0f32; cols];
        for row in all() {
            for (c, cell) in row.iter().enumerate().take(cols) {
                if let Some(l) = self.one_line(&self.fonts.body, &cell.text) {
                    let bold = row.as_ptr() == header.as_ptr();
                    if bold {
                        let _ =
                            l.SetFontWeight(DWRITE_FONT_WEIGHT_SEMI_BOLD, range(0..u32::MAX / 2));
                    }
                    natural[c] = natural[c].max(render::text_size(&l).0 + 2.0 * CELL_X + 2.0);
                }
            }
        }
        let widths = flow::table_widths(&natural, CELL_LEAST, w);
        let mut y = 0.0;
        let total_w: f32 = widths.iter().sum();
        for (k, row) in all().enumerate() {
            let head = k == 0;
            let mut cell_scene = Scene::default();
            let mut rh: f32 = 0.0;
            let mut x = 0.0;
            for (c, cell) in row.iter().enumerate().take(cols) {
                let cw = widths[c];
                let ink = if head {
                    self.pal.fg
                } else {
                    self.pal.fg.with_alpha(0.88)
                };
                let a = align.get(c).copied().unwrap_or(Align::None);
                let mark = cell_scene.units.len();
                let (h, _) = self.inline(
                    &mut cell_scene,
                    cell,
                    &self.fonts.body,
                    x + CELL_X,
                    y + CELL_Y,
                    cw - 2.0 * CELL_X,
                    ink,
                    a,
                );
                if head {
                    if let Some(u) = cell_scene.units.get(mark) {
                        let _ = u.layout.SetFontWeight(
                            DWRITE_FONT_WEIGHT_SEMI_BOLD,
                            range(0..u.text.encode_utf16().count() as u32),
                        );
                    }
                }
                rh = rh.max(h + 2.0 * CELL_Y);
                x += cw;
            }
            if head {
                s.fill(
                    Rect::new(0.0, y, total_w, rh),
                    0.0,
                    self.pal.fg.with_alpha(0.06),
                );
            } else if k % 2 == 0 {
                s.fill(
                    Rect::new(0.0, y, total_w, rh),
                    0.0,
                    self.pal.fg.with_alpha(0.025),
                );
            }
            s.append(&cell_scene, 0.0, 0.0);
            y += rh;
            s.fill(
                Rect::new(0.0, y - 1.0, total_w, 1.0),
                0.0,
                self.pal.fg.with_alpha(0.09),
            );
        }
        s.stroke(
            Rect::new(0.0, 0.0, total_w, y),
            6.0,
            self.pal.fg.with_alpha(0.14),
            1.0,
        );
        s.h = y + 4.0;
        s
    }
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
    let bytes = horadric_core::release::base64_decode(base64)?;
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = factory.CreateStream().ok()?;
        stream.InitializeFromMemory(&bytes).ok()?;
        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 {
            return None;
        }
        let k = (THUMB_PX as f32 / w.max(h) as f32).min(1.0);
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
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let size = D2D_SIZE_U {
            width: tw,
            height: th,
        };
        let bitmap = gfx
            .rt
            .CreateBitmap(
                size,
                Some(pixels.as_ptr() as *const std::ffi::c_void),
                tw * 4,
                &props,
            )
            .ok()?;
        Some((bitmap, tw, th))
    }
}
