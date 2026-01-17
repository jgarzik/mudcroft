//! Object system - LPC-style objects with class inheritance

mod class;
mod object;
mod store;

pub use class::{ClassDef, ClassRegistry};
pub use object::{Object, ObjectId, Properties};
pub use store::ObjectStore;
