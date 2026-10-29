//! The composer at the bottom of a Folio: the text being written and the
//! button that sends it or stops Claude.

use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE;

use super::view::{d2d, Gfx, Hit, Palette, View};
use super::{with, Chat};
use crate::field;
use crate::folio_look as look;
use crate::layout::Rect;
use crate::render;

const COMPOSER_PAD: f32 = 12.0;
const COMPOSER_BOTTOM: f32 = 16.0;
const COMPOSER_LINES: f32 = 8.0;
const SEND: f32 = 28.0;
impl View {
    /// The composer at the bottom: the text being written, its caret and
    /// selection, and the button that sends it, or stops Claude while it
    /// works. Returns its box.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn composer(
        &self,
        gfx: &Gfx,
        chat: &mut Chat,
        pal: &Palette,
        cx: f32,
        cw: f32,
        h: f32,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> Rect {
        let gpu = &self.shared.gpu;
        let inner_w = (cw - 2.0 * COMPOSER_PAD - SEND - 8.0).max(10.0);
        let f = &chat.composer;
        let empty = f.text.is_empty();
        let shown = if empty { "M" } else { f.text.as_str() };
        let Ok(l) = render::wrapped(gpu, &gfx.text, shown, inner_w) else {
            return Rect::new(cx, h, cw, 0.0);
        };
        let (_, th) = render::text_size(&l);
        let line_h = render::wrapped(gpu, &gfx.text, "M", inner_w)
            .map(|l| render::text_size(&l).1)
            .unwrap_or(20.0);
        let view_h = th.clamp(line_h, line_h * COMPOSER_LINES);
        let bh = view_h + 2.0 * COMPOSER_PAD;
        let r = Rect::new(cx, h - COMPOSER_BOTTOM - bh, cw, bh);
        let focused = self.has_keyboard();
        gfx.fill_round(&r, 16.0, pal.bg.mix(pal.fg, 0.07).with_alpha(0.94));
        let edge = if focused {
            pal.accent.with_alpha(0.6)
        } else {
            pal.fg.with_alpha(0.14)
        };
        gfx.stroke_round(&r, 16.0, edge, 1.0);
        let (ox, oy) = (r.x + COMPOSER_PAD, r.y + COMPOSER_PAD);
        let text_box = Rect::new(ox, oy, inner_w, view_h);
        if empty {
            let hint = if chat.folio.busy {
                "Claude is working. Esc stops it."
            } else {
                "Message Claude. Enter sends, Shift+Enter breaks the line."
            };
            gfx.text_in(
                hint,
                &gfx.line,
                // Clear of the caret standing at its start.
                &Rect::new(ox + 4.0, oy, inner_w - 4.0, line_h),
                pal.faint,
            );
            chat.composer_scroll = 0.0;
        } else {
            let caret = render::caret_at(&l, field::utf16_at(&f.text, f.caret));
            chat.composer_scroll =
                field::follow(chat.composer_scroll, caret.y, caret.h, view_h, th);
        }
        let sy = oy - chat.composer_scroll;
        gfx.rt
            .PushAxisAlignedClip(&d2d(&text_box), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        if !empty {
            let (a, b) = f.selection();
            let (a16, b16) = (field::utf16_at(&f.text, a), field::utf16_at(&f.text, b));
            for s in render::range_rects(&l, a16, b16 - a16) {
                gfx.fill(
                    &Rect::new(ox + s.x, sy + s.y, s.w, s.h),
                    pal.accent.with_alpha(0.35),
                );
            }
            gfx.layout_at(&l, ox, sy, pal.fg);
        }
        if focused {
            let c = render::caret_at(
                &l,
                if empty {
                    0
                } else {
                    field::utf16_at(&f.text, f.caret)
                },
            );
            gfx.fill(
                &Rect::new(ox + c.x, sy + c.y, 1.5, c.h),
                pal.accent.mix(pal.fg, 0.4),
            );
        }
        gfx.rt.PopAxisAlignedClip();
        *self.composer.borrow_mut() = (!empty).then_some((l, ox, sy));
        hits.push((r, Hit::Composer));

        let send = Rect::new(
            r.right() - COMPOSER_PAD - SEND,
            r.bottom() - COMPOSER_PAD / 2.0 - SEND - 2.0,
            SEND,
            SEND,
        );
        let (glyph, fill, hit) = if chat.folio.busy {
            ('\u{E71A}', pal.fg.with_alpha(0.85), Hit::Stop)
        } else if look::prompt_of(&f.text).is_some() && !chat.ended {
            ('\u{E724}', pal.accent, Hit::Send)
        } else {
            ('\u{E724}', pal.fg.with_alpha(0.12), Hit::Send)
        };
        gfx.fill_round(&send, SEND / 2.0, fill);
        gfx.text_in(&glyph.to_string(), &gfx.icon, &send, pal.bg);
        hits.push((send, hit));
        r
    }

    /// Moves the composer's caret a line up or down. None when it is on
    /// the first or the last line already.
    pub(super) fn caret_line(&self, key: &str, up: bool, extend: bool) -> Option<()> {
        let held = self.composer.borrow();
        let (l, _, _) = held.as_ref()?;
        with(key, |c| {
            let f = &mut c.composer;
            let caret = render::caret_at(l, field::utf16_at(&f.text, f.caret));
            let y = if up {
                caret.y - caret.h / 2.0
            } else {
                caret.y + caret.h * 1.5
            };
            let (_, th) = render::text_size(l);
            if y < 0.0 || y > th {
                return None;
            }
            let at = render::offset_at(l, caret.x, y);
            f.set_caret(field::byte_at(&f.text, at), extend);
            Some(())
        })?
    }

    /// The composer's text under a point in the view, as a byte offset.
    pub(super) fn composer_at(&self, x: f32, y: f32) -> Option<usize> {
        let held = self.composer.borrow();
        let (l, ox, oy) = held.as_ref()?;
        let at = render::offset_at(l, x - ox, y - oy);
        with(&self.key, |c| field::byte_at(&c.composer.text, at))
    }
}
