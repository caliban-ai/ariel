//! `ariel-discord` — the Discord backend for Ariel.
//!
//! Consumed as an optional dependency behind the `discord` feature, so a build
//! without that feature cannot load Discord.

/// The provider name this backend registers under.
pub const NAME: &str = "discord";
