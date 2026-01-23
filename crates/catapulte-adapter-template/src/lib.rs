//! Template adapter for catapulte
//!
//! This crate implements the `TemplateLoader` and `TemplateRenderer` ports
//! using local filesystem, HTTP, and mrml for MJML rendering.

pub mod loader;
pub mod renderer;

pub use loader::{HttpLoader, HttpLoaderConfig, LocalLoader, LocalLoaderConfig, MultiLoader};
pub use renderer::{MrmlRenderer, MrmlRendererConfig};
