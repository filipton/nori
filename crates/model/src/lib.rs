//! Shared types: library records (model.rs), derived display lines (lines.rs), numbers as every client
//! writes them (numbers.rs), `CoreError`, the log (alog.rs) and the heap counter (heap.rs).

pub mod alog;
pub mod heap;
pub mod lines;
pub mod model;
pub mod numbers;

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
    /// A smart playlist definition that does not read: where in its JSON (`match.rules[0].value`), and what.
    #[error("smart playlist {path}: {problem:?}")]
    Smart { path: String, problem: SmartProblem },
}

/// What is wrong with a smart playlist definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum SmartProblem {
    NotJson,
    NotObject,
    UnknownKey,
    /// Neither a rule `{field, op, value}` nor a group `{all, rules}`.
    NotRuleOrGroup,
    /// A single rule where a group belongs.
    SingleRule,
    TooDeep,
    NotFlag,
    NotList,
    NoField,
    UnknownField,
    NoOperator,
    /// An operator that is unknown or does not apply to the field.
    WrongOperator,
    NoValue,
    TakesNoValue,
    NotNumber,
    NotDate,
    NotText,
    DaysOutOfRange,
    Backwards,
    CannotSort,
    Negative,
}

impl From<rusqlite::Error> for CoreError {
    fn from(e: rusqlite::Error) -> Self {
        CoreError::Db { reason: e.to_string() }
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;
