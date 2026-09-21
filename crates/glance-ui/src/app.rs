//! The UI thread: owns the cluster windows, receives hook events, keeps the
//! set of windows in sync with the set of projects.
//!
//! Hook events arrive on a channel from the listener thread. A small feeder
//! thread applies them to the shared registry and pokes the UI thread with a
//! thread message, so the UI never blocks on the network and an event shows
//! up on screen within a frame. A one second timer moves the age lines.

use std::collections::HashMap;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use glance_core::Registry;
use glance_hooks::listener::{self, Tagged};
use windows::core::Result;
use windows::Win32::Foundation::{LPARAM, RECT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PostThreadMessageW, SetTimer, SystemParametersInfoW,
    TranslateMessage, MSG, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WM_APP, WM_QUIT,
    WM_TIMER,
};

use crate::layout::{self, Metrics};
use crate::render::Gpu;
use crate::window::{self, project_key, project_name, Cluster, Shared};

const WM_GLANCE_EVENT: u32 = WM_APP + 1;
const ENDED_LINGER: Duration = Duration::from_secs(20);
const MARGIN_DIP: i32 = 12;
const GAP_DIP: i32 = 12;

/// Runs the whole desktop app on the calling thread until quit.
pub fn run(port: u16) -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    window::register_class()?;

    let registry = Arc::new(Mutex::new(Registry::new()));
    let ui_thread = unsafe { GetCurrentThreadId() };

    // Listener thread: sockets in, tagged events out.
    let (tx, rx) = mpsc::channel::<Tagged>();
    thread::spawn(move || {
        if let Err(e) = listener::serve(port, tx) {
            eprintln!("glance: listener stopped: {e}");
        }
    });

    // Feeder thread: apply to the registry, wake the UI.
    let feed_registry = Arc::clone(&registry);
    thread::spawn(move || {
        for t in rx {
            let changed = feed_registry
                .lock()
                .map(|mut r| r.apply(&t.glance_id, &t.event, SystemTime::now()))
                .unwrap_or(false);
            unsafe {
                let _ = PostThreadMessageW(
                    ui_thread,
                    WM_GLANCE_EVENT,
                    WPARAM(changed as usize),
                    LPARAM(0),
                );
            }
        }
    });

    let shared = Rc::new(Shared {
        gpu: Gpu::new()?,
        metrics: Metrics::default(),
        registry,
    });
    let mut app = App {
        shared,
        clusters: Vec::new(),
    };

    unsafe {
        SetTimer(None, 0, 1000, None);
    }

    let mut msg = MSG::default();
    loop {
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if !got.as_bool() || msg.message == WM_QUIT {
            break;
        }
        if msg.hwnd.is_invalid() {
            // Thread messages: ours, and the timer.
            match msg.message {
                WM_GLANCE_EVENT => app.reconcile(msg.wParam.0 != 0),
                WM_TIMER => app.tick(),
                _ => {}
            }
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    for c in &app.clusters {
        c.destroy();
    }
    Ok(())
}

struct App {
    shared: Rc<Shared>,
    // Boxed on purpose: the window procedure holds a raw pointer to each
    // Cluster, so it must not move when the Vec grows.
    #[allow(clippy::vec_box)]
    clusters: Vec<Box<Cluster>>,
}

impl App {
    /// Once a second: drop long ended sessions, redraw ages, apply clicks.
    fn tick(&mut self) {
        let pruned = self
            .shared
            .registry
            .lock()
            .map(|mut r| r.prune_ended(ENDED_LINGER, SystemTime::now()))
            .unwrap_or(0);
        if pruned > 0 {
            self.reconcile(true);
        } else {
            self.apply_input();
            for c in &self.clusters {
                c.invalidate();
            }
        }
    }

    /// Makes the windows match the projects in the registry.
    fn reconcile(&mut self, phase_changed: bool) {
        self.apply_input();

        let projects: HashMap<String, usize> = self
            .shared
            .registry
            .lock()
            .map(|r| {
                let mut m = HashMap::new();
                for s in r.all() {
                    *m.entry(project_key(s)).or_insert(0) += 1;
                }
                m
            })
            .unwrap_or_default();

        // Remove clusters whose project is gone.
        let mut i = 0;
        while i < self.clusters.len() {
            if projects.contains_key(&self.clusters[i].key) {
                i += 1;
            } else {
                let c = self.clusters.remove(i);
                c.destroy();
            }
        }

        // Add clusters for new projects, off screen until laid out.
        for key in projects.keys() {
            if !self.clusters.iter().any(|c| &c.key == key) {
                match Cluster::create(
                    Rc::clone(&self.shared),
                    key.clone(),
                    project_name(key),
                    -10_000,
                    -10_000,
                ) {
                    Ok(c) => self.clusters.push(c),
                    Err(e) => eprintln!("glance: cannot create window: {e}"),
                }
            }
        }

        for c in &self.clusters {
            c.fit();
        }
        self.arrange();
        if std::env::var_os("GLANCE_DEBUG").is_some() {
            for c in &self.clusters {
                eprintln!(
                    "cluster {} at {:?} size {:?} pinned={}",
                    c.name,
                    c.position(),
                    c.size_px(),
                    c.pinned
                );
            }
        }

        if phase_changed {
            for c in &self.clusters {
                c.raise();
            }
        }
    }

    /// Header clicks and drags collected by the window procedures.
    fn apply_input(&mut self) {
        let toggles = Cluster::take_toggles();
        let pinned = Cluster::take_pinned();
        if toggles.is_empty() && pinned.is_empty() {
            return;
        }
        for c in &mut self.clusters {
            let id = c.hwnd.0 as isize;
            if toggles.contains(&id) {
                c.collapsed = !c.collapsed;
                c.fit();
            }
            if pinned.contains(&id) {
                c.pinned = true;
            }
        }
        self.arrange();
    }

    /// Stacks the unpinned clusters down the right edge of the work area.
    fn arrange(&self) {
        let free: Vec<&Cluster> = self
            .clusters
            .iter()
            .map(|c| c.as_ref())
            .filter(|c| !c.pinned)
            .collect();
        if free.is_empty() {
            return;
        }
        let work = work_area();
        let scale = free[0].dpi() as f32 / 96.0;
        let width = (self.shared.metrics.width * scale).round() as i32;
        let heights: Vec<i32> = free.iter().map(|c| c.size_px().1).collect();
        let positions = layout::stack(
            &heights,
            width,
            (MARGIN_DIP as f32 * scale) as i32,
            (GAP_DIP as f32 * scale) as i32,
            work,
        );
        for (c, (x, y)) in free.iter().zip(positions) {
            if c.position() != (x, y) {
                c.move_to(x, y);
                // A window that was created off screen has never painted.
                // Moving it into view does not always ask it to.
                c.invalidate();
            }
        }
    }
}

/// Primary monitor work area as (left, top, right, bottom).
fn work_area() -> (i32, i32, i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    if r.right <= r.left {
        return (0, 0, 1920, 1080);
    }
    (r.left, r.top, r.right, r.bottom)
}
