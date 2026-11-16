//! Plugin trait API for `devx_unity_rust`, cleanly modeling DevX's `DevXC` and
//! `DevXCControl` extension hooks in safe Rust.

pub mod host;
pub mod registry;
pub mod types;

pub use host::{InMemoryHostContext, LogEntry, LogLevel, PluginHostContext};
pub use registry::PluginRegistry;
pub use types::{
    AssetRef, AssetSummary, DecodedAudio, DecodedImage, DecodedMesh, ExportArtifact,
    MaterialPropertyValue, ShaderPostProcessInput, ShaderPostProcessStage, SubMeshSlice,
};

use thiserror::Error;

/// Errors returned by plugin hooks and exporters.
#[derive(Debug, Error)]
pub enum PluginError {
    #[error("unsupported asset class `{class_name}` (id {class_id}) for plugin `{plugin}`")]
    UnsupportedAsset {
        plugin: String,
        class_id: i32,
        class_name: String,
    },
    #[error("plugin `{plugin}` failed: {message}")]
    Execution { plugin: String, message: String },
    #[error("exporter `{0}` not found")]
    ExporterNotFound(String),
}

pub type PluginResult<T> = Result<T, PluginError>;

/// Metadata describing a registered plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMetadata {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
}

/// Core extension trait mirroring DevX `DevXC` hooks (`Startup`, `ConvertTexture`,
/// `AutioConvertor`, `ShaderPostProcessor`, `MaterialPostProcessor`).
pub trait DevXPlugin: Send + Sync {
    /// Plugin identity metadata.
    fn metadata(&self) -> PluginMetadata;

    /// Called once when the application/workspace boots (`Startup.devxc`).
    fn on_startup(&self, _host: &mut dyn PluginHostContext) -> PluginResult<()> {
        Ok(())
    }

    /// Post-processes decoded RGBA8 image data (`ConvertTexture.devxc`).
    fn on_texture_decoded(
        &self,
        _asset: &AssetSummary,
        _image: &mut DecodedImage,
        _host: &mut dyn PluginHostContext,
    ) -> PluginResult<()> {
        Ok(())
    }

    /// Post-processes decoded PCM audio samples (`AutioConvertor.devxc`).
    fn on_audio_decoded(
        &self,
        _asset: &AssetSummary,
        _audio: &mut DecodedAudio,
        _host: &mut dyn PluginHostContext,
    ) -> PluginResult<()> {
        Ok(())
    }

    /// Post-processes shader source text (`ShaderPostProcessor.devxc`).
    fn on_shader_text(
        &self,
        _input: &ShaderPostProcessInput,
        _shader_text: &mut String,
        _host: &mut dyn PluginHostContext,
    ) -> PluginResult<()> {
        Ok(())
    }

    /// Customizes material shader/property output (`MaterialPostProcessor.devxc`).
    fn on_material_properties(
        &self,
        _asset: &AssetSummary,
        _shader_name: &mut Option<String>,
        _properties: &mut Vec<(String, MaterialPropertyValue)>,
        _host: &mut dyn PluginHostContext,
    ) -> PluginResult<()> {
        Ok(())
    }
}

/// Export handler registered by plugins (PNG, DDS, OBJ, STL, GLB, WAV, JSON, etc.).
pub trait AssetExporterPlugin: Send + Sync {
    /// Unique exporter identifier (e.g. `"builtin.png"`).
    fn id(&self) -> &str;

    /// Human-readable export format label (e.g. `"PNG Image (.png)"`).
    fn label(&self) -> &str;

    /// Primary file extension without leading dot.
    fn extension(&self) -> &str;

    /// Returns true if this exporter supports the given Unity `ClassID`.
    fn supports_class(&self, class_id: i32) -> bool;

    /// Generates exported bytes from an asset payload.
    fn export(
        &self,
        asset: &AssetSummary,
        payload: &ExportPayload<'_>,
        host: &mut dyn PluginHostContext,
    ) -> PluginResult<ExportArtifact>;
}

/// Borrowed decoded or raw payload handed to an [`AssetExporterPlugin`].
#[derive(Debug)]
pub enum ExportPayload<'a> {
    Image(&'a DecodedImage),
    Audio(&'a DecodedAudio),
    Mesh(&'a DecodedMesh),
    Text(&'a str),
    Raw(&'a [u8]),
    Json(&'a serde_json::Value),
}
