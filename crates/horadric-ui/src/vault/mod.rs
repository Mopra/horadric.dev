//! The Vault in the app: the sealed store, sessions' identities, secret run and web login fills. See docs/plan/vault.md.

pub mod fill;
pub mod identity;
pub mod run;
pub mod secrets;
pub mod settings;
pub mod split;
pub mod store;
