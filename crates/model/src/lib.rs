//! Shared types: library records (model.rs), derived display lines (lines.rs), `CoreError`, the log
//! (alog.rs) and the heap counter (heap.rs).

pub mod alog;
pub mod heap;
pub mod lines;
pub mod model;

pub use model::*;

// Exported with Display: uniffi's Kotlin exception has no message otherwise.
#[derive(Debug, thiserror::Error)]
#[cfg_attr(feature = "ffi", derive(uniffi::Error), uniffi::export(Display))]
pub enum CoreError {
    #[error("{reason}")]
    Api { code: i32, reason: String },
    #[error("bad response: {reason}")]
    Parse { reason: String },
    #[error("database: {reason}")]
    Db { reason: String },
}

impl From<rusqlite::Error> for CoreError {
    fn from(e: rusqlite::Error) -> Self {
        CoreError::Db { reason: e.to_string() }
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;
