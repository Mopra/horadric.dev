//! How the Arcane Sanctuary's graph sits in its pane: a camera that looks
//! at the layout in depth, turned about what it looks at, zoomed toward
//! the cursor and panned across the glass; how large each dot is at its
//! depth, which node is under the mouse and which few hubs are named.
//! Pure, so the pane only draws what this says.

use std::collections::HashMap;

use horadric_core::glow::Glow;
use horadric_core::sanctuary::{Graph, Kind, Point};

/// How much of the room a fitted picture leaves free at each side, in DIPs.
const MARGIN: f32 = 24.0;
/// How far in and out the wheel goes, in DIPs per layout unit.
const MOST: f32 = 40.0;
const LEAST: f32 = 0.01;
/// The smallest a dot gets on screen, in DIPs, so a zoomed out picture
/// still shows every node.
const DOT_MIN: f32 = 1.5;
/// How far past its dot the mouse still finds a node, in DIPs.
const REACH: f32 = 6.0;
/// How many of the largest hubs are named without the mouse on them.
pub const HUBS: usize = 6;
/// How far the camera stands from what it looks at, in the picture's
/// reach: near enough that the depth shows, far enough that the near side
/// does not balloon.
const DISTANCE: f32 = 3.0;
/// How far the near side of a ball of `reach` spreads on screen at that
/// distance, as a share of `reach`: the widest the edge of a sphere seen
/// from three radii away projects to.
const SPREAD: f32 = 1.07;
/// The steepest the camera looks down or up, short of straight, where the
/// turn about the vertical would spin the picture in place.
pub const PITCH_MOST: f32 = 1.4;
/// How many far motes the sky sphere behind the picture holds; the camera
/// sees about a tenth of them at a time.
const MOTES: usize = 700;

/// Where the camera looks from and how large it draws: `target` is drawn
/// at `(x, y)`, a layout unit at its depth is `scale` DIPs, and the
/// camera stands `DISTANCE` reaches back from it, turned by `yaw` about
/// the vertical and `pitch` about the horizontal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub scale: f32,
    pub x: f32,
    pub y: f32,
    pub target: Point,
    /// The picture's radius in layout units, which sets how far back the
    /// camera stands and how deep the fog goes.
    pub reach: f32,
    pub yaw: f32,
    pub pitch: f32,
}

/// A point as the camera sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seen {
    pub at: (f32, f32),
    /// How much nearer than the target's depth makes it larger.
    pub grow: f32,
    /// 0 at the near side of the picture, 1 at the far side.
    pub depth: f32,
}

impl View {
    /// The view that shows all of `points` inside `room` (left, top,
    /// right, bottom), centred, as large as fits whichever way it turns.
    pub fn fit(points: &[Point], room: [f32; 4], yaw: f32, pitch: f32) -> View {
        let (centre, reach) = ball(points);
        View::fit_ball(centre, reach, room, yaw, pitch)
    }

    /// The view that fills `room` with the ball at `centre` of `reach`.
    pub fn fit_ball(centre: Point, reach: f32, room: [f32; 4], yaw: f32, pitch: f32) -> View {
        let [left, top, right, bottom] = room;
        let w = (right - left - 2.0 * MARGIN).max(1.0);
        let h = (bottom - top - 2.0 * MARGIN).max(1.0);
        let reach = reach.max(1.0);
        View {
            scale: (w.min(h) / 2.0 / (reach * SPREAD)).clamp(LEAST, MOST),
            x: (left + right) / 2.0,
            y: (top + bottom) / 2.0,
            target: centre,
            reach,
            yaw,
            pitch,
        }
    }

    /// `d`, from the target, in the camera's frame: x right, y down, z
    /// away from the camera.
    fn rotate(&self, d: Point) -> Point {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let x = d[0] * cy - d[2] * sy;
        let z = d[0] * sy + d[2] * cy;
        [x, d[1] * cp - z * sp, d[1] * sp + z * cp]
    }

    /// The way back from the camera's frame to the layout's.
    fn unrotate(&self, c: Point) -> Point {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let y = c[1] * cp + c[2] * sp;
        let z = -c[1] * sp + c[2] * cp;
        [c[0] * cy + z * sy, y, -c[0] * sy + z * cy]
    }

    pub fn see(&self, p: Point) -> Seen {
        let c = self.rotate(std::array::from_fn(|k| p[k] - self.target[k]));
        let back = self.reach * DISTANCE;
        // A point behind the camera is pinned close in front of it, so a
        // stray far node cannot divide by nothing.
        let grow = back / (back + c[2]).max(back * 0.05);
        Seen {
            at: (
                self.x + c[0] * grow * self.scale,
                self.y + c[1] * grow * self.scale,
            ),
            grow,
            depth: (c[2] / self.reach * 0.5 + 0.5).clamp(0.0, 1.0),
        }
    }

    pub fn to_screen(&self, p: Point) -> (f32, f32) {
        self.see(p).at
    }

    /// Moved by a drag of `(dx, dy)` DIPs: what lies at the target's depth
    /// goes with the mouse, and the turn stays about the middle.
    pub fn panned(&self, dx: f32, dy: f32) -> View {
        let d = self.unrotate([-dx / self.scale, -dy / self.scale, 0.0]);
        View {
            target: std::array::from_fn(|k| self.target[k] + d[k]),
            ..*self
        }
    }

    /// Zoomed by `factor` about the point `at` in DIPs, which stays over
    /// the same place at the target's depth, as a map zooms toward the
    /// cursor.
    pub fn zoomed(&self, at: (f32, f32), factor: f32) -> View {
        let scale = (self.scale * factor).clamp(LEAST, MOST);
        let keep = 1.0 - self.scale / scale;
        let d = self.unrotate([
            (at.0 - self.x) / self.scale * keep,
            (at.1 - self.y) / self.scale * keep,
            0.0,
        ]);
        View {
            scale,
            target: std::array::from_fn(|k| self.target[k] + d[k]),
            ..*self
        }
    }
}

/// The zoom a wheel's turn makes: a notch of 120 is a fifth larger.
pub fn wheel_factor(delta: i32) -> f32 {
    1.2f32.powf(delta as f32 / 120.0)
}

/// The middle of the box around `points` and how far the furthest is from
/// it, the origin and 1 for none.
pub fn ball(points: &[Point]) -> (Point, f32) {
    let Some(first) = points.first() else {
        return ([0.0; 3], 1.0);
    };
    let (lo, hi) = points.iter().fold((*first, *first), |(lo, hi), p| {
        (
            std::array::from_fn(|k| lo[k].min(p[k])),
            std::array::from_fn(|k| hi[k].max(p[k])),
        )
    });
    let mid: Point = std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0);
    let reach = points
        .iter()
        .map(|p| horadric_core::sanctuary::dist(mid, *p))
        .fold(0.0, f32::max);
    (mid, reach.max(1.0))
}

/// A node's radius in layout units: a hub grows with the root of its
/// links, so one with a hundred is large but does not swallow the rest,
/// and a note or a quest is smaller than a file.
pub fn radius(kind: Kind, weight: u32) -> f32 {
    let r = 2.0 + (weight as f32).sqrt().min(12.0) * 0.9;
    match kind {
        Kind::File => r,
        Kind::Note | Kind::Quest => r * 0.7,
    }
}

/// A node's radius on screen, seen as `seen` in this view.
pub fn screen_radius(view: &View, seen: &Seen, kind: Kind, weight: u32) -> f32 {
    (radius(kind, weight) * view.scale * seen.grow).max(DOT_MIN)
}

/// The node under the point `at` in DIPs: of the dots that hold it the
/// nearest the camera, else the nearest on the glass within a little room
/// around its dot.
pub fn node_at(g: &Graph, points: &[Point], view: &View, at: (f32, f32)) -> Option<usize> {
    g.nodes
        .iter()
        .zip(points)
        .enumerate()
        .filter_map(|(i, (n, p))| {
            let s = view.see(*p);
            let d = ((s.at.0 - at.0).powi(2) + (s.at.1 - at.1).powi(2)).sqrt();
            let r = screen_radius(view, &s, n.kind, n.weight);
            (d <= r + REACH).then_some((i, d > r, if d > r { d } else { s.depth }))
        })
        .min_by(|a, b| a.1.cmp(&b.1).then(a.2.total_cmp(&b.2)))
        .map(|(i, ..)| i)
}

/// The `most` heaviest files with any link, heaviest first, which are
/// named even without the mouse on them.
pub fn hubs(g: &Graph, most: usize) -> Vec<usize> {
    let mut files: Vec<usize> = (0..g.nodes.len())
        .filter(|&i| g.nodes[i].kind == Kind::File && g.nodes[i].weight > 0)
        .collect();
    files.sort_by(|&a, &b| g.nodes[b].weight.cmp(&g.nodes[a].weight).then(a.cmp(&b)));
    files.truncate(most);
    files
}

/// What a hub's label says: a file by its name alone, a note or a quest
/// whole but cut short. Hovered, a file shows its whole path.
pub fn short_label(kind: Kind, label: &str) -> String {
    match kind {
        Kind::File => cut(label.rsplit('/').next().unwrap_or(label)),
        Kind::Note | Kind::Quest => cut(label),
    }
}

fn cut(s: &str) -> String {
    const LONGEST: usize = 64;
    if s.chars().count() > LONGEST {
        let cut: String = s.chars().take(LONGEST - 3).collect();
        format!("{}...", cut.trim_end())
    } else {
        s.to_string()
    }
}

/// What the agents lit in a graph, by node index: each lit node's colour
/// and strength, how far a deleted one has faded (1 still there, 0 gone),
/// and the sparks travelling between two nodes, `t` of the way.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Light {
    pub lit: HashMap<usize, (u32, f32)>,
    pub gone: HashMap<usize, f32>,
    pub sparks: Vec<(usize, usize, u32, f32)>,
}

/// Adds what `glow` lit at `now` to `light`, for the nodes `index` finds by
/// id. `prefix` goes before each glow id, so the town's sky, whose ids
/// carry their project, can take each project's glow in turn.
pub fn shine(
    index: &HashMap<String, usize>,
    prefix: &str,
    glow: &Glow,
    now: f64,
    light: &mut Light,
) {
    let find = |id: &str| index.get(&format!("{prefix}{id}")).copied();
    for id in glow.touched() {
        let Some(i) = find(id) else {
            continue;
        };
        if let Some(l) = glow.light(id, now) {
            if l.strength > 0.0 {
                light.lit.insert(i, (l.rgb, l.strength));
            }
        }
        if let Some(g) = glow.gone(id, now) {
            light.gone.insert(i, g);
        }
    }
    for s in glow.sparks(now) {
        if let (Some(a), Some(b)) = (find(&s.from), find(&s.to)) {
            light.sparks.push((a, b, s.rgb, s.t));
        }
    }
}

/// What is drawn of the graph in one frame, in DIPs, far things first so
/// the near ones lie over them. The colours are the pane's to pick from
/// `kind`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SkyFrame {
    pub motes: Vec<Mote>,
    pub lines: Vec<Line>,
    pub dots: Vec<Dot>,
    pub labels: Vec<Label>,
    /// Said in the middle while there is nothing to draw yet.
    pub message: Option<String>,
    pub sparks: Vec<SparkMark>,
}

/// A far speck of the sky behind the picture, which turns with the camera
/// but never comes nearer, so the turn reads even where the graph is thin.
#[derive(Debug, Clone, PartialEq)]
pub struct Mote {
    pub at: (f32, f32),
    pub r: f32,
    pub alpha: f32,
}

/// A spark on its way along a link: its head at `at`, its tail trailing
/// back toward where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct SparkMark {
    pub at: (f32, f32),
    pub tail: (f32, f32),
    pub rgb: u32,
}

/// How long a spark's tail is, as a share of its link.
const TAIL: f32 = 0.25;

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub a: (f32, f32),
    pub b: (f32, f32),
    /// How strongly it is drawn, 0 to 1, by its weight.
    pub strength: f32,
    /// One of its ends is under the mouse.
    pub lit: bool,
    /// Its middle's depth, 0 near to 1 far.
    pub depth: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dot {
    pub at: (f32, f32),
    pub r: f32,
    pub kind: Kind,
    /// Under the mouse or linked to the node that is.
    pub lit: bool,
    /// Under the mouse itself.
    pub hovered: bool,
    /// The colour and strength an agent lit it in.
    pub glow: Option<(u32, f32)>,
    /// 1 while it is there, falling to 0 as a deleted file fades out.
    pub fade: f32,
    /// 0 near to 1 far.
    pub depth: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    /// The point the text is centred under.
    pub at: (f32, f32),
    pub text: String,
    /// The node under the mouse, drawn brighter than a hub's name.
    pub hovered: bool,
}

/// The far motes the camera sees in `room`: a fixed scatter over a sphere
/// around it, seen through a lens about as wide as the room.
fn motes(view: &View, room: [f32; 4]) -> Vec<Mote> {
    let [left, top, right, bottom] = room;
    let (cx, cy) = ((left + right) / 2.0, (top + bottom) / 2.0);
    let lens = (right - left).max(bottom - top) * 0.6;
    let golden = std::f32::consts::PI * (3.0 - 5f32.sqrt());
    (0..MOTES)
        .filter_map(|k| {
            // A Fibonacci sphere, each mote nudged by a hash of its place
            // so they do not sit in visible rows.
            let h = (k as u32).wrapping_mul(2_654_435_761);
            let y = 1.0 - 2.0 * (k as f32 + 0.5) / MOTES as f32;
            let ring = (1.0 - y * y).max(0.0).sqrt();
            let a = k as f32 * golden + (h % 1000) as f32 / 1000.0;
            let c = view.rotate([ring * a.cos(), y, ring * a.sin()]);
            if c[2] <= 0.1 {
                return None;
            }
            let at = (cx + c[0] / c[2] * lens, cy + c[1] / c[2] * lens);
            let inside = at.0 >= left && at.0 <= right && at.1 >= top && at.1 <= bottom;
            inside.then(|| Mote {
                at,
                r: 0.5 + ((h >> 10) % 100) as f32 / 140.0,
                alpha: 0.12 + ((h >> 20) % 100) as f32 / 400.0,
            })
        })
        .collect()
}

/// The frame for `g` laid out at `points` in `view`, with the node
/// `hover` under the mouse. Only what falls in `room` is drawn.
pub fn frame(
    g: &Graph,
    points: &[Point],
    view: &View,
    hover: Option<usize>,
    room: [f32; 4],
    light: &Light,
) -> SkyFrame {
    let [left, top, right, bottom] = room;
    let inside = |(x, y): (f32, f32), r: f32| {
        x + r >= left && x - r <= right && y + r >= top && y - r <= bottom
    };
    let near = |i: usize| hover.is_some_and(|h| h == i);
    let seen: Vec<Seen> = points.iter().map(|p| view.see(*p)).collect();
    let mut f = SkyFrame {
        motes: motes(view, room),
        ..SkyFrame::default()
    };
    let heaviest = g.links.iter().map(|l| l.weight).max().unwrap_or(1).max(1) as f32;
    for l in &g.links {
        let (a, b) = (seen[l.a], seen[l.b]);
        let box_ = [
            a.at.0.min(b.at.0),
            a.at.1.min(b.at.1),
            a.at.0.max(b.at.0),
            a.at.1.max(b.at.1),
        ];
        if box_[2] < left || box_[0] > right || box_[3] < top || box_[1] > bottom {
            continue;
        }
        f.lines.push(Line {
            a: a.at,
            b: b.at,
            strength: 0.35 + 0.65 * (l.weight as f32).ln_1p() / heaviest.ln_1p(),
            lit: near(l.a) || near(l.b),
            depth: (a.depth + b.depth) / 2.0,
        });
    }
    f.lines.sort_by(|a, b| b.depth.total_cmp(&a.depth));
    let linked: Vec<bool> = match hover {
        Some(h) => {
            let mut v = vec![false; g.nodes.len()];
            v[h] = true;
            for l in g.links.iter().filter(|l| l.a == h || l.b == h) {
                v[l.a] = true;
                v[l.b] = true;
            }
            v
        }
        None => Vec::new(),
    };
    for (i, (n, s)) in g.nodes.iter().zip(&seen).enumerate() {
        let r = screen_radius(view, s, n.kind, n.weight);
        if inside(s.at, r) {
            f.dots.push(Dot {
                at: s.at,
                r,
                kind: n.kind,
                lit: linked.get(i).copied().unwrap_or(false),
                hovered: hover == Some(i),
                glow: light.lit.get(&i).copied(),
                fade: light.gone.get(&i).copied().unwrap_or(1.0),
                depth: s.depth,
            });
        }
    }
    f.dots.sort_by(|a, b| b.depth.total_cmp(&a.depth));
    for &(a, b, rgb, t) in &light.sparks {
        let (Some(pa), Some(pb)) = (points.get(a), points.get(b)) else {
            continue;
        };
        let along = |t: f32| view.to_screen(std::array::from_fn(|k| pa[k] + (pb[k] - pa[k]) * t));
        let at = along(t);
        if inside(at, REACH) {
            f.sparks.push(SparkMark {
                at,
                tail: along((t - TAIL).max(0.0)),
                rgb,
            });
        }
    }
    let mut named: Vec<usize> = hubs(g, HUBS);
    named.retain(|&i| Some(i) != hover);
    for (i, hovered) in named
        .into_iter()
        .map(|i| (i, false))
        .chain(hover.map(|h| (h, true)))
    {
        let n = &g.nodes[i];
        let s = seen[i];
        let r = screen_radius(view, &s, n.kind, n.weight);
        if inside(s.at, r) {
            f.labels.push(Label {
                at: (s.at.0, s.at.1 + r + 2.0),
                text: if hovered {
                    cut(&n.label)
                } else {
                    short_label(n.kind, &n.label)
                },
                hovered,
            });
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::sanctuary::{Link, Node};

    fn node(id: &str, kind: Kind, weight: u32) -> Node {
        Node {
            id: id.into(),
            kind,
            label: id.into(),
            weight,
        }
    }

    fn graph() -> Graph {
        Graph {
            nodes: vec![
                node("src/app.rs", Kind::File, 5),
                node("src/a.rs", Kind::File, 3),
                node("src/b.rs", Kind::File, 2),
                node("note:1", Kind::Note, 0),
            ],
            links: vec![
                Link {
                    a: 0,
                    b: 1,
                    weight: 3,
                },
                Link {
                    a: 0,
                    b: 2,
                    weight: 2,
                },
            ],
        }
    }

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-2 && (a.1 - b.1).abs() < 1e-2
    }

    /// Looking straight on, one DIP per unit, the origin in the middle.
    fn flat(x: f32, y: f32) -> View {
        View {
            scale: 1.0,
            x,
            y,
            target: [0.0; 3],
            reach: 1000.0,
            yaw: 0.0,
            pitch: 0.0,
        }
    }

    #[test]
    fn a_fitted_view_holds_every_point_however_it_turns() {
        let pts = [
            [-10.0, -5.0, 40.0],
            [30.0, 15.0, -20.0],
            [0.0, 0.0, 0.0],
            [5.0, 40.0, 5.0],
        ];
        let room = [0.0, 0.0, 400.0, 300.0];
        for k in 0..12 {
            let a = k as f32 * 0.55;
            let v = View::fit(&pts, room, a, (a * 1.7).sin());
            for p in pts {
                let (x, y) = v.to_screen(p);
                assert!((MARGIN - 0.5..=400.0 - MARGIN + 0.5).contains(&x), "{x}");
                assert!((MARGIN - 0.5..=300.0 - MARGIN + 0.5).contains(&y), "{y}");
            }
        }
        let v = View::fit(&pts, room, 0.3, 0.2);
        assert!(close(v.to_screen(ball(&pts).0), (200.0, 150.0)));
    }

    #[test]
    fn an_empty_picture_fits_to_the_middle() {
        let v = View::fit(&[], [0.0, 0.0, 100.0, 50.0], 1.0, 0.5);
        assert!(close(v.to_screen([0.0; 3]), (50.0, 25.0)));
    }

    #[test]
    fn turning_back_undoes_the_turn() {
        let v = View {
            yaw: 0.8,
            pitch: -0.6,
            ..flat(0.0, 0.0)
        };
        let p = [3.0, -7.0, 11.0];
        let back = v.unrotate(v.rotate(p));
        for k in 0..3 {
            assert!((back[k] - p[k]).abs() < 1e-4);
        }
    }

    #[test]
    fn the_near_side_is_drawn_larger_and_in_front() {
        let v = View::fit(&[[0.0, 0.0, -50.0], [0.0, 0.0, 50.0]], [0.0, 0.0, 400.0, 400.0], 0.0, 0.0);
        let (near, far) = (v.see([0.0, 0.0, -50.0]), v.see([0.0, 0.0, 50.0]));
        assert!(near.grow > 1.0 && far.grow < 1.0);
        assert!(near.depth < far.depth);
        // A half turn about the vertical swaps them.
        let v = View {
            yaw: std::f32::consts::PI,
            ..v
        };
        assert!(v.see([0.0, 0.0, -50.0]).depth > v.see([0.0, 0.0, 50.0]).depth);
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let v = View {
            scale: 2.0,
            yaw: 0.9,
            pitch: 0.4,
            target: [5.0, 1.0, -3.0],
            ..flat(10.0, -4.0)
        };
        let at = (123.0, 77.0);
        // The point under the cursor at the target's depth.
        let o = v.unrotate([(at.0 - v.x) / v.scale, (at.1 - v.y) / v.scale, 0.0]);
        let under: Point = std::array::from_fn(|k| v.target[k] + o[k]);
        assert!(close(v.to_screen(under), at));
        let z = v.zoomed(at, 1.7);
        assert!((z.scale - 3.4).abs() < 1e-4);
        assert!(close(z.to_screen(under), at));
        let back = z.zoomed(at, 1.0 / 1.7);
        assert!(close(back.to_screen([0.0; 3]), v.to_screen([0.0; 3])));
    }

    #[test]
    fn zoom_stops_at_its_limits() {
        let v = flat(0.0, 0.0);
        assert_eq!(v.zoomed((0.0, 0.0), 1e6).scale, MOST);
        assert_eq!(v.zoomed((0.0, 0.0), 1e-6).scale, LEAST);
    }

    #[test]
    fn a_drag_moves_the_picture_with_the_mouse() {
        let v = View {
            scale: 3.0,
            yaw: -0.7,
            pitch: 0.3,
            ..flat(1.0, 2.0)
        };
        let p = v.target;
        let (x, y) = v.to_screen(p);
        assert!(close(v.panned(5.0, -7.0).to_screen(p), (x + 5.0, y - 7.0)));
    }

    #[test]
    fn a_wheel_notch_up_zooms_in_and_down_out_by_as_much() {
        assert!(wheel_factor(120) > 1.0);
        assert!((wheel_factor(120) * wheel_factor(-120) - 1.0).abs() < 1e-5);
        assert_eq!(wheel_factor(0), 1.0);
    }

    #[test]
    fn hubs_are_larger_and_notes_smaller_than_files() {
        assert!(radius(Kind::File, 100) > radius(Kind::File, 1));
        assert!(radius(Kind::Note, 4) < radius(Kind::File, 4));
        // A giant does not grow without end.
        assert_eq!(radius(Kind::File, 10_000), radius(Kind::File, 1_000_000));
        let far = View {
            scale: 0.001,
            ..flat(0.0, 0.0)
        };
        let s = far.see([0.0; 3]);
        assert_eq!(screen_radius(&far, &s, Kind::Quest, 0), DOT_MIN);
    }

    #[test]
    fn the_mouse_finds_the_nearest_node_or_none() {
        let g = graph();
        let pts = [
            [0.0, 0.0, 0.0],
            [100.0, 0.0, 0.0],
            [0.0, 100.0, 0.0],
            [100.0, 100.0, 0.0],
        ];
        let v = flat(0.0, 0.0);
        assert_eq!(node_at(&g, &pts, &v, (2.0, 1.0)), Some(0));
        assert_eq!(node_at(&g, &pts, &v, (98.0, 3.0)), Some(1));
        assert_eq!(node_at(&g, &pts, &v, (50.0, 50.0)), None);
    }

    #[test]
    fn of_two_dots_on_one_spot_the_mouse_finds_the_front_one() {
        let g = graph();
        let pts = [
            [0.0, 0.0, 300.0],
            [0.0, 0.0, -300.0],
            [500.0, 0.0, 0.0],
            [0.0, 500.0, 0.0],
        ];
        let v = flat(0.0, 0.0);
        assert_eq!(node_at(&g, &pts, &v, (0.5, 0.5)), Some(1));
        let behind = View {
            yaw: std::f32::consts::PI,
            ..v
        };
        assert_eq!(node_at(&g, &pts, &behind, (0.5, 0.5)), Some(0));
    }

    #[test]
    fn hubs_are_the_heaviest_linked_files() {
        let g = graph();
        assert_eq!(hubs(&g, 2), vec![0, 1]);
        assert_eq!(hubs(&g, 10), vec![0, 1, 2]);
    }

    #[test]
    fn a_file_is_named_by_its_name_and_a_long_note_cut_short() {
        assert_eq!(short_label(Kind::File, "crates/ui/src/app.rs"), "app.rs");
        let long = "word ".repeat(30);
        let s = short_label(Kind::Note, &long);
        assert!(s.ends_with("...") && s.chars().count() <= 64);
        assert_eq!(short_label(Kind::Quest, "Ship it"), "Ship it");
    }

    #[test]
    fn a_frame_lights_the_hovered_node_and_its_links_and_names_it() {
        let g = graph();
        let pts = [
            [0.0, 0.0, 0.0],
            [50.0, 0.0, 0.0],
            [0.0, 50.0, 0.0],
            [200.0, 200.0, 0.0],
        ];
        let v = flat(10.0, 10.0);
        let room = [0.0, 0.0, 120.0, 120.0];
        let f = frame(&g, &pts, &v, Some(1), room, &Light::default());
        // The note far outside the room is left out.
        assert_eq!(f.dots.len(), 3);
        assert_eq!(f.lines.len(), 2);
        let lit: Vec<bool> = f.dots.iter().map(|d| d.lit).collect();
        assert_eq!(lit, vec![true, true, false]);
        assert!(f.dots[1].hovered && !f.dots[0].hovered);
        // The hovered node's link is lit, and the heavier link stronger.
        assert!(f.lines[0].lit && !f.lines[1].lit);
        assert!(f.lines[0].strength > f.lines[1].strength);
        assert!(f.lines[0].strength <= 1.0);
        let hovered: Vec<&Label> = f.labels.iter().filter(|l| l.hovered).collect();
        assert_eq!(hovered.len(), 1);
        assert_eq!(hovered[0].text, "src/a.rs");
        // A hub named on its own is not named twice when hovered.
        assert!(!f.labels.iter().any(|l| l.text == "a.rs"));
    }

    #[test]
    fn a_frame_draws_the_far_dots_first() {
        let g = graph();
        let pts = [
            [0.0, 0.0, -100.0],
            [10.0, 0.0, 300.0],
            [0.0, 10.0, 100.0],
            [10.0, 10.0, 0.0],
        ];
        let v = flat(50.0, 50.0);
        let f = frame(&g, &pts, &v, None, [0.0, 0.0, 100.0, 100.0], &Light::default());
        let depths: Vec<f32> = f.dots.iter().map(|d| d.depth).collect();
        assert!(depths.windows(2).all(|w| w[0] >= w[1]), "{depths:?}");
        assert_eq!(f.dots.last().unwrap().kind, Kind::File);
        assert!(f.dots.last().unwrap().r > f.dots[0].r);
    }

    #[test]
    fn the_far_motes_turn_with_the_camera() {
        let room = [0.0, 0.0, 400.0, 300.0];
        let v = flat(200.0, 150.0);
        let a = motes(&v, room);
        assert!(a.len() > 20, "{} motes", a.len());
        let b = motes(&View { yaw: 0.2, ..v }, room);
        assert_ne!(a, b);
        // Zoom and pan leave them where they were: they are infinitely far.
        assert_eq!(motes(&View { scale: 5.0, ..v }.panned(30.0, 10.0), room), a);
    }

    fn index(g: &Graph, prefix: &str) -> HashMap<String, usize> {
        g.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("{prefix}{}", n.id), i))
            .collect()
    }

    #[test]
    fn shine_lights_the_touched_nodes_and_the_spark_between_them() {
        use horadric_core::glow::Touch;
        let g = graph();
        let mut glow = Glow::default();
        glow.touch("src/a.rs", Touch::Read, 0x112233, 0.0);
        glow.touch("src/app.rs", Touch::Edit, 0x112233, 0.5);
        glow.touch("not/in/graph.rs", Touch::Read, 0x112233, 0.6);
        let mut light = Light::default();
        shine(&index(&g, ""), "", &glow, 1.0, &mut light);
        assert_eq!(light.lit.len(), 2);
        assert_eq!(light.lit[&1].0, 0x112233);
        assert!(
            light.lit[&0].1 > light.lit[&1].1,
            "an edit outshines a read"
        );
        assert_eq!(light.sparks.len(), 1);
        assert_eq!((light.sparks[0].0, light.sparks[0].1), (1, 0));
    }

    #[test]
    fn shine_finds_nodes_by_their_prefixed_ids() {
        use horadric_core::glow::Touch;
        let g = graph();
        let mut glow = Glow::default();
        glow.touch("src/b.rs", Touch::Read, 0x445566, 0.0);
        let mut light = Light::default();
        shine(&index(&g, "p\u{1f}"), "q\u{1f}", &glow, 0.5, &mut light);
        assert!(
            light.lit.is_empty(),
            "another project's glow lights nothing"
        );
        shine(&index(&g, "p\u{1f}"), "p\u{1f}", &glow, 0.5, &mut light);
        assert!(light.lit.contains_key(&2));
    }

    #[test]
    fn a_frame_carries_the_light_and_the_sparks() {
        let g = graph();
        let pts = [
            [0.0, 0.0, 0.0],
            [100.0, 0.0, 0.0],
            [0.0, 50.0, 0.0],
            [20.0, 20.0, 0.0],
        ];
        let v = flat(0.0, 0.0);
        let mut light = Light::default();
        light.lit.insert(1, (0xff0000, 0.5));
        light.gone.insert(2, 0.25);
        light.sparks.push((0, 1, 0x00ff00, 0.5));
        let f = frame(&g, &pts, &v, None, [-10.0, -10.0, 200.0, 200.0], &light);
        let dot = |at: (f32, f32)| f.dots.iter().find(|d| close(d.at, at)).unwrap();
        assert_eq!(dot((100.0, 0.0)).glow, Some((0xff0000, 0.5)));
        assert_eq!(dot((0.0, 50.0)).fade, 0.25);
        assert_eq!(dot((0.0, 0.0)).fade, 1.0);
        assert_eq!(f.dots.iter().filter(|d| d.glow.is_some()).count(), 1);
        assert_eq!(f.sparks.len(), 1);
        assert!(close(f.sparks[0].at, (50.0, 0.0)));
        assert!(close(f.sparks[0].tail, (25.0, 0.0)));
    }
}
