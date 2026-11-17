//! Application-level capture planning, independent of rendering and execution.
pub mod admission;
pub mod baseline;
pub mod cadence_alignment;
pub mod dataset;
pub mod deadlines;
pub mod geometry;
pub mod job_grouper;
pub mod parallel;
pub mod resources;
pub mod satisfiability;
pub mod scheduling;
pub mod session;
mod spatial;
pub mod storage;
#[cfg(test)]
mod tests;
mod validation;
pub mod workload;
