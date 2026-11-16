use crate::host::PluginHostContext;
use crate::types::{
    AssetSummary, DecodedAudio, DecodedImage, ExportArtifact, MaterialPropertyValue,
    ShaderPostProcessInput,
};
use crate::{AssetExporterPlugin, DevXPlugin, ExportPayload, PluginError, PluginResult};
use std::sync::Arc;

/// Thread-safe registry managing all active lifecycle plugins and exporters.
#[derive(Default)]
pub struct PluginRegistry {
    plugins: Vec<Arc<dyn DevXPlugin>>,
    exporters: Vec<Arc<dyn AssetExporterPlugin>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_plugin(&mut self, plugin: Arc<dyn DevXPlugin>) {
        self.plugins.push(plugin);
    }

    pub fn register_exporter(&mut self, exporter: Arc<dyn AssetExporterPlugin>) {
        self.exporters.push(exporter);
    }

    pub fn plugins(&self) -> &[Arc<dyn DevXPlugin>] {
        &self.plugins
    }

    pub fn exporters(&self) -> &[Arc<dyn AssetExporterPlugin>] {
        &self.exporters
    }

    pub fn exporters_for_class(&self, class_id: i32) -> Vec<Arc<dyn AssetExporterPlugin>> {
        self.exporters
            .iter()
            .filter(|e| e.supports_class(class_id))
            .cloned()
            .collect()
    }

    pub fn find_exporter(&self, id: &str) -> Option<Arc<dyn AssetExporterPlugin>> {
        self.exporters.iter().find(|e| e.id() == id).cloned()
    }

    pub fn run_startup(&self, host: &mut dyn PluginHostContext) -> PluginResult<()> {
        for plugin in &self.plugins {
            plugin.on_startup(host)?;
        }
        Ok(())
    }

    pub fn post_process_texture(
        &self,
        asset: &AssetSummary,
        image: &mut DecodedImage,
        host: &mut dyn PluginHostContext,
    ) -> PluginResult<()> {
        for plugin in &self.plugins {
            plugin.on_texture_decoded(asset, image, host)?;
        }
        Ok(())
    }

    pub fn post_process_audio(
        &self,
        asset: &AssetSummary,
        audio: &mut DecodedAudio,
        host: &mut dyn PluginHostContext,
    ) -> PluginResult<()> {
        for plugin in &self.plugins {
            plugin.on_audio_decoded(asset, audio, host)?;
        }
        Ok(())
    }

    pub fn post_process_shader(
        &self,
        input: &ShaderPostProcessInput,
        shader_text: &mut String,
        host: &mut dyn PluginHostContext,
    ) -> PluginResult<()> {
        for plugin in &self.plugins {
            plugin.on_shader_text(input, shader_text, host)?;
        }
        Ok(())
    }

    pub fn post_process_material(
        &self,
        asset: &AssetSummary,
        shader_name: &mut Option<String>,
        properties: &mut Vec<(String, MaterialPropertyValue)>,
        host: &mut dyn PluginHostContext,
    ) -> PluginResult<()> {
        for plugin in &self.plugins {
            plugin.on_material_properties(asset, shader_name, properties, host)?;
        }
        Ok(())
    }

    pub fn export_with(
        &self,
        exporter_id: &str,
        asset: &AssetSummary,
        payload: &ExportPayload<'_>,
        host: &mut dyn PluginHostContext,
    ) -> PluginResult<ExportArtifact> {
        let exporter = self
            .find_exporter(exporter_id)
            .ok_or_else(|| PluginError::ExporterNotFound(exporter_id.to_owned()))?;
        exporter.export(asset, payload, host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::InMemoryHostContext;
    use crate::types::AssetRef;
    use crate::PluginMetadata;

    struct InvertAlphaPlugin;

    impl DevXPlugin for InvertAlphaPlugin {
        fn metadata(&self) -> PluginMetadata {
            PluginMetadata {
                id: "test.invert_alpha".into(),
                name: "Invert Alpha".into(),
                version: "0.1.0".into(),
                description: "Test texture hook".into(),
            }
        }

        fn on_texture_decoded(
            &self,
            _asset: &AssetSummary,
            image: &mut DecodedImage,
            host: &mut dyn PluginHostContext,
        ) -> PluginResult<()> {
            for px in image.rgba8.as_chunks_mut::<4>().0 {
                px[3] = 255 - px[3];
            }
            host.log_info("inverted alpha");
            Ok(())
        }
    }

    #[test]
    fn plugin_registry_runs_texture_post_processor() {
        let mut reg = PluginRegistry::new();
        reg.register_plugin(Arc::new(InvertAlphaPlugin));
        let asset = AssetSummary {
            asset_ref: AssetRef {
                file_index: 0,
                path_id: 42,
            },
            class_id: 28,
            class_name: "Texture2D".into(),
            name: "hero".into(),
            container_path: None,
            source_file: "sharedassets0.assets".into(),
            byte_size: 16,
            unity_version: "2022.3.0f1".into(),
        };
        let mut img = DecodedImage {
            width: 1,
            height: 1,
            rgba8: vec![10, 20, 30, 100],
            format_name: "RGBA32".into(),
            mip_count: 1,
        };
        let mut host = InMemoryHostContext::default();
        reg.post_process_texture(&asset, &mut img, &mut host)
            .expect("hook succeeds");
        assert_eq!(img.rgba8[3], 155);
        assert_eq!(host.logs.len(), 1);
    }
}
