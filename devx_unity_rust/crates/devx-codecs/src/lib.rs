//! Pure-Rust codecs and geometry/shader/audio/texture decoders for Unity assets.

pub mod animation;
pub mod animator;
pub mod audio;
pub mod collada;
pub mod fbx;
pub mod gltf_scene;
pub mod material;
pub mod mesh;
pub mod particle;
pub mod scene_extract;
pub mod shader;
pub mod smolv_spirv;
pub mod terrain;
pub mod text_font;
pub mod texture;

use thiserror::Error;

/// Unified error type for codec parsing, decoding, and format encoding.
#[derive(Debug, Error)]
pub enum CodecError {
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("unsupported terrain payload: {0}")]
    UnsupportedTerrain(String),

    #[error("unsupported particle system payload: {0}")]
    UnsupportedParticle(String),

    #[error("missing external resource stream: {0}")]
    MissingStream(String),

    #[error("malformed asset payload: {0}")]
    MalformedData(String),

    #[error("binary parse error: {0}")]
    Binary(#[from] unity_asset_binary::BinaryError),

    #[error("image encode error: {0}")]
    Image(#[from] image::ImageError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type CodecResult<T> = Result<T, CodecError>;
