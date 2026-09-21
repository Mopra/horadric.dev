//! Core model for Glance: what a session is, what state it is in, and how
//! Claude Code hook events move it between states.
//!
//! This crate has no I/O. The hook listener feeds it [`HookEvent`]s, the UI
//! reads [`Session`]s out of a [`Registry`]. Everything here is testable
//! without a terminal or a network.

pub mod event;
pub mod registry;
pub mod session;

pub use event::HookEvent;
pub use registry::Registry;
pub use session::{Phase, Session, WaitReason};
