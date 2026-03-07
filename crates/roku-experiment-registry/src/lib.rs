//! Experiment lifecycle tracking and persistence.

mod registry;
mod repository;

pub use registry::ExperimentRegistry;
pub use repository::{
	ExperimentRegistryError, ExperimentRunRepository, FileExperimentRunRepository,
	InMemoryExperimentRunRepository,
};
