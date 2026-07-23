//! Embedded compiled Elm frontend (`index.html`, `elm.js`).
//!
//! `build.rs` resolves the asset directory into `RUST_TEMPLATE_FRONTEND_DIR`;
//! `rust_embed` reads it at macro-expansion time, embedding the assets in
//! release builds and reading them from disk in debug builds.
#[derive(rust_embed::RustEmbed)]
#[folder = "$RUST_TEMPLATE_FRONTEND_DIR"]
pub struct Frontend;
