//! A fenced code block in a reply.

use super::rich::{colour_code, Ctx, Scene, F};
use super::transcript::Act;
use super::view::Hit;
use crate::layout::Rect;
use crate::render;
use crate::theme::Color;

/// A code block's padding and its header with the language and Copy.
const CODE_PAD: f32 = 12.0;
const CODE_HEAD: f32 = 28.0;

impl Ctx<'_> {
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
}
