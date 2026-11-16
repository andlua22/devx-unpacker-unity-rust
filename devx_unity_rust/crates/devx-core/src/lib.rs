//! Core workspace model, multi-mode asset tree, `PPtr` reference graph,
//! search engine, and non-blocking background job runner.

pub mod catalog;
pub mod jobs;
pub mod tree;

pub use catalog::{AssetCatalog, AssetEntry, ReferenceEdge, ReferenceGraph, SearchHit};
pub use jobs::{JobEvent, JobHandle, JobManager};
pub use tree::{AssetTreeNode, TreeGroupingMode, TreeNodeKind, build_asset_tree};

use thiserror::Error;

/// Errors emitted by `devx-core`.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Io(#[from] devx_unity_io::IoError),
    #[error("asset `{0:?}` not found in catalog")]
    AssetNotFound(devx_plugin_api::AssetRef),
    #[error("job cancelled")]
    Cancelled,
}

pub type CoreResult<T> = Result<T, CoreError>;
