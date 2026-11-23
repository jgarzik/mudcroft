//! Image storage and generation module
//!
//! Provides:
//! - Content-addressed image storage (like code_store)
//! - Room image generation pipeline

mod generate;
mod jobs;
mod store;

pub use generate::{generate_room_image, room_needs_image, store_image_statement};
pub use jobs::{ImageJob, ImageJobs};
pub use store::{ImageData, ImageStore};
