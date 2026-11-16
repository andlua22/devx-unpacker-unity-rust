use serde::{Deserialize, Serialize};

/// Stable reference to a Unity object inside a container/file catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssetRef {
    pub file_index: usize,
    pub path_id: i64,
}

/// High-level immutable descriptor passed to plugins and exporters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetSummary {
    pub asset_ref: AssetRef,
    pub class_id: i32,
    pub class_name: String,
    pub name: String,
    pub container_path: Option<String>,
    pub source_file: String,
    pub byte_size: u32,
    pub unity_version: String,
}

/// Decoded RGBA8 image buffer.
#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
    pub format_name: String,
    pub mip_count: u32,
}

/// Decoded PCM audio buffer.
#[derive(Debug, Clone)]
pub struct DecodedAudio {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples_i16: Vec<i16>,
    pub source_format: String,
    pub duration_secs: f32,
    pub raw_container: Option<(String, Vec<u8>)>,
}

/// Contiguous triangle range inside a decoded mesh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubMeshSlice {
    pub first_index: u32,
    pub index_count: u32,
    pub topology: i32,
}

/// Normalized triangle mesh ready for viewport rendering or OBJ/STL/GLB export.
#[derive(Debug, Clone)]
pub struct DecodedMesh {
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub submeshes: Vec<SubMeshSlice>,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
}

/// Context passed to shader post-processors.
#[derive(Debug, Clone)]
pub struct ShaderPostProcessInput {
    pub asset: AssetSummary,
    pub stage: ShaderPostProcessStage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderPostProcessStage {
    BeforeDecompile,
    AfterDecompile,
}

/// Generic material property value exposed to post-processors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MaterialPropertyValue {
    Float(f32),
    Color([f32; 4]),
    Vector([f32; 4]),
    TextureName(String),
}

/// Output artifact produced by an [`crate::AssetExporterPlugin`].
#[derive(Debug, Clone)]
pub struct ExportArtifact {
    pub relative_path: String,
    pub bytes: Vec<u8>,
    pub mime: String,
}
