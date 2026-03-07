//! Validation pipeline for child results.

mod config;
mod cross_check;
mod pipeline;
mod policy;
mod provenance;
mod schema;
mod semantic;

pub use config::ValidationConfig;
pub use pipeline::ValidationPipeline;
