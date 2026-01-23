//! HTTP adapter for catapulte
//!
//! This crate provides the Axum-based HTTP server that exposes the email sending API.

pub mod controller;
pub mod error;
pub mod server;

pub use controller::ApiDoc;
pub use server::{HttpConfig, HttpServer};
