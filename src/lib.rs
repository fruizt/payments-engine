//! A small payments engine: reads transactions from CSV, applies them to client accounts one at
//! a time, and writes the final state of every account as CSV.
//!
//! The design, and the assumptions (A1-A14) cited throughout the code, are in the README.

pub mod datastore;
pub mod domain;
pub mod engine;
pub mod error;
pub mod io;
pub mod metrics;
pub mod transaction;
