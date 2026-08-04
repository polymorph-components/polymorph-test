//! Host-side formats: the inventory lockfile and the canonical results
//! model (#26): WIT-shaped event records, JSONL edge encoding, and the
//! stream→document fold (including the `not-reached` rule).

pub mod aggregate;
pub mod inventory;
pub mod lockfile;
pub mod manifest;
pub mod matrix;
pub mod results;
