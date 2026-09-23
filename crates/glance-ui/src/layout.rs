//! Where things go inside a cluster window, in device independent pixels.
//!
//! Pure functions so the geometry can be tested without a window. The
//! renderer scales by DPI, this module never sees a physical pixel.

/// A rectangle in DIPs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// Shrinks on every side.
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2.0 * d, self.h - 2.0 * d)
    }
}

/// Fixed sizes for a cluster. One place to tune the look.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub width: f32,
    pub pad: f32,
    pub header_h: f32,
    pub tile_h: f32,
    pub gap: f32,
    pub radius: f32,
    pub tile_radius: f32,
    pub dot: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            width: 280.0,
            pad: 10.0,
            header_h: 30.0,
            tile_h: 54.0,
            gap: 6.0,
            radius: 12.0,
            tile_radius: 8.0,
            dot: 8.0,
        }
    }
}

/// The computed geometry of one cluster window.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterLayout {
    /// Full window size.
    pub size: (f32, f32),
    pub header: Rect,
    /// The button at the right end of the header that starts a new session
    /// in this project. Inside `header`, so it is hit tested first.
    pub new: Rect,
    /// One rect per tile, in the order given. Empty when collapsed.
    pub tiles: Vec<Rect>,
}

/// Lays out a cluster with `n` tiles.
pub fn cluster(m: &Metrics, n: usize, collapsed: bool) -> ClusterLayout {
    let header = Rect::new(m.pad, m.pad, m.width - 2.0 * m.pad, m.header_h);
    let new = Rect::new(
        header.right() - m.header_h,
        header.y,
        m.header_h,
        m.header_h,
    );
    let mut tiles = Vec::new();
    let mut y = header.bottom() + m.gap;
    if !collapsed {
        for _ in 0..n {
            tiles.push(Rect::new(m.pad, y, m.width - 2.0 * m.pad, m.tile_h));
            y += m.tile_h + m.gap;
        }
    }
    // Trailing gap becomes bottom padding.
    let height = if tiles.is_empty() {
        header.bottom() + m.pad
    } else {
        y - m.gap + m.pad
    };
    ClusterLayout {
        size: (m.width, height),
        header,
        new,
        tiles,
    }
}

/// Which part of the cluster a point is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    New,
    Header,
    Tile(usize),
    Nothing,
}

pub fn hit(layout: &ClusterLayout, x: f32, y: f32) -> Hit {
    if layout.new.contains(x, y) {
        return Hit::New;
    }
    if layout.header.contains(x, y) {
        return Hit::Header;
    }
    for (i, t) in layout.tiles.iter().enumerate() {
        if t.contains(x, y) {
            return Hit::Tile(i);
        }
    }
    Hit::Nothing
}

/// Stacks cluster windows down the right edge of a work area.
///
/// `heights` are window heights in physical pixels, `work` is the monitor
/// work area as (left, top, right, bottom). Returns the top left corner for
/// each window. When the column overflows, a new column starts to the left.
pub fn stack(
    heights: &[i32],
    width: i32,
    margin: i32,
    gap: i32,
    work: (i32, i32, i32, i32),
) -> Vec<(i32, i32)> {
    let (left, top, right, bottom) = work;
    let mut out = Vec::with_capacity(heights.len());
    let mut x = right - margin - width;
    let mut y = top + margin;
    for &h in heights {
        if y + h > bottom - margin && y != top + margin {
            x -= width + gap;
            y = top + margin;
        }
        // Never leave the screen to the left. Overlap is better than lost.
        let x = x.max(left + margin);
        out.push((x, y));
        y += h + gap;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapsed_is_header_only() {
        let m = Metrics::default();
        let l = cluster(&m, 5, true);
        assert!(l.tiles.is_empty());
        assert_eq!(l.size.1, m.pad + m.header_h + m.pad);
    }

    #[test]
    fn tiles_stack_with_gaps() {
        let m = Metrics::default();
        let l = cluster(&m, 3, false);
        assert_eq!(l.tiles.len(), 3);
        assert_eq!(l.tiles[1].y - l.tiles[0].bottom(), m.gap);
        assert_eq!(l.size.1, l.tiles[2].bottom() + m.pad);
        assert_eq!(hit(&l, 20.0, l.tiles[2].y + 1.0), Hit::Tile(2));
        assert_eq!(hit(&l, 20.0, m.pad + 1.0), Hit::Header);
        assert_eq!(hit(&l, m.width - m.pad - 2.0, m.pad + 1.0), Hit::New);
        assert_eq!(hit(&l, 1.0, 1.0), Hit::Nothing);
    }

    #[test]
    fn stack_goes_down_then_left() {
        let pos = stack(&[100, 100, 100], 280, 12, 12, (0, 0, 1920, 250));
        assert_eq!(pos[0], (1920 - 12 - 280, 12));
        assert_eq!(pos[1], (1920 - 12 - 280, 124));
        // Third does not fit below, new column.
        assert_eq!(pos[2], (1920 - 12 - 280 - 292, 12));
    }
}
