//! `ariel-core` — the provider-agnostic core of the Ariel chat bridge.
//!
//! Home of the `ChatProvider` trait, router, renderer, auth, and the prospero
//! and gonzalo clients. It is always compiled and carries no chat platform SDK;
//! each platform lives in its own feature-gated backend crate.
