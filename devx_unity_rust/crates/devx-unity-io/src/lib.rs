//! Unified Unity container, archive, `SerializedFile`, and TypeTree I/O layer
//! combining `rabex` (embedded AssetRipper TPK & write-back builders) with
//! `unity-asset-binary` (bundles, serialized files, webfiles, TypeTree parsing).

pub mod deps;
pub mod tpk_bridge;
pub mod vfs;
pub mod workspace_io;

pub use deps::{
    DependencyResolutionReport, load_deferred_bundles_with_dependencies,
    resolve_dependencies_for_files,
};
pub use tpk_bridge::{
    RabexEmbeddedTpkRegistry, sanitize_unity_version, shared_tpk_registry, tpk_class_count,
};
pub use unity_asset_binary::object::UnityObject;
pub use unity_asset_core::UnityValue;
pub use vfs::{
    ContainerKind, ExtractBundlesReport, ResourceStreamStore, ScriptArtifact, UnityPackageEntry,
    WorkspaceLoadOptions, extract_raw_bundles, load_deferred_bundle_indices, load_path,
    load_path_with_options,
};
pub use workspace_io::{
    DeferredBundleEntry, LoadedAssetFile, PPtrLink, RawArtifact, UnityWorkspaceData,
    class_name_for, extract_bytes_field, extract_i32_field, extract_i64_field, read_object,
    resolve_object_stream, scan_pptrs, unity_value_to_json,
};

use thiserror::Error;

/// Errors emitted by `devx-unity-io`.
#[derive(Debug, Error)]
pub enum IoError {
    #[error("I/O failure for `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Unity binary parse failure in `{path}`: {message}")]
    UnityParse { path: String, message: String },
    #[error("archive `{path}` extraction failed: {message}")]
    Archive { path: String, message: String },
    #[error("unsupported container format at `{0}`")]
    UnsupportedFormat(String),
}

pub type IoResult<T> = Result<T, IoError>;
