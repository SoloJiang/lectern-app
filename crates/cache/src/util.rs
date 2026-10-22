//! Lock helpers, and what a log record keeps of an error.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Locks a mutex, recovering the data if a panicking thread poisoned it.
///
/// No lock is held while caller code (`Entry::decode`, hooks) runs except the
/// session read lock, whose data a panic leaves consistent.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// What a log record keeps of a rusqlite error (docs/design/cache.md
/// 「日志与诊断」): SQLite's extended result code, or the kind of rusqlite's
/// own error. Never the message, which can quote SQL text and values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SqlError {
    /// The `sqlite` field.
    pub(crate) sqlite: Option<i32>,
    /// The `error` field: set when SQLite did not report the error.
    pub(crate) error: Option<&'static str>,
}

impl SqlError {
    pub(crate) fn of(e: &rusqlite::Error) -> SqlError {
        use rusqlite::Error as E;
        let kind = match e {
            E::SqliteFailure(f, _) => return SqlError::code(f.extended_code),
            E::SqlInputError { error, .. } => return SqlError::code(error.extended_code),
            E::SqliteSingleThreadedMode => "single_threaded_mode",
            E::FromSqlConversionFailure(..) => "from_sql_conversion_failure",
            E::IntegralValueOutOfRange(..) => "integral_value_out_of_range",
            E::Utf8Error(..) => "utf8_error",
            E::NulError(_) => "nul_error",
            E::InvalidParameterName(_) => "invalid_parameter_name",
            E::InvalidPath(_) => "invalid_path",
            E::ExecuteReturnedResults => "execute_returned_results",
            E::QueryReturnedNoRows => "query_returned_no_rows",
            E::QueryReturnedMoreThanOneRow => "query_returned_more_than_one_row",
            E::InvalidColumnIndex(_) => "invalid_column_index",
            E::InvalidColumnName(_) => "invalid_column_name",
            E::InvalidColumnType(..) => "invalid_column_type",
            E::StatementChangedRows(_) => "statement_changed_rows",
            E::ToSqlConversionFailure(_) => "to_sql_conversion_failure",
            E::InvalidQuery => "invalid_query",
            E::MultipleStatement => "multiple_statement",
            E::InvalidParameterCount(..) => "invalid_parameter_count",
            _ => "other",
        };
        SqlError {
            sqlite: None,
            error: Some(kind),
        }
    }

    fn code(extended: i32) -> SqlError {
        SqlError {
            sqlite: Some(extended),
            error: None,
        }
    }
}
