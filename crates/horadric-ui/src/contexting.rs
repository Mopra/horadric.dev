//! The app's side of the Context window: which folder a project's
//! switches write, which projects the overview lists, and handing the
//! window each project's reading as its thread finishes it.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::{App, WM_HORADRIC_CONTEXT};
use crate::contextpanel::{self, Ask, ContextPanel, Project};
use crate::project::folder_key;
use crate::window::project_name;

impl App {
    /// Does what the Context window asks, or opens it.
    pub(super) fn context_asks(&mut self, ask: Ask) {
        match ask {
            Ask::Close => {
                if let Some(w) = self.context_panel.take() {
                    w.destroy();
                }
            }
            Ask::Open(key) => {
                let p = self.context_project(&key);
                let job = vec![(p.key.clone(), p.main.clone())];
                if let Some(w) = self.context_window() {
                    w.show_project(p);
                    w.raise();
                }
                contextpanel::gather(job, self.notify.0 as isize, WM_HORADRIC_CONTEXT);
            }
            Ask::Overview => {
                let mut keys: Vec<String> = self
                    .recent
                    .iter()
                    .filter(|p| Path::new(p).is_dir())
                    .map(|p| folder_key(p))
                    .collect();
                keys.dedup();
                let projects: Vec<Project> = keys.iter().map(|k| self.context_project(k)).collect();
                let jobs = projects
                    .iter()
                    .map(|p| (p.key.clone(), p.main.clone()))
                    .collect();
                if let Some(w) = self.context_window() {
                    w.show_overview(projects);
                    w.raise();
                }
                contextpanel::gather(jobs, self.notify.0 as isize, WM_HORADRIC_CONTEXT);
            }
        }
    }

    /// The window, opened if it is not.
    fn context_window(&mut self) -> Option<&ContextPanel> {
        if self.context_panel.is_none() {
            match ContextPanel::open(Rc::clone(&self.shared)) {
                Ok(w) => self.context_panel = Some(w),
                Err(e) => eprintln!("horadric: cannot open the context window: {e}"),
            }
        }
        self.context_panel.as_deref()
    }

    fn context_project(&self, key: &str) -> Project {
        Project {
            key: key.to_string(),
            name: project_name(key),
            main: self.project_dir(key).unwrap_or_else(|| PathBuf::from(key)),
        }
    }

    /// Hands the window what the threads have read since last time.
    pub(super) fn take_context(&mut self) {
        let read = contextpanel::take();
        if let Some(w) = &self.context_panel {
            for (key, inv) in read {
                w.gathered(key, inv);
            }
        }
    }
}
