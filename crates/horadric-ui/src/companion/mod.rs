//! The companion: follows the human's work outside Horadric and writes
//! it to the trail, so a session can be handed what they were doing.
//! The plan is docs/plan/companion.md.
//!
//! The watching runs in a process of its own (`horadric companion`,
//! [`run`]): a UI Automation call into a hung program can wait for
//! ever, and that must not be the process that draws the tiles. The
//! app starts and stops that process ([`link`]), reads the trail it
//! writes ([`trail`]) and starts the session a take over asks for
//! ([`takeover`]).

pub mod link;
pub mod never;
pub mod read;
pub mod takeover;
pub mod trail;
pub mod watch;

/// `horadric companion [peek]`: watches until it is stopped. `peek`
/// prints once what it would have written for the window in front.
pub fn run(args: &[String]) -> Result<(), String> {
    watch::run(args)
}
