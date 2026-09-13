//! Durable and in-memory worker journal services.
//! Both backends share state-machine validation; SQLite keeps pending events
//! durable until the Hub ACK, without decoding dispatch JSON on event writes.
mod error;
mod memory;
mod sqlite;
mod state;

pub use error::JournalError;
pub use memory::MemoryJournal;
pub use sqlite::SqliteJournal;

#[cfg(test)]
mod tests;
