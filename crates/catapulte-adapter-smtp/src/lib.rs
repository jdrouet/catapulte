//! SMTP adapter for catapulte
//!
//! This crate implements the `EmailSender` port using Lettre for SMTP delivery.

mod config;
mod sender;

pub use config::SmtpConfig;
pub use sender::SmtpSender;
