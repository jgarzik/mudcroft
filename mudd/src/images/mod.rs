//! Image storage and generation module
//!
//! Provides:
//! - Content-addressed image storage (like code_store)
//! - Room image generation pipeline

mod gen;
mod store;

pub use gen::generate_room_image;
pub use store::{ImageData, ImageStore};
