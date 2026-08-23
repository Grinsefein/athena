//! Athena - A Rust-based video download service
//!
//! This crate provides the backend for a video downloading application,
//! with rich error handling and type-safe abstractions.

pub mod errors;
pub mod handlers;
pub mod metadata;
pub mod models;
pub mod state;
pub mod tags;
pub mod ytdlp;

pub use errors::{AppError, AppResult, ErrorResponse};
