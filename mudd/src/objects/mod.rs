//! Object system - LPC-style objects with class inheritance

mod class;
mod object;
mod path;
mod store;

pub use class::{ClassDef, ClassRegistry};
pub use object::{Object, ObjectId, Properties};
pub use path::{parent_path, path_name, validate_object_path, PathValidationError};
pub use store::{ObjectStore, UniverseInfo};
