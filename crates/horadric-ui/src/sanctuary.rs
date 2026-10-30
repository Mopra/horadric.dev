//! The Arcane Sanctuary's pane, one per project: its graph read on a
//! thread, its layout stepped on the vsync clock while it settles, and
//! where the view is panned and zoomed to.
//! What a frame shows is worked out in [`crate::sky`]; this keeps the state
//! the pane comes back to, so a stage that switches projects and back
//! finds the picture as it left it.
//!
//! The graph needs the code index, which lives in the `horadric` binary,
//! not here, so the binary hands over the function that builds it with
//! [`set_source`] before the app starts, and the one that builds the
//! town's sky of every project with [`set_town_source`].
//!
//! What the agents touch is lit from [`crate::glows`]: a touch posts the
//! app an event, the app redraws every sanctuary when the glows' stamp
//! moved, and the pane keeps its vsync clock running while anything fades
//! or travels.
//!
//! The picture is in depth: a drag turns it, a drag with Shift pans it, a
//! flick lets it coast, and left alone it drifts slowly round, stopping
//! while a node is under the mouse so its name can be read.
//!
//! M turns a project's sanctuary into its mind and back: the memory's
//! passages linked by meaning, lit by [`crate::mind`] as recalls find
//! them. Each picture keeps its own state while the other is shown.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::OnceLock;
use std::time::Instant;

use horadric_core::sanctuary::{Graph, Layout, Point};
use horadric_core::saved::{Dock, Side};
use horadric_hooks::lore::Place;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_USER};

use crate::sky::{self, Label, Light, SkyFrame, View};

/// Posted to the pane that asked for a graph once it is read.
pub const WM_SKY_READY: u32 = WM_USER + 8;
/// Layout steps per frame: the simulation cools in about 300, so a picture
/// from nothing settles in under three seconds.
const STEPS: usize = 2;
/// How fast the picture drifts round when left alone, in radians a
/// second: once round in a minute and a half.
const DRIFT: f32 = 0.07;
/// How far a drag turns the picture, in radians per DIP.
const TURN: f32 = 0.006;
/// How much of a flick's speed is left after a second of coasting.
const COAST: f32 = 0.15;
/// The tilt a picture first opens at, so its depth shows from the start.
const TILT: f32 = 0.35;

/// Builds a project's graph from a folder in it.
pub type Source = fn(&Path) -> Result<Graph, String>;

static SOURCE: OnceLock<Source> = OnceLock::new();

/// Gives the app the function that builds a graph. Without it the pane
/// says it has nothing to show.
pub fn set_source(f: Source) {
    let _ = SOURCE.set(f);
}

/// Tristram's sky as the pane draws it: every project's graph in one,
/// each node's id `<project key>` and [`SEP`] before its own, at fixed
/// points.
pub struct Town {
    pub graph: Graph,
    pub points: Vec<Point>,
    pub galaxies: Vec<Galaxy>,
}

pub struct Galaxy {
    pub key: String,
    pub name: String,
    /// On the plane the galaxies are packed on, at depth 0.
    pub centre: [f32; 2],
    pub radius: f32,
    /// Its nodes' indices in the town's graph.
    pub nodes: Range<usize>,
}

/// What joins a project key to a node id in the town's graph.
pub const SEP: char = '\u{1f}';

/// Builds Tristram's sky.
pub type TownSource = fn() -> Result<Town, String>;

static TOWN_SOURCE: OnceLock<TownSource> = OnceLock::new();

pub fn set_town_source(f: TownSource) {
    let _ = TOWN_SOURCE.set(f);
}

enum Read {
    Project(Graph, Layout),
    Town(Town),
    /// The mind, with the memory's key of the project.
    Mind(String, Graph, Layout),
}

enum State {
    /// Not asked for yet: the pane has not drawn.
    Idle,
    Reading(Receiver<Result<Read, String>>),
    Failed(String),
    Ready {
        graph: Graph,
        layout: Layout,
    },
    /// The town: laid out already, never stepped.
    Town(Town),
}

/// The picture not shown, code or mind, as it was left.
struct Stashed {
    state: State,
    index: HashMap<String, usize>,
    view: Option<View>,
}

struct Sky {
    dir: PathBuf,
    key: String,
    town: bool,
    /// Showing the memory rather than the code.
    mind: bool,
    stash: Option<Stashed>,
    /// The memory's key of the project, which recalls are told by, once
    /// the mind is read.
    mind_key: Option<String>,
    /// The mind's synapses: each node's neighbours, and each pair once.
    next: Vec<Vec<usize>>,
    pairs: Vec<(usize, usize)>,
    /// Each node's index by its id, for the glow to find it.
    index: HashMap<String, usize>,
    /// The glass of the last frame, which a click into a galaxy fits to.
    room: [f32; 4],
    /// Whether the mouse moved since it was pressed, so a release is a
    /// click only when it did not.
    dragged: bool,
    state: State,
    /// Where the user panned and zoomed to, None while the picture still
    /// fits itself to the pane.
    view: Option<View>,
    /// The view of the last frame, which a first drag or wheel starts from.
    last: Option<View>,
    hover: Option<usize>,
    /// Where the mouse is over the glass, so a frame that turned the
    /// picture under it finds what it is over now.
    mouse: Option<(f32, f32)>,
    /// Where the mouse was pressed or last dragged to, in DIPs.
    grab: Option<(f32, f32)>,
    /// When it was last dragged, to take a flick's speed from.
    grabbed_at: Option<Instant>,
    yaw: f32,
    pitch: f32,
    /// How fast it turns about the vertical and the horizontal, in
    /// radians a second.
    spin: (f32, f32),
    /// The last frame the turn was stepped at.
    tick: Option<Instant>,
}

thread_local! {
    static SKIES: RefCell<HashMap<String, Sky>> = RefCell::new(HashMap::new());
    /// Kept apart from the skies, so a sanctuary closed and opened again
    /// comes back to the side it stood on.
    static DOCKS: RefCell<HashMap<String, Dock>> = RefCell::new(HashMap::new());
}

/// Where the project's sanctuary stands beside the stage's grid, or None
/// while it takes a place in it.
pub fn dock(key: &str) -> Option<Dock> {
    DOCKS.with(|d| d.borrow().get(key).copied())
}

/// Stands the project's sanctuary beside the grid, or puts it back in it
/// with None. The caller lays the stage out again.
pub fn set_dock(key: &str, dock: Option<Dock>) {
    DOCKS.with(|d| {
        let mut d = d.borrow_mut();
        match dock {
            Some(v) => d.insert(key.to_string(), v),
            None => d.remove(key),
        }
    });
}

/// Moves the sanctuary to `side`, keeping its size along the same axis,
/// or back into the grid when it is there already.
pub fn toggle_dock(key: &str, side: Side) {
    set_dock(key, crate::viewport::toggled(dock(key), side));
}

/// Every project's dock, to save.
pub fn docks() -> std::collections::BTreeMap<String, Dock> {
    DOCKS.with(|d| d.borrow().iter().map(|(k, &v)| (k.clone(), v)).collect())
}

/// The docks saved last time.
pub fn set_docks(saved: &std::collections::BTreeMap<String, Dock>) {
    DOCKS.with(|d| *d.borrow_mut() = saved.iter().map(|(k, &v)| (k.clone(), v)).collect());
}

fn with<T>(key: &str, f: impl FnOnce(&mut Sky) -> T) -> Option<T> {
    SKIES.with(|s| s.borrow_mut().get_mut(key).map(f))
}

/// The sanctuary of the project `key`, in `dir`. Opening it again keeps the
/// one there is.
pub fn open(key: &str, dir: &Path) {
    SKIES.with(|s| {
        s.borrow_mut()
            .entry(key.to_string())
            .or_insert_with(|| Sky {
                dir: dir.to_path_buf(),
                key: key.to_string(),
                town: crate::town::key() == Some(key),
                mind: false,
                stash: None,
                mind_key: None,
                next: Vec::new(),
                pairs: Vec::new(),
                index: HashMap::new(),
                room: [0.0; 4],
                dragged: false,
                state: State::Idle,
                view: None,
                last: None,
                hover: None,
                mouse: None,
                grab: None,
                grabbed_at: None,
                yaw: 0.0,
                pitch: TILT,
                spin: (DRIFT, 0.0),
                tick: None,
            });
    });
}

pub fn close(key: &str) {
    SKIES.with(|s| s.borrow_mut().remove(key));
}

/// Starts reading the graph when nothing has, and takes it once read. The
/// thread wakes `hwnd` when it is done.
fn poll(sky: &mut Sky, hwnd: HWND) {
    match &sky.state {
        State::Idle => {
            let (tx, rx) = mpsc::channel();
            let dir = sky.dir.clone();
            let (town, mind) = (sky.town, sky.mind);
            let wake = hwnd.0 as isize;
            std::thread::spawn(move || {
                let read = if town {
                    read_town()
                } else if mind {
                    read_mind(&dir)
                } else {
                    read(&dir)
                };
                let _ = tx.send(read);
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(wake as *mut _)),
                        WM_SKY_READY,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            });
            sky.state = State::Reading(rx);
        }
        State::Reading(rx) => match rx.try_recv() {
            Ok(Ok(Read::Project(graph, layout))) => {
                sky.index = index(&graph);
                sky.state = State::Ready { graph, layout };
            }
            Ok(Ok(Read::Mind(key, graph, layout))) => {
                sky.index = index(&graph);
                sky.next = vec![Vec::new(); graph.nodes.len()];
                for l in &graph.links {
                    sky.next[l.a].push(l.b);
                    sky.next[l.b].push(l.a);
                }
                sky.pairs = graph.links.iter().map(|l| (l.a, l.b)).collect();
                sky.mind_key = Some(key);
                sky.state = State::Ready { graph, layout };
            }
            Ok(Ok(Read::Town(town))) => {
                sky.index = index(&town.graph);
                sky.state = State::Town(town);
            }
            Ok(Err(e)) => sky.state = State::Failed(e),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => sky.state = State::Failed("no graph".into()),
        },
        _ => {}
    }
}

fn read(dir: &Path) -> Result<Read, String> {
    let source = SOURCE.get().ok_or("this build cannot read a graph")?;
    let place = Place::of(dir)?;
    let graph = source(dir)?;
    let layout = Layout::new(&graph, &place.positions());
    Ok(Read::Project(graph, layout))
}

fn read_mind(dir: &Path) -> Result<Read, String> {
    let (key, graph) = crate::memory::mind(dir)?;
    let layout = Layout::new(&graph, &Place::of(dir)?.mind_positions());
    Ok(Read::Mind(key, graph, layout))
}

fn read_town() -> Result<Read, String> {
    let source = TOWN_SOURCE.get().ok_or("this build cannot read the sky")?;
    Ok(Read::Town(source()?))
}

fn index(g: &Graph) -> HashMap<String, usize> {
    g.nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.clone(), i))
        .collect()
}

/// What the agents lit in `sky` now: its project's glow, or in the town
/// every project's.
fn light(sky: &Sky) -> Light {
    let mut light = Light::default();
    let now = crate::glows::now();
    if sky.mind {
        if let Some(key) = &sky.mind_key {
            light = crate::mind::shine(key, &sky.index, &sky.next, &sky.pairs, now);
        }
        return light;
    }
    match &sky.state {
        State::Town(town) => {
            for g in &town.galaxies {
                let prefix = format!("{}{SEP}", g.key);
                crate::glows::with(&g.key, |glow| {
                    sky::shine(&sky.index, &prefix, glow, now, &mut light)
                });
            }
        }
        _ => {
            crate::glows::with(&sky.key, |glow| {
                sky::shine(&sky.index, "", glow, now, &mut light)
            });
        }
    }
    light
}

/// Whether anything the agents lit in `sky` still fades or travels.
fn glowing(sky: &Sky) -> bool {
    let now = crate::glows::now();
    let moving = |key: &str| crate::glows::with(key, |glow| glow.moving(now)).unwrap_or(false);
    if sky.mind {
        // Its wandering sparks never rest while it has synapses.
        return matches!(sky.state, State::Ready { .. })
            && (!sky.pairs.is_empty()
                || sky
                    .mind_key
                    .as_deref()
                    .is_some_and(|k| crate::mind::firing(k, now)));
    }
    match &sky.state {
        State::Town(town) => town.galaxies.iter().any(|g| moving(&g.key)),
        State::Ready { .. } => moving(&sky.key),
        _ => false,
    }
}

/// The view of `points` this frame: where the user panned and zoomed to,
/// or fitted, turned as the picture is turned now.
fn view_of(view: Option<View>, (yaw, pitch): (f32, f32), points: &[Point], room: [f32; 4]) -> View {
    match view {
        Some(v) => View {
            yaw,
            pitch,
            x: (room[0] + room[2]) / 2.0,
            y: (room[1] + room[3]) / 2.0,
            ..v
        },
        None => View::fit(points, room, yaw, pitch),
    }
}

/// What the pane draws in `room`, the glass in DIPs.
pub fn frame(key: &str, hwnd: HWND, room: [f32; 4]) -> SkyFrame {
    with(key, |sky| {
        poll(sky, hwnd);
        let message = |m: &str| SkyFrame {
            message: Some(m.to_string()),
            ..SkyFrame::default()
        };
        match &sky.state {
            State::Idle | State::Reading(_) if sky.mind => message("Waking the mind\u{2026}"),
            State::Idle | State::Reading(_) => message("Reading the project\u{2026}"),
            State::Failed(e) => message(e),
            State::Ready { graph, .. } if sky.mind && graph.nodes.is_empty() => {
                message("Nothing remembered with meaning here yet")
            }
            State::Ready { graph, .. } if graph.nodes.is_empty() => {
                message("No files here to draw")
            }
            State::Town(town) if town.graph.nodes.is_empty() => message("No projects charted yet"),
            State::Ready { graph, layout } => {
                let points = layout.positions();
                let view = view_of(sky.view, (sky.yaw, sky.pitch), points, room);
                (sky.last, sky.room) = (Some(view), room);
                // The picture may have turned under a still mouse.
                let hover = sky
                    .mouse
                    .and_then(|at| sky::node_at(graph, points, &view, at));
                sky.hover = hover;
                sky::frame(graph, points, &view, hover, room, &light(sky))
            }
            State::Town(town) => {
                let points = &town.points;
                let view = view_of(sky.view, (sky.yaw, sky.pitch), points, room);
                (sky.last, sky.room) = (Some(view), room);
                let hover = sky
                    .mouse
                    .and_then(|at| sky::node_at(&town.graph, points, &view, at));
                sky.hover = hover;
                let light = light(sky);
                let mut f = sky::frame(&town.graph, points, &view, hover, room, &light);
                // Out here a galaxy is named, not a project's hubs.
                f.labels.retain(|l| l.hovered);
                for g in &town.galaxies {
                    let s = view.see([g.centre[0], g.centre[1], 0.0]);
                    f.labels.push(Label {
                        at: (s.at.0, s.at.1 - g.radius * view.scale * s.grow - 18.0),
                        text: g.name.clone(),
                        hovered: false,
                    });
                }
                f
            }
        }
    })
    .unwrap_or_default()
}

/// What the pane's header says after its name.
pub fn detail(key: &str) -> String {
    with(key, |sky| match &sky.state {
        State::Town(town) => format!("{} projects", town.galaxies.len()),
        State::Ready { graph, .. } if sky.mind => format!(
            "mind: {} memories, {} synapses  \u{b7}  M for code",
            graph.nodes.len(),
            graph.links.len()
        ),
        State::Ready { graph, .. } => {
            let files = graph
                .nodes
                .iter()
                .filter(|n| n.kind == horadric_core::sanctuary::Kind::File)
                .count();
            format!(
                "{files} files, {} links  \u{b7}  M for the mind",
                graph.links.len()
            )
        }
        _ => String::new(),
    })
    .unwrap_or_default()
}

/// Whether the layout is still settling, a glow still fades or the
/// picture turns, so the pane wants frames.
pub fn moving(key: &str) -> bool {
    with(key, |sky| {
        matches!(&sky.state, State::Ready { layout, .. } if !layout.still())
            || glowing(sky)
            || turning(sky)
    })
    .unwrap_or(false)
}

/// Whether the picture turns on its own: drifting, or coasting from a
/// flick, and not held by the mouse.
fn turning(sky: &Sky) -> bool {
    let shown = matches!(sky.state, State::Ready { .. } | State::Town(_));
    shown && sky.grab.is_none() && (sky.spin.0.abs() + sky.spin.1.abs() > 1e-3 || drifts(sky))
}

/// Left alone it drifts; with a node under the mouse it holds still, so
/// the name can be read and the node clicked.
fn drifts(sky: &Sky) -> bool {
    sky.hover.is_none()
}

/// Turns the picture by the time since the last frame: a flick's speed
/// eases toward the drift, or toward still while a node is held.
fn turn(sky: &mut Sky) {
    let now = Instant::now();
    // A frame after a pause turns as if no time passed in between.
    let dt = sky
        .tick
        .replace(now)
        .map_or(0.0, |t| (now - t).as_secs_f32().min(0.1));
    if sky.grab.is_some() {
        return;
    }
    sky.yaw = (sky.yaw + sky.spin.0 * dt) % std::f32::consts::TAU;
    sky.pitch = (sky.pitch + sky.spin.1 * dt).clamp(-sky::PITCH_MOST, sky::PITCH_MOST);
    let ease = COAST.powf(dt);
    let rest = if drifts(sky) { DRIFT } else { 0.0 };
    sky.spin = (rest + (sky.spin.0 - rest) * ease, sky.spin.1 * ease);
}

/// Moves the layout and the turn on a frame. Once the layout comes to
/// rest, where it settled is kept, so the next open starts from there.
pub fn step(key: &str) {
    with(key, |sky| {
        turn(sky);
        let State::Ready { graph, layout } = &mut sky.state else {
            return;
        };
        if layout.still() {
            return;
        }
        for _ in 0..STEPS {
            layout.step();
        }
        if layout.still() {
            let kept = layout.kept(graph);
            let dir = sky.dir.clone();
            let mind = sky.mind;
            std::thread::spawn(move || {
                let keep = |p: Place| {
                    if mind {
                        p.keep_mind_positions(&kept)
                    } else {
                        p.keep_positions(&kept)
                    }
                };
                if let Err(e) = Place::of(&dir).and_then(keep) {
                    eprintln!("horadric: cannot keep the sanctuary's layout: {e}");
                }
            });
        }
    });
}

/// The mouse moved to `at`, or left the pane at None. True when what is
/// under it changed, so the pane draws again.
pub fn hover(key: &str, at: Option<(f32, f32)>) -> bool {
    with(key, |sky| {
        sky.mouse = at;
        let under = match (&sky.state, sky.last, at) {
            (State::Ready { graph, layout }, Some(view), Some(at)) => {
                sky::node_at(graph, layout.positions(), &view, at)
            }
            (State::Town(town), Some(view), Some(at)) => {
                sky::node_at(&town.graph, &town.points, &view, at)
            }
            _ => None,
        };
        std::mem::replace(&mut sky.hover, under) != under
    })
    .unwrap_or(false)
}

/// The mouse pressed at `at` in the glass, which may start a drag.
pub fn grab(key: &str, at: (f32, f32)) {
    with(key, |sky| {
        sky.grab = Some(at);
        sky.grabbed_at = Some(Instant::now());
        sky.dragged = false;
        sky.spin = (0.0, 0.0);
    });
}

/// A drag to `at` turns the picture with the mouse, or with Shift held
/// pans it. True when it moved.
pub fn drag(key: &str, at: (f32, f32)) -> bool {
    let pan = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    with(key, |sky| {
        let (Some(from), Some(view)) = (sky.grab, sky.view.or(sky.last)) else {
            return false;
        };
        if at == from {
            return false;
        }
        let (dx, dy) = (at.0 - from.0, at.1 - from.1);
        let now = Instant::now();
        let dt = sky
            .grabbed_at
            .replace(now)
            .map_or(0.0, |t| (now - t).as_secs_f32());
        sky.grab = Some(at);
        sky.dragged = true;
        if pan {
            sky.view = Some(view.panned(dx, dy));
            sky.spin = (0.0, 0.0);
        } else {
            sky.yaw += dx * TURN;
            sky.pitch = (sky.pitch + dy * TURN).clamp(-sky::PITCH_MOST, sky::PITCH_MOST);
            // The speed of the last stretch of the drag, which a let go
            // keeps, so a flick coasts on.
            if dt > 0.0 {
                sky.spin = (dx * TURN / dt.max(0.008), dy * TURN / dt.max(0.008));
            }
        }
        true
    })
    .unwrap_or(false)
}

/// The mouse let go. A drag that stopped before letting go does not
/// coast. In the town a click that did not drag goes into the galaxy
/// under it, fitting the view to it. True when the view changed.
pub fn release(key: &str) -> bool {
    with(key, |sky| {
        if sky
            .grabbed_at
            .is_some_and(|t| t.elapsed().as_secs_f32() > 0.08)
        {
            sky.spin = (0.0, 0.0);
        }
        sky.tick = None;
        let click = sky.grab.take().filter(|_| !sky.dragged);
        let (Some(at), Some(view), State::Town(town)) = (click, sky.last, &sky.state) else {
            return false;
        };
        // Of the galaxies whose disc on the glass holds the click, the
        // nearest the camera.
        let Some(g) = town
            .galaxies
            .iter()
            .filter_map(|g| {
                let s = view.see([g.centre[0], g.centre[1], 0.0]);
                let d = ((s.at.0 - at.0).powi(2) + (s.at.1 - at.1).powi(2)).sqrt();
                (d <= g.radius * view.scale * s.grow).then_some((g, s.depth))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(g, _)| g)
        else {
            return false;
        };
        let [x, y] = g.centre;
        sky.view = Some(View::fit_ball(
            [x, y, 0.0],
            g.radius,
            sky.room,
            sky.yaw,
            sky.pitch,
        ));
        true
    })
    .unwrap_or(false)
}

/// The press ended without a release, as when a menu took the capture:
/// no click.
pub fn cancel(key: &str) {
    with(key, |sky| sky.grab = None);
}

pub fn dragging(key: &str) -> bool {
    with(key, |sky| sky.grab.is_some()).unwrap_or(false)
}

/// The wheel turned by `delta` with the mouse at `at`: zoom toward it.
pub fn wheel(key: &str, at: (f32, f32), delta: i32) {
    with(key, |sky| {
        if let Some(view) = sky.view.or(sky.last) {
            sky.view = Some(view.zoomed(at, sky::wheel_factor(delta)));
        }
    });
}

/// Turns a project's sanctuary from its code to its mind or back, keeping
/// the picture left for when it comes back. The town has no mind. True
/// when it turned.
pub fn toggle_mind(key: &str) -> bool {
    with(key, |sky| {
        if sky.town {
            return false;
        }
        let left = Stashed {
            state: std::mem::replace(&mut sky.state, State::Idle),
            index: std::mem::take(&mut sky.index),
            view: sky.view.take(),
        };
        if let Some(back) = sky.stash.replace(left) {
            (sky.state, sky.index, sky.view) = (back.state, back.index, back.view);
        }
        sky.mind = !sky.mind;
        (sky.hover, sky.last) = (None, None);
        true
    })
    .unwrap_or(false)
}

/// Back to the whole picture fitted to the pane, as at first.
pub fn refit(key: &str) {
    with(key, |sky| sky.view = None);
}
