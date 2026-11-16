//! Built-in exporter & post-processor plugins registered by default in `devx_unity_rust`.

use devx_codecs::audio::encode_wav;
use devx_codecs::collada::export_mesh_to_dae;
use devx_codecs::fbx::export_mesh_to_fbx;
use devx_codecs::mesh::{export_glb, export_obj, export_ply, export_stl_binary};
use devx_codecs::texture::{encode_bmp, encode_dds, encode_jpeg, encode_png, encode_tga, encode_webp};
use devx_codecs::CodecResult;
use devx_plugin_api::{
    AssetExporterPlugin, AssetSummary, DecodedImage, DecodedMesh, DevXPlugin, ExportArtifact,
    ExportPayload, PluginError, PluginHostContext, PluginMetadata, PluginRegistry, PluginResult,
};
use std::sync::Arc;

/// Core built-in lifecycle plugin (`Startup` hook).
pub struct BuiltinCorePlugin;

impl DevXPlugin for BuiltinCorePlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: "devx.builtin.core".to_owned(),
            name: "DevX Built-in Core".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            description: "Default lifecycle hooks and standard exporters".to_owned(),
        }
    }

    fn on_startup(&self, host: &mut dyn PluginHostContext) -> PluginResult<()> {
        host.log_info("DevX Built-in Plugin Suite initialized");
        Ok(())
    }
}

/// Builds a [`PluginRegistry`] pre-populated with all built-in plugins & exporters.
pub fn default_plugin_registry() -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    registry.register_plugin(Arc::new(BuiltinCorePlugin));
    registry.register_exporter(Arc::new(PngExporter));
    registry.register_exporter(Arc::new(JpgExporter));
    registry.register_exporter(Arc::new(BmpExporter));
    registry.register_exporter(Arc::new(WebpExporter));
    registry.register_exporter(Arc::new(TgaExporter));
    registry.register_exporter(Arc::new(DdsExporter));
    registry.register_exporter(Arc::new(WavExporter));
    registry.register_exporter(Arc::new(AudioNativeExporter));
    registry.register_exporter(Arc::new(ObjExporter));
    registry.register_exporter(Arc::new(FbxExporter));
    registry.register_exporter(Arc::new(DaeExporter));
    registry.register_exporter(Arc::new(PlyExporter));
    registry.register_exporter(Arc::new(StlExporter));
    registry.register_exporter(Arc::new(GlbExporter));
    registry.register_exporter(Arc::new(JsonExporter));
    registry.register_exporter(Arc::new(RawExporter));
    registry
}

fn sanitize_stem(asset: &AssetSummary) -> String {
    let clean: String = asset
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.is_empty() {
        format!("{}_{}", asset.class_name, asset.asset_ref.path_id)
    } else {
        clean
    }
}

fn unsupported(plugin: &str, asset: &AssetSummary) -> PluginError {
    PluginError::UnsupportedAsset {
        plugin: plugin.to_owned(),
        class_id: asset.class_id,
        class_name: asset.class_name.clone(),
    }
}

fn export_image_with(
    plugin_id: &str,
    ext: &str,
    mime: &str,
    asset: &AssetSummary,
    payload: &ExportPayload<'_>,
    encoder: impl FnOnce(&DecodedImage) -> CodecResult<Vec<u8>>,
) -> PluginResult<ExportArtifact> {
    let ExportPayload::Image(img) = payload else {
        return Err(unsupported(plugin_id, asset));
    };
    let bytes = encoder(img).map_err(|e| PluginError::Execution {
        plugin: plugin_id.to_owned(),
        message: e.to_string(),
    })?;
    Ok(ExportArtifact {
        relative_path: format!("{}.{ext}", sanitize_stem(asset)),
        bytes,
        mime: mime.to_owned(),
    })
}

fn export_mesh_with(
    plugin_id: &str,
    ext: &str,
    mime: &str,
    asset: &AssetSummary,
    payload: &ExportPayload<'_>,
    encoder: impl FnOnce(&DecodedMesh) -> CodecResult<Vec<u8>>,
) -> PluginResult<ExportArtifact> {
    let ExportPayload::Mesh(mesh) = payload else {
        return Err(unsupported(plugin_id, asset));
    };
    let bytes = encoder(mesh).map_err(|e| PluginError::Execution {
        plugin: plugin_id.to_owned(),
        message: e.to_string(),
    })?;
    Ok(ExportArtifact {
        relative_path: format!("{}.{ext}", sanitize_stem(asset)),
        bytes,
        mime: mime.to_owned(),
    })
}

macro_rules! impl_image_exporter {
    ($doc:literal, $name:ident, $id:literal, $label:literal, $ext:literal, $mime:literal, $enc:expr) => {
        #[doc = $doc]
        pub struct $name;
        impl AssetExporterPlugin for $name {
            fn id(&self) -> &str {
                $id
            }
            fn label(&self) -> &str {
                $label
            }
            fn extension(&self) -> &str {
                $ext
            }
            fn supports_class(&self, class_id: i32) -> bool {
                matches!(class_id, 28 | 213)
            }
            fn export(
                &self,
                asset: &AssetSummary,
                payload: &ExportPayload<'_>,
                _host: &mut dyn PluginHostContext,
            ) -> PluginResult<ExportArtifact> {
                export_image_with(self.id(), $ext, $mime, asset, payload, $enc)
            }
        }
    };
}

macro_rules! impl_mesh_exporter {
    ($doc:literal, $name:ident, $id:literal, $label:literal, $ext:literal, $mime:literal, $enc:expr) => {
        #[doc = $doc]
        pub struct $name;
        impl AssetExporterPlugin for $name {
            fn id(&self) -> &str {
                $id
            }
            fn label(&self) -> &str {
                $label
            }
            fn extension(&self) -> &str {
                $ext
            }
            fn supports_class(&self, class_id: i32) -> bool {
                class_id == 43
            }
            fn export(
                &self,
                asset: &AssetSummary,
                payload: &ExportPayload<'_>,
                _host: &mut dyn PluginHostContext,
            ) -> PluginResult<ExportArtifact> {
                export_mesh_with(self.id(), $ext, $mime, asset, payload, $enc)
            }
        }
    };
}

impl_image_exporter!(
    "Exports `Texture2D` / `Sprite` assets to `.png`.",
    PngExporter,
    "builtin.png",
    "PNG Image (.png)",
    "png",
    "image/png",
    encode_png
);
impl_image_exporter!(
    "Exports `Texture2D` / `Sprite` assets to `.jpg`.",
    JpgExporter,
    "builtin.jpg",
    "JPEG Image (.jpg)",
    "jpg",
    "image/jpeg",
    |img| encode_jpeg(img, 92)
);
impl_image_exporter!(
    "Exports `Texture2D` / `Sprite` assets to `.bmp`.",
    BmpExporter,
    "builtin.bmp",
    "BMP Image (.bmp)",
    "bmp",
    "image/bmp",
    encode_bmp
);
impl_image_exporter!(
    "Exports `Texture2D` / `Sprite` assets to `.webp`.",
    WebpExporter,
    "builtin.webp",
    "WebP Image (.webp)",
    "webp",
    "image/webp",
    encode_webp
);
impl_image_exporter!(
    "Exports `Texture2D` / `Sprite` assets to `.tga`.",
    TgaExporter,
    "builtin.tga",
    "TGA Image (.tga)",
    "tga",
    "image/x-tga",
    encode_tga
);
impl_image_exporter!(
    "Exports `Texture2D` / `Sprite` assets to uncompressed BGRA `.dds`.",
    DdsExporter,
    "builtin.dds",
    "DirectDraw Surface (.dds)",
    "dds",
    "image/vnd-ms.dds",
    |img| encode_dds(None, img)
);

/// Exports decoded `AudioClip` assets as PCM `.wav`.
pub struct WavExporter;
impl AssetExporterPlugin for WavExporter {
    fn id(&self) -> &str {
        "builtin.wav"
    }
    fn label(&self) -> &str {
        "WAV Audio (.wav)"
    }
    fn extension(&self) -> &str {
        "wav"
    }
    fn supports_class(&self, class_id: i32) -> bool {
        class_id == 83
    }
    fn export(
        &self,
        asset: &AssetSummary,
        payload: &ExportPayload<'_>,
        _host: &mut dyn PluginHostContext,
    ) -> PluginResult<ExportArtifact> {
        let ExportPayload::Audio(audio) = payload else {
            return Err(unsupported(self.id(), asset));
        };
        let bytes = encode_wav(audio).map_err(|e| PluginError::Execution {
            plugin: self.id().to_owned(),
            message: e.to_string(),
        })?;
        Ok(ExportArtifact {
            relative_path: format!("{}.wav", sanitize_stem(asset)),
            bytes,
            mime: "audio/wav".to_owned(),
        })
    }
}

/// Exports `AudioClip` in its unpacked native container (`.ogg`/`.mp3`/`.wav`) or WAV fallback.
pub struct AudioNativeExporter;
impl AssetExporterPlugin for AudioNativeExporter {
    fn id(&self) -> &str {
        "builtin.audio_native"
    }
    fn label(&self) -> &str {
        "Original Audio Stream (.ogg/.wav/.mp3)"
    }
    fn extension(&self) -> &str {
        "ogg"
    }
    fn supports_class(&self, class_id: i32) -> bool {
        class_id == 83
    }
    fn export(
        &self,
        asset: &AssetSummary,
        payload: &ExportPayload<'_>,
        _host: &mut dyn PluginHostContext,
    ) -> PluginResult<ExportArtifact> {
        let ExportPayload::Audio(audio) = payload else {
            return Err(unsupported(self.id(), asset));
        };
        if let Some((ext, bytes)) = &audio.raw_container {
            return Ok(ExportArtifact {
                relative_path: format!("{}.{}", sanitize_stem(asset), ext),
                bytes: bytes.clone(),
                mime: format!("audio/{ext}"),
            });
        }
        WavExporter.export(asset, payload, _host)
    }
}

impl_mesh_exporter!(
    "Exports `Mesh` assets as Wavefront `.obj`.",
    ObjExporter,
    "builtin.obj",
    "Wavefront OBJ (.obj)",
    "obj",
    "text/plain",
    |m| Ok(export_obj(m).into_bytes())
);
impl_mesh_exporter!(
    "Exports `Mesh` assets as Autodesk Binary `.fbx` (FBX 7.4).",
    FbxExporter,
    "builtin.fbx",
    "Autodesk FBX 7.4 Binary (.fbx)",
    "fbx",
    "application/octet-stream",
    export_mesh_to_fbx
);
impl_mesh_exporter!(
    "Exports `Mesh` assets as COLLADA 1.4.1 `.dae`.",
    DaeExporter,
    "builtin.dae",
    "COLLADA XML (.dae)",
    "dae",
    "model/vnd.collada+xml",
    |m| export_mesh_to_dae(m).map(String::into_bytes)
);
impl_mesh_exporter!(
    "Exports `Mesh` assets as ASCII Stanford Polygon `.ply`.",
    PlyExporter,
    "builtin.ply",
    "Stanford PLY (.ply)",
    "ply",
    "text/plain",
    |m| Ok(export_ply(m).into_bytes())
);
impl_mesh_exporter!(
    "Exports `Mesh` assets as binary `.stl`.",
    StlExporter,
    "builtin.stl",
    "Binary STL (.stl)",
    "stl",
    "model/stl",
    |m| Ok(export_stl_binary(m))
);
impl_mesh_exporter!(
    "Exports `Mesh` assets as glTF 2.0 binary `.glb`.",
    GlbExporter,
    "builtin.glb",
    "glTF 2.0 Binary (.glb)",
    "glb",
    "model/gltf-binary",
    export_glb
);

/// Exports TypeTree properties as pretty JSON.
pub struct JsonExporter;
impl AssetExporterPlugin for JsonExporter {
    fn id(&self) -> &str {
        "builtin.json"
    }
    fn label(&self) -> &str {
        "TypeTree JSON (.json)"
    }
    fn extension(&self) -> &str {
        "json"
    }
    fn supports_class(&self, _class_id: i32) -> bool {
        true
    }
    fn export(
        &self,
        asset: &AssetSummary,
        payload: &ExportPayload<'_>,
        _host: &mut dyn PluginHostContext,
    ) -> PluginResult<ExportArtifact> {
        let ExportPayload::Json(val) = payload else {
            return Err(unsupported(self.id(), asset));
        };
        let bytes = serde_json::to_vec_pretty(val).map_err(|e| PluginError::Execution {
            plugin: self.id().to_owned(),
            message: e.to_string(),
        })?;
        Ok(ExportArtifact {
            relative_path: format!("{}.json", sanitize_stem(asset)),
            bytes,
            mime: "application/json".to_owned(),
        })
    }
}

/// Exports raw serialized object bytes.
pub struct RawExporter;
impl AssetExporterPlugin for RawExporter {
    fn id(&self) -> &str {
        "builtin.raw"
    }
    fn label(&self) -> &str {
        "Raw Object Bytes (.bin)"
    }
    fn extension(&self) -> &str {
        "bin"
    }
    fn supports_class(&self, _class_id: i32) -> bool {
        true
    }
    fn export(
        &self,
        asset: &AssetSummary,
        payload: &ExportPayload<'_>,
        _host: &mut dyn PluginHostContext,
    ) -> PluginResult<ExportArtifact> {
        let ExportPayload::Raw(bytes) = payload else {
            return Err(unsupported(self.id(), asset));
        };
        Ok(ExportArtifact {
            relative_path: format!("{}_{}.bin", sanitize_stem(asset), asset.asset_ref.path_id),
            bytes: bytes.to_vec(),
            mime: "application/octet-stream".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devx_plugin_api::{AssetRef, InMemoryHostContext};

    #[test]
    fn default_registry_runs_startup_and_exporters() {
        let reg = default_plugin_registry();
        let mut host = InMemoryHostContext::default();
        reg.run_startup(&mut host).expect("startup ok");
        assert!(!host.logs.is_empty());
        assert!(reg.find_exporter("builtin.png").is_some());
        assert!(reg.find_exporter("builtin.glb").is_some());

        let asset = AssetSummary {
            asset_ref: AssetRef {
                file_index: 0,
                path_id: 42,
            },
            class_id: 49,
            class_name: "TextAsset".into(),
            name: "test".into(),
            container_path: None,
            source_file: "resources.assets".into(),
            byte_size: 4,
            unity_version: "2022.3.0f1".into(),
        };
        let art = reg
            .export_with("builtin.raw", &asset, &ExportPayload::Raw(b"ABCD"), &mut host)
            .expect("raw export ok");
        assert_eq!(art.bytes, b"ABCD");

        let tex_asset = AssetSummary {
            asset_ref: AssetRef {
                file_index: 0,
                path_id: 43,
            },
            class_id: 28,
            class_name: "Texture2D".into(),
            name: "tex".into(),
            container_path: None,
            source_file: "resources.assets".into(),
            byte_size: 16,
            unity_version: "2022.3.0f1".into(),
        };
        let img = DecodedImage {
            width: 2,
            height: 2,
            rgba8: vec![255; 16],
            format_name: "RGBA32".into(),
            mip_count: 1,
        };
        for (id, magic) in [
            ("builtin.jpg", &[0xFFu8, 0xD8, 0xFF][..]),
            ("builtin.bmp", b"BM"),
            ("builtin.webp", b"RIFF"),
        ] {
            let exported = reg
                .export_with(id, &tex_asset, &ExportPayload::Image(&img), &mut host)
                .expect(id);
            assert!(exported.bytes.starts_with(magic), "{id} header mismatch");
        }

        let mesh_asset = AssetSummary {
            asset_ref: AssetRef {
                file_index: 0,
                path_id: 44,
            },
            class_id: 43,
            class_name: "Mesh".into(),
            name: "tri".into(),
            container_path: None,
            source_file: "resources.assets".into(),
            byte_size: 36,
            unity_version: "2022.3.0f1".into(),
        };
        let mesh = DecodedMesh {
            name: "tri".into(),
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            indices: vec![0, 1, 2],
            submeshes: vec![devx_plugin_api::SubMeshSlice {
                first_index: 0,
                index_count: 3,
                topology: 0,
            }],
            bounds_min: [0.0; 3],
            bounds_max: [1.0, 1.0, 0.0],
        };
        let fbx = reg
            .export_with("builtin.fbx", &mesh_asset, &ExportPayload::Mesh(&mesh), &mut host)
            .expect("fbx");
        assert!(fbx.bytes.starts_with(b"Kaydara FBX Binary  \0"));
        let dae = reg
            .export_with("builtin.dae", &mesh_asset, &ExportPayload::Mesh(&mesh), &mut host)
            .expect("dae");
        assert!(String::from_utf8_lossy(&dae.bytes).contains("<COLLADA "));
        let ply = reg
            .export_with("builtin.ply", &mesh_asset, &ExportPayload::Mesh(&mesh), &mut host)
            .expect("ply");
        assert!(String::from_utf8_lossy(&ply.bytes).starts_with("ply\n"));
    }
}
