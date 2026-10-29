//! A headless `claude.exe` on pipes, speaking stream-json. The session's
//! conversation is drawn by Horadric (the Folio), not a terminal. See
//! docs/plan/headless.md.

use std::io;
use std::path::Path;

pub struct Headless {
    // Filled in by the spike: the child, its stdin, the reader thread.
}

impl Headless {
    /// Starts `claude.exe` in `cwd`, resuming `resume` when given. Each stdout
    /// line goes to `on_line` on a reader thread of its own; the stream ending
    /// calls it with None.
    pub fn start(
        cwd: &Path,
        resume: Option<&str>,
        on_line: impl FnMut(Option<String>) + Send + 'static,
    ) -> io::Result<Headless> {
        let _ = (cwd, resume, on_line);
        todo!()
    }

    pub fn send_prompt(&self, text: &str) -> io::Result<()> {
        let _ = text;
        todo!()
    }

    pub fn interrupt(&self) -> io::Result<()> {
        todo!()
    }

    /// Writes one raw line, such as the `control_response` from
    /// `Folio::answer`.
    pub fn send_line(&self, line: &str) -> io::Result<()> {
        let _ = line;
        todo!()
    }
}

// Dropping a Headless ends the claude.exe it started (the spike adds the Drop).
