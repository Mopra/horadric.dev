//! An edit's diff in the transcript.


use windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_TRAILING;

use super::flow::self;
use super::rich::{self, Ctx, Scene, F, MONO_LINE};
use super::view::Hit;
use crate::layout::Rect;
use crate::theme::{self, Color};
use super::transcript::*;

impl Ctx<'_> {
    /// An edit's file, as a link that opens it, and its diff inline. `id`
    /// is what "Show all" is kept under: the step's, or the ask's. With
    /// `review`, an edit ask open to comments: each row's numbers take a
    /// click, and the rows commented on are marked.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn diff(
        &self,
        s: &mut Scene,
        id: &str,
        e: &horadric_core::folio::FileEdit,
        x: f32,
        y: f32,
        w: f32,
        st: &State,
        review: Option<&super::comments::Review>,
    ) -> f32 {
        let head = Rect::new(x, y, w, 22.0);
        let what = if e.created { "Created" } else { "Changed" };
        s.line(what, F::Small, Rect::new(x, y, 60.0, 22.0), self.pal.faint);
        let file = Rect::new(x + 60.0, y, (w - 60.0).min(500.0), 22.0);
        let shown = flow::project_path(self.dir, &e.path).unwrap_or_else(|| e.path.clone());
        s.line(shown, F::Small, file, self.link_ink());
        s.hit(file, Hit::Tr(Act::Path(e.path.clone(), None)));
        let _ = head;
        let mut y = y + 24.0;
        let rows = flow::diff_rows(e);
        if rows.is_empty() {
            return y;
        }
        let all = st.all.contains(id);
        let shown = if all {
            rows.len()
        } else {
            rows.len().min(DIFF_ROWS)
        };
        let digits = (rows.iter())
            .filter_map(|r| r.old.max(r.new))
            .max()
            .unwrap_or(1)
            .to_string()
            .len() as f32;
        let gw = digits * 7.4 + 10.0;
        let num = |n: Option<u32>| n.map_or(String::new(), |n| n.to_string());
        let olds: Vec<String> = rows[..shown].iter().map(|r| num(r.old)).collect();
        let news: Vec<String> = rows[..shown].iter().map(|r| num(r.new)).collect();
        let text: Vec<String> = (rows[..shown].iter())
            .map(|r| match r.mark {
                '~' => "\u{22ef}".to_string(),
                m => format!("{m} {}", r.text),
            })
            .collect();
        let text = text.join("\n");
        let plate = Rect::new(x, y, w, shown as f32 * MONO_LINE + 12.0);
        s.fill(plate, 6.0, Color::rgb(0).with_alpha(0.3));
        s.items.push(rich::Item::Clip(plate));
        for (k, r) in rows[..shown].iter().enumerate() {
            let tint = match r.mark {
                '+' => theme::done().with_alpha(0.14),
                '-' => theme::error().with_alpha(0.16),
                _ => continue,
            };
            s.fill(
                Rect::new(x, y + 6.0 + k as f32 * MONO_LINE, w, MONO_LINE),
                0.0,
                tint,
            );
        }
        if let Some(review) = review {
            for (k, r) in rows[..shown].iter().enumerate() {
                if r.mark == '~' {
                    continue;
                }
                let row = Rect::new(x, y + 6.0 + k as f32 * MONO_LINE, 2.0 * gw, MONO_LINE);
                if review.marks(k) {
                    s.fill(row, 0.0, self.pal.accent.with_alpha(0.22));
                    s.fill(Rect::new(x, row.y, 2.0, row.h), 0.0, self.pal.accent);
                }
                s.hit(row, Hit::Tr(Act::Line(id.to_string(), k)));
            }
        }
        for (k, nums) in [olds, news].iter().enumerate() {
            if let Some(l) = self.layout(&self.fonts.rows, &nums.join("\n"), gw - 6.0) {
                let _ = l.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
                s.text(&l, x + k as f32 * gw, y + 6.0, self.pal.faint);
            }
        }
        if let Some(l) = self.layout(&self.fonts.rows, &text, 100_000.0) {
            s.unit(
                &l,
                x + 2.0 * gw + 6.0,
                y + 6.0,
                self.pal.fg.with_alpha(0.88),
                &text,
            );
        }
        s.items.push(rich::Item::Unclip);
        y = plate.bottom() + 2.0;
        if rows.len() > DIFF_ROWS {
            let say = if all {
                "Show less".to_string()
            } else {
                format!("Show all {} rows", rows.len())
            };
            s.line(
                say,
                F::Small,
                Rect::new(x + 10.0, y, w - 10.0, 20.0),
                self.link_ink(),
            );
            s.hit(Rect::new(x, y, w, 20.0), Hit::Tr(Act::All(id.to_string())));
            y += 20.0;
        }
        y
    }
}
