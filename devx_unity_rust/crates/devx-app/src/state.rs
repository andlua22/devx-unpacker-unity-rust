//! Shared workbench state, preview caches, tree flattening, and background job actions.

use crate::audio_player::AudioPlaybackEngine;
use crate::gl_viewport::GlViewportHandle;
use crate::scene_preview::{
    HierarchyPreviewData, apply_scene_animation_pose, attach_controller_to_preview,
    build_hierarchy_preview, build_particle_preview, build_terrain_preview,
    step_and_apply_animator_sim,
};
use crossbeam::channel::{self, Receiver, Sender};
use devx_codecs::animation::{DecodedAnimationClip, decode_animation_clip};
use devx_codecs::animator::{DecodedAnimatorController, decode_animator_controller};
use devx_codecs::audio::{DecodedAudioClip, decode_audio_clip};
use devx_codecs::gltf_scene::GltfSceneOptions;
use devx_codecs::material::{
    DecodedCubemap, DecodedMaterial, MaterialTextureSlot, decode_cubemap, decode_material,
};
use devx_codecs::mesh::{DecodedBlendShape, decode_mesh_full};
use devx_codecs::particle::{DecodedParticleSystem, decode_particle_system};
use devx_codecs::scene_extract::resolve_scene_root_transforms;
use devx_codecs::shader::{DecodedShader, decode_shader};
use devx_codecs::terrain::{DecodedTerrainData, decode_terrain_data};
use devx_codecs::text_font::{DecodedFont, DecodedTextAsset, decode_font, decode_text_asset};
use devx_codecs::texture::{
    DecodedSprite, TextureDecodeOptions, decode_sprite, decode_sprite_cached, decode_texture2d,
    decode_texture2d_with_plugins, inspect_texture2d_raw,
};
use devx_core::catalog::SearchHit;
use devx_core::tree::{
    AssetTreeNode, TreeFilter, TreeGroupingMode, TreeNodeKind, build_asset_tree,
};
use devx_core::{AssetCatalog, JobEvent, JobHandle, JobManager, ReferenceGraph};
use devx_exporter::bevy_export::{BevyExportOptions, export_bevy_project};
use devx_exporter::hierarchy::{
    HierarchyExportFormat, collect_loaded_animation_clips, export_all_hierarchies_with_format,
    export_hierarchy_with_format,
};
use devx_exporter::prefab::{
    PrefabExportOptions, export_prefab_to_dir, export_prefab_unitypackage,
};
use devx_exporter::repack::{
    BundleFileEntry, CompressionType, replace_object_payload_in_bundle,
    replace_object_payload_in_serialized_file, write_unityfs_bundle,
};
use devx_exporter::prefab::resolve_pptr_ref;
use devx_exporter::ui_canvas::{
    DEFAULT_MAX_UI_NODES, ExtractedUiCanvas, UiCanvasDocument, extract_ui_canvas,
    recompute_ui_layout,
};
use devx_exporter::unity_project::{UnityProjectExportOptions, export_unity_project};
use devx_exporter::{
    ExportAssetOptions, export_batch_directory, export_unitypackage_from_catalog,
    serialize_object_yaml,
};
use devx_plugin_api::{
    AssetRef, DecodedImage, DecodedMesh, InMemoryHostContext, PluginHostContext, PluginRegistry,
};
use devx_plugins_builtin::default_plugin_registry;
use devx_scripts::ScriptsIndex;
use devx_scripts::il2cpp::{
    DisassembledMethodReport, Il2CppDisasmOptions, Il2CppDumpReport, Il2CppExportConfig,
    Il2CppMethodEntry,
};
use devx_unity_io::{
    UnityObject, UnityValue, UnityWorkspaceData, extract_i32_field, extract_i64_field,
    read_object, shared_tpk_registry, tpk_class_count, unity_value_to_json,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Dockable workbench panes managed by `egui_tiles`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DockPane {
    AssetTree,
    Preview,
    Inspector,
    BottomTools,
}

impl DockPane {
    pub fn title(self) -> &'static str {
        match self {
            Self::AssetTree => "Asset Explorer",
            Self::Preview => "Preview & Viewport",
            Self::Inspector => "Object Inspector",
            Self::BottomTools => "Search / Batch / Scripts / Console",
        }
    }
}

/// Active mode selector inside the central Preview pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewTab {
    Auto,
    Overview,
    Media,
    YamlJson,
    HexRaw,
}

impl PreviewTab {
    pub const ALL: [Self; 5] = [
        Self::Auto,
        Self::Overview,
        Self::Media,
        Self::YamlJson,
        Self::HexRaw,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Overview => "Overview",
            Self::Media => "Media / 3D",
            Self::YamlJson => "YAML / JSON",
            Self::HexRaw => "Hex RAW",
        }
    }
}

/// Shader decompiler tab mode inside the Preview pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShaderViewMode {
    #[default]
    DecompiledCg,
    ShaderLabMeta,
    SubPrograms,
}

/// Sub-view selector inside the bottom IL2CPP Scripts inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Il2CppViewTab {
    #[default]
    TypeSource,
    Disassembler,
}

/// Cache key for formatted IL2CPP type source + optional native disassembly / pseudo-C.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Il2CppSourceCacheKey {
    pub type_def_index: usize,
    pub include_disassembly: bool,
    pub include_pseudo_c: bool,
    pub disasm: Il2CppDisasmOptions,
}

/// Cached rendered output for the currently selected IL2CPP type.
#[derive(Debug, Clone)]
pub struct Il2CppSourceCache {
    pub key: Il2CppSourceCacheKey,
    pub text: String,
}

/// Active tab inside the bottom workbench pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BottomTab {
    DeepSearch,
    BatchExport,
    Scripts,
    PluginsConsole,
}

impl BottomTab {
    pub const ALL: [Self; 4] = [
        Self::DeepSearch,
        Self::BatchExport,
        Self::Scripts,
        Self::PluginsConsole,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::DeepSearch => "Deep Search",
            Self::BatchExport => "Batch Export & Pack",
            Self::Scripts => "Scripts & IL2CPP",
            Self::PluginsConsole => "Plugins & Console",
        }
    }
}

/// Scope for batch export actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchScope {
    AllAssets,
    FilteredTree,
    SelectedClass,
}

/// Interactive camera and shading state for the hardware GL / software CPU 3D mesh viewport.
#[derive(Clone)]
pub struct MeshViewportState {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub pan: egui::Vec2,
    pub shaded: bool,
    pub textured: bool,
    pub wireframe: bool,
    pub show_bones: bool,
    pub backface_cull: bool,
    pub show_axes: bool,
    pub use_gpu: bool,
    pub gpu: GlViewportHandle,
}

impl Default for MeshViewportState {
    fn default() -> Self {
        Self::reset()
    }
}

impl MeshViewportState {
    pub fn reset() -> Self {
        Self {
            yaw: 0.65,
            pitch: 0.35,
            distance: 2.6,
            pan: egui::Vec2::ZERO,
            shaded: true,
            textured: true,
            wireframe: false,
            show_bones: true,
            backface_cull: false,
            show_axes: true,
            use_gpu: true,
            gpu: GlViewportHandle::default(),
        }
    }

    /// Resets orbit/shading toggles while preserving `use_gpu` and allocated GL context handles.
    pub fn reset_camera(&mut self) {
        let mut next = Self::reset();
        next.use_gpu = self.use_gpu;
        next.gpu = self.gpu.clone();
        *self = next;
    }
}

/// Resolved texture slot preview attached to a `Material`.
#[derive(Clone)]
pub struct MaterialTexturePreview {
    pub slot: MaterialTextureSlot,
    pub asset_ref: Option<AssetRef>,
    pub texture_name: String,
    pub image: Option<DecodedImage>,
    pub texture: Option<egui::TextureHandle>,
}

/// Decoded packed sprite thumbnail inside a `SpriteAtlas` preview.
#[derive(Clone)]
pub struct SpriteAtlasItemPreview {
    pub name: String,
    pub asset_ref: AssetRef,
    pub image: Option<DecodedImage>,
    pub texture: Option<egui::TextureHandle>,
}

/// Decoded atlas master page preview inside a `SpriteAtlas`.
#[derive(Clone)]
pub struct SpriteAtlasPagePreview {
    pub name: String,
    pub asset_ref: Option<AssetRef>,
    pub image: DecodedImage,
    pub texture: Option<egui::TextureHandle>,
}

/// Editable UI parameters for Bevy 0.20 zero-logic game project generation.
#[derive(Debug, Clone)]
pub struct BevyExportUiState {
    pub project_name: String,
    pub scene_filters: String,
    pub char_filters: String,
    pub prop_filters: String,
    pub anim_filters: String,
    pub ui_panel_filters: String,
    pub max_scenes: usize,
    pub max_characters: usize,
    pub max_props: usize,
    pub max_audio: usize,
    pub max_ui_sprites: usize,
    pub max_ui_panels: usize,
}

impl Default for BevyExportUiState {
    fn default() -> Self {
        Self {
            project_name: "bevy_zero_game".to_owned(),
            scene_filters: String::new(),
            char_filters: String::new(),
            prop_filters: String::new(),
            anim_filters: String::new(),
            ui_panel_filters: String::new(),
            max_scenes: 6,
            max_characters: 8,
            max_props: 10,
            max_audio: 24,
            max_ui_sprites: 32,
            max_ui_panels: 6,
        }
    }
}

/// Sprite layout metadata displayed above cropped `Sprite` previews.
#[derive(Debug, Clone)]
pub struct SpritePreviewMeta {
    pub rect: [f32; 4],
    pub pivot: [f32; 2],
    pub pixels_to_units: f32,
    pub source_texture: Option<AssetRef>,
}

/// Interactive 2D uGUI Canvas preview state (`Canvas` / `RectTransform` / `Image` / `Text`).
pub struct UiCanvasPreviewState {
    pub document: UiCanvasDocument,
    pub images: HashMap<String, DecodedImage>,
    pub textures: HashMap<String, egui::TextureHandle>,
    pub zoom: f32,
    pub fit_to_view: bool,
    pub pan_offset: [f32; 2],
    pub show_outlines: bool,
    pub show_inactive: bool,
    pub show_labels: bool,
    pub show_checkerboard: bool,
    pub clip_to_canvas: bool,
    pub anim_playing: bool,
    pub anim_time: f32,
    pub hovered_info: Option<String>,
}

impl UiCanvasPreviewState {
    pub fn from_extracted(
        extracted: ExtractedUiCanvas,
        ctx: Option<&egui::Context>,
        nearest: bool,
    ) -> Self {
        let mut textures = HashMap::with_capacity(extracted.images.len());
        if let Some(c) = ctx {
            let opts = if nearest {
                egui::TextureOptions::NEAREST
            } else {
                egui::TextureOptions::LINEAR
            };
            for (key, img) in &extracted.images {
                if img.width > 0 && img.height > 0 {
                    let color_img = egui::ColorImage::from_rgba_unmultiplied(
                        [img.width as usize, img.height as usize],
                        &img.rgba8,
                    );
                    textures.insert(
                        key.clone(),
                        c.load_texture(format!("devx_ui_{key}"), color_img, opts),
                    );
                }
            }
        }
        Self {
            document: extracted.document,
            images: extracted.images,
            textures,
            zoom: 1.0,
            fit_to_view: true,
            pan_offset: [0.0, 0.0],
            show_outlines: false,
            show_inactive: false,
            show_labels: true,
            show_checkerboard: true,
            clip_to_canvas: true,
            anim_playing: true,
            anim_time: 0.0,
            hovered_info: None,
        }
    }
}

/// Decoded media/text content cached for the currently selected asset.
pub enum PreviewContent {
    Empty,
    Texture {
        image: DecodedImage,
        texture: Option<egui::TextureHandle>,
        dds_hint: Option<String>,
        sprite_meta: Option<SpritePreviewMeta>,
    },
    Audio {
        clip: DecodedAudioClip,
        selected_stream: usize,
    },
    Mesh {
        mesh: DecodedMesh,
        base_mesh: Option<DecodedMesh>,
        blend_shapes: Vec<DecodedBlendShape>,
        blend_weights: Vec<f32>,
    },
    Scene3D(HierarchyPreviewData),
    Material {
        material: DecodedMaterial,
        shader_ref: Option<AssetRef>,
        shader_name: Option<String>,
        textures: Vec<MaterialTexturePreview>,
    },
    AnimationClip {
        clip: DecodedAnimationClip,
        selected_track: usize,
    },
    AnimatorController {
        controller: DecodedAnimatorController,
        selected_layer: usize,
    },
    Cubemap {
        cubemap: DecodedCubemap,
        cross_texture: Option<egui::TextureHandle>,
        face_textures: Vec<Option<egui::TextureHandle>>,
        selected_face: Option<usize>,
    },
    SpriteAtlas {
        name: String,
        tags: Vec<String>,
        packed_count: usize,
        pages: Vec<SpriteAtlasPagePreview>,
        sprites: Vec<SpriteAtlasItemPreview>,
    },
    Text {
        asset: DecodedTextAsset,
    },
    Shader {
        shader: DecodedShader,
    },
    Font {
        font: DecodedFont,
    },
    Terrain {
        terrain: DecodedTerrainData,
        scene: HierarchyPreviewData,
    },
    ParticleSystem {
        particle: DecodedParticleSystem,
        scene: HierarchyPreviewData,
    },
    UiCanvas(Box<UiCanvasPreviewState>),
    Structured {
        note: String,
    },
    Error(String),
}

/// Flattened row used to virtualize arbitrarily nested asset trees with `ScrollArea::show_rows`.
#[derive(Debug, Clone)]
pub struct FlatTreeRow {
    pub key: String,
    pub depth: usize,
    pub label: String,
    pub subtitle: Option<String>,
    pub kind: TreeNodeKind,
    pub expanded: bool,
    pub has_children: bool,
}

/// Full workbench model shared across all dockable UI views.
pub struct DevXState {
    pub workspace_path: Option<PathBuf>,
    pub workspace_gen: u64,
    pub catalog: Option<Arc<AssetCatalog>>,
    pub graph: Option<Arc<ReferenceGraph>>,
    pub scripts: Option<Arc<ScriptsIndex>>,
    pub tpk_class_count: usize,

    pub jobs: JobManager,
    pub active_job: Option<(u64, String, f32, String)>,
    pub active_job_handle: Option<JobHandle>,
    scripts_tx: Sender<(u64, Arc<ScriptsIndex>)>,
    scripts_rx: Receiver<(u64, Arc<ScriptsIndex>)>,

    pub plugins: PluginRegistry,
    pub host: InMemoryHostContext,
    pub export_opts: ExportAssetOptions,

    pub grouping_mode: TreeGroupingMode,
    pub filter: TreeFilter,
    pub cached_tree: Vec<AssetTreeNode>,
    pub flat_rows: Vec<FlatTreeRow>,
    pub expanded_nodes: HashSet<String>,
    pub tree_dirty: bool,

    pub selected: Option<AssetRef>,
    pub selection_history: Vec<AssetRef>,
    pub selected_object: Option<UnityObject>,
    pub selected_json: Option<String>,
    pub selected_yaml: Option<String>,
    pub selected_raw_bytes: Option<Vec<u8>>,

    pub preview_tab: PreviewTab,
    pub tex_options: TextureDecodeOptions,
    pub tex_zoom: f32,
    pub tex_fit: bool,
    pub tex_nearest: bool,
    pub preview: PreviewContent,
    pub shader_view_mode: ShaderViewMode,
    pub shader_subprogram_filter: String,
    pub deferred_bundle_filter: String,
    pub load_deps_on_bundle_open: bool,
    pub mesh_view: MeshViewportState,
    pub audio_engine: AudioPlaybackEngine,

    pub bottom_tab: BottomTab,
    pub search_query: String,
    pub search_raw_bytes: bool,
    pub search_hits: Vec<SearchHit>,
    pub batch_scope: BatchScope,
    pub bevy_export_ui: BevyExportUiState,
    pub script_filter: String,
    pub selected_assembly_idx: Option<usize>,
    pub selected_il2cpp_image_idx: Option<usize>,
    pub selected_il2cpp_type_idx: Option<usize>,
    pub il2cpp_view_tab: Il2CppViewTab,
    pub il2cpp_type_source_cache: Option<Il2CppSourceCache>,
    pub il2cpp_include_disasm: bool,
    pub il2cpp_include_pseudo_c: bool,
    pub il2cpp_include_dummy_dlls: bool,
    pub il2cpp_disasm_opts: Il2CppDisasmOptions,
    pub il2cpp_type_methods_cache: Option<(usize, Vec<Il2CppMethodEntry>)>,
    pub il2cpp_selected_method_idx: Option<usize>,
    pub il2cpp_rva_input: String,
    pub il2cpp_disasm_result: Option<Result<DisassembledMethodReport, String>>,
    pub il2cpp_search: String,

    pub dark_mode: bool,
    pub status_banner: String,
    pub last_report: Option<String>,
}

impl Default for DevXState {
    fn default() -> Self {
        Self::new()
    }
}

impl DevXState {
    pub fn new() -> Self {
        let plugins = default_plugin_registry();
        let mut host = InMemoryHostContext::default();
        let _ = plugins.run_startup(&mut host);
        let (scripts_tx, scripts_rx) = channel::unbounded();

        Self {
            workspace_path: None,
            workspace_gen: 0,
            catalog: None,
            graph: None,
            scripts: None,
            tpk_class_count: tpk_class_count(),
            jobs: JobManager::default(),
            active_job: None,
            active_job_handle: None,
            scripts_tx,
            scripts_rx,
            plugins,
            host,
            export_opts: ExportAssetOptions::default(),
            grouping_mode: TreeGroupingMode::ByClass,
            filter: TreeFilter::default(),
            cached_tree: Vec::new(),
            flat_rows: Vec::new(),
            expanded_nodes: HashSet::new(),
            tree_dirty: false,
            selected: None,
            selection_history: Vec::new(),
            selected_object: None,
            selected_json: None,
            selected_yaml: None,
            selected_raw_bytes: None,
            preview_tab: PreviewTab::Auto,
            tex_options: TextureDecodeOptions::default(),
            tex_zoom: 1.0,
            tex_fit: true,
            tex_nearest: false,
            preview: PreviewContent::Empty,
            shader_view_mode: ShaderViewMode::DecompiledCg,
            shader_subprogram_filter: String::new(),
            deferred_bundle_filter: String::new(),
            load_deps_on_bundle_open: true,
            mesh_view: MeshViewportState::default(),
            audio_engine: AudioPlaybackEngine::default(),
            bottom_tab: BottomTab::DeepSearch,
            search_query: String::new(),
            search_raw_bytes: false,
            search_hits: Vec::new(),
            batch_scope: BatchScope::AllAssets,
            bevy_export_ui: BevyExportUiState::default(),
            script_filter: String::new(),
            selected_assembly_idx: None,
            selected_il2cpp_image_idx: None,
            selected_il2cpp_type_idx: None,
            il2cpp_view_tab: Il2CppViewTab::TypeSource,
            il2cpp_type_source_cache: None,
            il2cpp_include_disasm: false,
            il2cpp_include_pseudo_c: false,
            il2cpp_include_dummy_dlls: false,
            il2cpp_disasm_opts: Il2CppDisasmOptions::default(),
            il2cpp_type_methods_cache: None,
            il2cpp_selected_method_idx: None,
            il2cpp_rva_input: String::new(),
            il2cpp_disasm_result: None,
            il2cpp_search: String::new(),
            dark_mode: true,
            status_banner: "Ready. Open a Unity game directory, bundle, APK/XAPK/IPA/WebGL archive, or drag files here.".to_owned(),
            last_report: None,
        }
    }

    /// Clears transient IL2CPP UI selection and disassembler caches.
    pub fn clear_il2cpp_caches(&mut self) {
        self.selected_il2cpp_image_idx = None;
        self.selected_il2cpp_type_idx = None;
        self.il2cpp_type_source_cache = None;
        self.il2cpp_type_methods_cache = None;
        self.il2cpp_selected_method_idx = None;
        self.il2cpp_rva_input.clear();
        self.il2cpp_disasm_result = None;
    }

    /// Runs on-demand native disassembly at `rva` and stores the formatted report or error.
    pub fn disassemble_il2cpp_rva(
        &mut self,
        report: &Il2CppDumpReport,
        rva: u64,
        type_def_hint: Option<usize>,
        method_def_hint: Option<usize>,
    ) {
        self.il2cpp_rva_input = format!("0x{rva:X}");
        self.il2cpp_selected_method_idx = method_def_hint;
        self.il2cpp_disasm_result = Some(
            report
                .disassemble_rva(rva, type_def_hint, method_def_hint, &self.il2cpp_disasm_opts)
                .map_err(|e| e.to_string()),
        );
    }

    pub fn bind_repaint_hook(&mut self, ctx: &egui::Context) {
        let repaint_ctx = ctx.clone();
        self.jobs
            .set_repaint_hook(Arc::new(move || repaint_ctx.request_repaint()));
    }

    /// Starts asynchronous loading of a file or game directory.
    pub fn open_path_async(&mut self, path: PathBuf) {
        self.workspace_path = Some(path.clone());
        self.status_banner = format!("Opening {}...", path.display());
        self.host
            .log_info(&format!("Loading workspace: {}", path.display()));
        self.active_job_handle = Some(self.jobs.spawn_load_path(path));
    }

    /// Synchronously installs an already-indexed [`AssetCatalog`] (used both on `WorkspaceLoaded`
    /// and in deterministic unit tests).
    pub fn install_catalog(&mut self, catalog: Arc<AssetCatalog>, spawn_background_indexes: bool) {
        self.workspace_gen = self.workspace_gen.wrapping_add(1);
        let ws_gen = self.workspace_gen;

        self.audio_engine.stop();
        self.catalog = Some(Arc::clone(&catalog));
        self.graph = None;
        self.scripts = None;
        self.selected = None;
        self.selection_history.clear();
        self.selected_object = None;
        self.selected_json = None;
        self.selected_yaml = None;
        self.selected_raw_bytes = None;
        self.preview = PreviewContent::Empty;
        self.search_hits.clear();
        self.selected_assembly_idx = None;
        self.clear_il2cpp_caches();

        self.host.assets = catalog.entries.iter().map(|e| e.summary.clone()).collect();
        self.expanded_nodes.clear();
        self.filter = TreeFilter::default();
        self.tree_dirty = true;
        self.rebuild_tree_now();

        let deferred_total = catalog.workspace.deferred_bundles.len();
        let deferred_unloaded = catalog
            .workspace
            .deferred_bundles
            .iter()
            .filter(|d| !d.loaded)
            .count();
        self.status_banner = if deferred_total > 0 {
            format!(
                "Loaded {} file(s), {} objects, {}/{} deferred .ab bundles.",
                catalog.workspace.files.len(),
                catalog.entries.len(),
                deferred_total - deferred_unloaded,
                deferred_total
            )
        } else {
            format!(
                "Loaded {} file(s), {} objects, {} script artifacts.",
                catalog.workspace.files.len(),
                catalog.entries.len(),
                catalog.workspace.script_artifacts.len()
            )
        };
        self.host.log_info(&self.status_banner);

        if spawn_background_indexes {
            self.jobs.spawn_reference_graph(Arc::clone(&catalog));
            let ws = Arc::clone(&catalog.workspace);
            let tx = self.scripts_tx.clone();
            std::thread::spawn(move || {
                let idx = Arc::new(ScriptsIndex::from_workspace(&ws));
                let _ = tx.send((ws_gen, idx));
            });
        } else {
            self.graph = Some(Arc::new(catalog.build_reference_graph()));
            self.scripts = Some(Arc::new(ScriptsIndex::from_workspace(&catalog.workspace)));
        }
    }

    /// Drains all background job events and async script-indexing completions.
    pub fn poll_background_events(&mut self) {
        while let Ok((ws_gen, idx)) = self.scripts_rx.try_recv() {
            if ws_gen == self.workspace_gen {
                let il2cpp_note = if let Some(ref rec) = idx.il2cpp_recovered {
                    format!(
                        ", IL2CPP v{} ({} types, {} methods recovered)",
                        rec.metadata_version,
                        rec.types.len(),
                        rec.total_method_count
                    )
                } else if idx.il2cpp.is_some() {
                    ", IL2CPP global-metadata.dat".to_owned()
                } else {
                    String::new()
                };
                self.host.log_info(&format!(
                    "Script index ready: {} MonoScripts, {} managed assemblies{il2cpp_note}",
                    idx.mono_scripts.len(),
                    idx.assemblies.len(),
                ));
                self.scripts = Some(idx);
            }
        }

        let events = self.jobs.poll_events();
        for ev in events {
            match ev {
                JobEvent::Started { id, label } => {
                    self.active_job = Some((id, label, 0.05, String::new()));
                }
                JobEvent::Progress {
                    id,
                    fraction,
                    detail,
                } => {
                    if let Some((cur_id, _, frac, det)) = self.active_job.as_mut()
                        && *cur_id == id
                    {
                        *frac = fraction;
                        *det = detail;
                    }
                }
                JobEvent::WorkspaceLoaded { id, catalog } => {
                    if self.active_job.as_ref().is_some_and(|(cur, ..)| *cur == id) {
                        self.active_job = None;
                        self.active_job_handle = None;
                    }
                    self.install_catalog(catalog, true);
                }
                JobEvent::ReferenceGraphBuilt { id, graph } => {
                    if self.active_job.as_ref().is_some_and(|(cur, ..)| *cur == id) {
                        self.active_job = None;
                    }
                    self.host.log_info(&format!(
                        "PPtr reference graph built ({} sources, {} referenced targets)",
                        graph.forward.len(),
                        graph.reverse.len()
                    ));
                    self.graph = Some(graph);
                }
                JobEvent::SearchFinished { id, matches } => {
                    if self.active_job.as_ref().is_some_and(|(cur, ..)| *cur == id) {
                        self.active_job = None;
                    }
                    self.status_banner = format!("Deep search found {} match(es).", matches.len());
                    self.host.log_info(&self.status_banner);
                    self.search_hits = matches;
                }
                JobEvent::ExportFinished {
                    id,
                    output_dir,
                    files_written,
                    errors,
                } => {
                    if self.active_job.as_ref().is_some_and(|(cur, ..)| *cur == id) {
                        self.active_job = None;
                    }
                    let summary = format!(
                        "Exported {files_written} file(s) to {} ({} error(s)).",
                        output_dir.display(),
                        errors.len()
                    );
                    self.status_banner = summary.clone();
                    self.last_report = Some(summary.clone());
                    self.host.log_info(&summary);
                    for err in errors.iter().take(25) {
                        self.host.log_warn(err);
                    }
                }
                JobEvent::RawBundlesExtracted {
                    id,
                    output_dir,
                    files_written,
                    total_bytes,
                } => {
                    if self.active_job.as_ref().is_some_and(|(cur, ..)| *cur == id) {
                        self.active_job = None;
                        self.active_job_handle = None;
                    }
                    let mb = total_bytes as f64 / (1024.0 * 1024.0);
                    let summary = format!(
                        "Extracted {files_written} .ab bundle(s) ({mb:.2} MiB) to {}.",
                        output_dir.display()
                    );
                    self.status_banner = summary.clone();
                    self.last_report = Some(summary.clone());
                    self.host.log_info(&summary);
                }
                JobEvent::Failed { id, error } => {
                    if self.active_job.as_ref().is_some_and(|(cur, ..)| *cur == id) {
                        self.active_job = None;
                        self.active_job_handle = None;
                    }
                    self.status_banner = format!("Operation failed: {error}");
                    self.host.log_error(&error);
                }
            }
        }
    }

    /// Rebuilds the cached hierarchical tree and flattened visible row table when filters change.
    pub fn rebuild_tree_now(&mut self) {
        let Some(catalog) = self.catalog.as_ref() else {
            self.cached_tree.clear();
            self.flat_rows.clear();
            self.tree_dirty = false;
            return;
        };

        self.cached_tree = build_asset_tree(catalog, self.grouping_mode, &self.filter);

        // Auto-expand top-level groups when there are only a few groups or when filtering by text.
        let force_expand = !self.filter.search_text.trim().is_empty();
        if self.expanded_nodes.is_empty() && !force_expand {
            for (idx, node) in self.cached_tree.iter().enumerate() {
                if node.children.len() <= 200 || idx == 0 {
                    self.expanded_nodes.insert(format!("0/{idx}:{}", node.label));
                }
            }
        }

        let mut rows = Vec::new();
        flatten_nodes(
            &self.cached_tree,
            0,
            "",
            &self.expanded_nodes,
            force_expand,
            &mut rows,
        );
        self.flat_rows = rows;
        self.tree_dirty = false;
    }

    pub fn toggle_tree_node(&mut self, key: &str) {
        if !self.expanded_nodes.remove(key) {
            self.expanded_nodes.insert(key.to_owned());
        }
        let force_expand = !self.filter.search_text.trim().is_empty();
        let mut rows = Vec::new();
        flatten_nodes(
            &self.cached_tree,
            0,
            "",
            &self.expanded_nodes,
            force_expand,
            &mut rows,
        );
        self.flat_rows = rows;
    }

    /// Selects an asset, parses its TypeTree/JSON/YAML/raw bytes, and decodes its media preview.
    pub fn select_asset(&mut self, asset_ref: AssetRef, record_history: bool, ctx: Option<&egui::Context>) {
        if record_history
            && let Some(prev) = self.selected
            && prev != asset_ref
        {
            self.selection_history.push(prev);
            if self.selection_history.len() > 64 {
                self.selection_history.remove(0);
            }
        }

        self.selected = Some(asset_ref);
        self.mesh_view.reset_camera();
        self.selected_object = None;
        self.selected_json = None;
        self.selected_yaml = None;
        self.selected_raw_bytes = None;

        let Some(catalog) = self.catalog.clone() else {
            self.preview = PreviewContent::Empty;
            return;
        };

        if let Some(raw) = catalog.raw_bytes(asset_ref) {
            self.selected_raw_bytes = Some(raw.to_vec());
        }

        if let Some((loaded, info)) = catalog
            .workspace
            .object_info(asset_ref.file_index, asset_ref.path_id)
        {
            match read_object(loaded, info) {
                Ok(obj) => {
                    let val = UnityValue::Object(obj.class.properties().clone());
                    let json_val = unity_value_to_json(&val, Some(512));
                    self.selected_json = serde_json::to_string_pretty(&json_val).ok();
                    self.selected_yaml = serialize_object_yaml(&obj, asset_ref.path_id).ok();
                    self.selected_object = Some(obj);
                }
                Err(err) => {
                    self.preview = PreviewContent::Error(format!(
                        "Failed to parse object {}:{} ({}): {err}",
                        asset_ref.file_index, asset_ref.path_id, err
                    ));
                    return;
                }
            }
        }

        self.refresh_preview(ctx);
    }

    pub fn navigate_back(&mut self, ctx: Option<&egui::Context>) {
        if let Some(prev) = self.selection_history.pop() {
            self.select_asset(prev, false, ctx);
        }
    }

    /// Re-decodes or updates GPU texture handles for the currently selected asset.
    pub fn refresh_preview(&mut self, ctx: Option<&egui::Context>) {
        let (Some(catalog), Some(asset_ref), Some(obj)) = (
            self.catalog.as_ref(),
            self.selected,
            self.selected_object.as_ref(),
        ) else {
            self.preview = PreviewContent::Empty;
            return;
        };

        let Some(entry) = catalog.entry(asset_ref) else {
            self.preview = PreviewContent::Empty;
            return;
        };
        let summary = &entry.summary;
        let prev_ui_view = match &self.preview {
            PreviewContent::UiCanvas(u) => Some(UiViewPrefs {
                root_ref: u.document.root_ref,
                ref_w: u.document.reference_width,
                ref_h: u.document.reference_height,
                zoom: u.zoom,
                fit_to_view: u.fit_to_view,
                pan_offset: u.pan_offset,
                show_outlines: u.show_outlines,
                show_inactive: u.show_inactive,
                show_labels: u.show_labels,
                show_checkerboard: u.show_checkerboard,
                clip_to_canvas: u.clip_to_canvas,
                anim_playing: u.anim_playing,
                anim_time: u.anim_time,
            }),
            _ => None,
        };
        let make_ui_preview = |extracted: ExtractedUiCanvas| {
            let mut state = UiCanvasPreviewState::from_extracted(extracted, ctx, self.tex_nearest);
            if let Some(prev) = prev_ui_view.filter(|p| p.root_ref == state.document.root_ref) {
                if (state.document.reference_width - prev.ref_w).abs() > 0.5
                    || (state.document.reference_height - prev.ref_h).abs() > 0.5
                {
                    recompute_ui_layout(&mut state.document, prev.ref_w, prev.ref_h);
                }
                state.zoom = prev.zoom;
                state.fit_to_view = prev.fit_to_view;
                state.pan_offset = prev.pan_offset;
                state.show_outlines = prev.show_outlines;
                state.show_inactive = prev.show_inactive;
                state.show_labels = prev.show_labels;
                state.show_checkerboard = prev.show_checkerboard;
                state.clip_to_canvas = prev.clip_to_canvas;
                state.anim_playing = prev.anim_playing;
                state.anim_time = prev.anim_time;
            }
            PreviewContent::UiCanvas(Box::new(state))
        };

        self.preview = match summary.class_id {
            28 => {
                // Texture2D
                match decode_texture2d_with_plugins(
                    obj,
                    summary,
                    &catalog.workspace.streams,
                    self.tex_options,
                    &self.plugins,
                    &mut self.host,
                ) {
                    Ok(image) => {
                        let dds_hint = inspect_texture2d_raw(
                            obj,
                            &summary.unity_version,
                            &catalog.workspace.streams,
                        )
                        .ok()
                        .map(|t| format!("{:?} ({} mip(s))", t.format, t.mip_count.max(1)));
                        let texture = ctx.map(|c| {
                            upload_texture_handle(c, asset_ref, &image, self.tex_nearest)
                        });
                        PreviewContent::Texture {
                            image,
                            texture,
                            dds_hint,
                            sprite_meta: None,
                        }
                    }
                    Err(err) => PreviewContent::Error(err.to_string()),
                }
            }
            213 => {
                // Sprite
                let tpk = shared_tpk_registry();
                match decode_sprite(
                    obj,
                    asset_ref.file_index,
                    &summary.unity_version,
                    &catalog.workspace,
                    &*tpk,
                ) {
                    Ok(DecodedSprite {
                        image,
                        rect,
                        pivot,
                        pixels_to_units,
                        source_texture,
                        ..
                    }) => {
                        let masked = if self.tex_options.channel_mask
                            != devx_codecs::texture::ChannelMask::All
                        {
                            devx_codecs::texture::apply_channel_mask(
                                &image,
                                self.tex_options.channel_mask,
                            )
                        } else {
                            image
                        };
                        let texture = ctx.map(|c| {
                            upload_texture_handle(c, asset_ref, &masked, self.tex_nearest)
                        });
                        PreviewContent::Texture {
                            image: masked,
                            texture,
                            dds_hint: None,
                            sprite_meta: Some(SpritePreviewMeta {
                                rect,
                                pivot,
                                pixels_to_units,
                                source_texture: source_texture.map(|(file_index, path_id)| {
                                    AssetRef {
                                        file_index,
                                        path_id,
                                    }
                                }),
                            }),
                        }
                    }
                    Err(err) => PreviewContent::Error(err.to_string()),
                }
            }
            83 => {
                // AudioClip
                match decode_audio_clip(obj, &summary.unity_version, &catalog.workspace.streams) {
                    Ok(mut clip) => {
                        let _ = self
                            .plugins
                            .post_process_audio(summary, &mut clip.primary, &mut self.host);
                        PreviewContent::Audio {
                            clip,
                            selected_stream: 0,
                        }
                    }
                    Err(err) => PreviewContent::Error(err.to_string()),
                }
            }
            43 => {
                // Mesh — upgrade to textured/rigged Scene3D automatically when Material backlinks resolve
                if let Some(scene) = build_hierarchy_preview(
                    &catalog.workspace,
                    self.graph.as_deref(),
                    asset_ref,
                    16,
                ) {
                    PreviewContent::Scene3D(scene)
                } else {
                    match decode_mesh_full(obj, &summary.unity_version, &catalog.workspace.streams) {
                        Ok(full) => {
                            let blend_weights =
                                full.blend_shapes.iter().map(|b| b.default_weight).collect();
                            let base_mesh =
                                (!full.blend_shapes.is_empty()).then(|| full.base.clone());
                            PreviewContent::Mesh {
                                mesh: full.base,
                                base_mesh,
                                blend_shapes: full.blend_shapes,
                                blend_weights,
                            }
                        }
                        Err(err) => PreviewContent::Error(err.to_string()),
                    }
                }
            }
            223 | 225 | 222 => {
                // Canvas / CanvasGroup / CanvasRenderer -> interactive 2D uGUI Canvas Preview
                if let Some(extracted) =
                    extract_ui_canvas(&catalog.workspace, asset_ref, DEFAULT_MAX_UI_NODES)
                {
                    make_ui_preview(extracted)
                } else {
                    PreviewContent::Structured {
                        note: format!(
                            "{} (class_id {}) — inspect TypeTree properties on the right, or view YAML / JSON / Hex RAW tabs.",
                            summary.class_name, summary.class_id
                        ),
                    }
                }
            }
            224 => {
                // RectTransform -> prefer 2D uGUI Canvas preview, fall back to 3D hierarchy if needed
                if let Some(extracted) =
                    extract_ui_canvas(&catalog.workspace, asset_ref, DEFAULT_MAX_UI_NODES)
                {
                    make_ui_preview(extracted)
                } else if let Some(scene) = build_hierarchy_preview(
                    &catalog.workspace,
                    self.graph.as_deref(),
                    asset_ref,
                    96,
                ) {
                    PreviewContent::Scene3D(scene)
                } else {
                    PreviewContent::Structured {
                        note: format!(
                            "{} (class_id {}) — inspect TypeTree properties on the right, or view YAML / JSON / Hex RAW tabs.",
                            summary.class_name, summary.class_id
                        ),
                    }
                }
            }
            1 | 4 | 23 | 33 | 137 | 142 => {
                // GameObject / Transform / MeshRenderer / MeshFilter / SkinnedMeshRenderer / AssetBundle
                // Prefer 2D uGUI Canvas preview when selecting a GameObject that directly owns a
                // Canvas (223) or RectTransform (224), even if it embeds a 3D role preview subtree.
                let owns_ui_comp = summary.class_id == 1
                    && obj
                        .get("m_Component")
                        .and_then(|v| v.as_array())
                        .is_some_and(|arr| {
                            arr.iter().any(|item| {
                                item.as_object()
                                    .and_then(|m| m.get("component"))
                                    .or(Some(item))
                                    .and_then(|p| {
                                        resolve_pptr_ref(
                                            &catalog.workspace,
                                            asset_ref.file_index,
                                            p,
                                        )
                                    })
                                    .and_then(|cref| catalog.entry(cref))
                                    .is_some_and(|ce| matches!(ce.summary.class_id, 223 | 224))
                            })
                        });
                let max_parts = if summary.class_id == 142 { 256 } else { 96 };
                if owns_ui_comp
                    && let Some(extracted) =
                        extract_ui_canvas(&catalog.workspace, asset_ref, DEFAULT_MAX_UI_NODES)
                {
                    make_ui_preview(extracted)
                } else if let Some(scene) = build_hierarchy_preview(
                    &catalog.workspace,
                    self.graph.as_deref(),
                    asset_ref,
                    max_parts,
                ) {
                    PreviewContent::Scene3D(scene)
                } else if let Some(extracted) = if summary.class_id == 142 {
                    resolve_scene_root_transforms(&catalog.workspace, &[asset_ref])
                        .into_iter()
                        .find_map(|(file_index, path_id)| {
                            extract_ui_canvas(
                                &catalog.workspace,
                                AssetRef { file_index, path_id },
                                DEFAULT_MAX_UI_NODES,
                            )
                        })
                } else {
                    extract_ui_canvas(&catalog.workspace, asset_ref, DEFAULT_MAX_UI_NODES)
                } {
                    make_ui_preview(extracted)
                } else {
                    let note = if summary.class_id == 142 {
                        format!(
                            "{} (class_id 142) — streamed scene / asset bundle has no local 3D meshes or UI Canvas in loaded files (switch tree grouping to 'By Container / File' or 'By Scene Hierarchy' to inspect individual objects, and check External References for companion CAB files).",
                            summary.class_name
                        )
                    } else {
                        format!(
                            "{} (class_id {}) — no attached mesh renderers; inspect Hierarchy/Components & TypeTree properties on the right.",
                            summary.class_name, summary.class_id
                        )
                    };
                    PreviewContent::Structured { note }
                }
            }
            114 => {
                // MonoBehaviour — show interactive UI Canvas if attached to a uGUI hierarchy
                if let Some(extracted) =
                    extract_ui_canvas(&catalog.workspace, asset_ref, DEFAULT_MAX_UI_NODES)
                {
                    make_ui_preview(extracted)
                } else {
                    PreviewContent::Structured {
                        note: format!(
                            "{} (class_id {}) — inspect TypeTree properties on the right, or view YAML / JSON / Hex RAW tabs.",
                            summary.class_name, summary.class_id
                        ),
                    }
                }
            }
            21 => {
                // Material
                match decode_material(obj) {
                    Ok(mat) => decode_material_preview(catalog, asset_ref, mat, ctx),
                    Err(err) => PreviewContent::Error(err.to_string()),
                }
            }
            74 => {
                // AnimationClip
                match decode_animation_clip(obj) {
                    Ok(mut clip) => {
                        resolve_clip_track_names(&catalog.workspace, &mut clip);
                        PreviewContent::AnimationClip {
                            clip,
                            selected_track: 0,
                        }
                    }
                    Err(err) => PreviewContent::Error(err.to_string()),
                }
            }
            91 => {
                // AnimatorController
                match decode_animator_controller(obj, asset_ref.file_index, &catalog.workspace) {
                    Ok(controller) => PreviewContent::AnimatorController {
                        controller,
                        selected_layer: 0,
                    },
                    Err(err) => PreviewContent::Error(err.to_string()),
                }
            }
            89 => {
                // Cubemap
                match decode_cubemap(obj, &summary.unity_version, &catalog.workspace.streams) {
                    Ok(cubemap) => PreviewContent::Cubemap {
                        cubemap,
                        cross_texture: None,
                        face_textures: Vec::new(),
                        selected_face: None,
                    },
                    Err(err) => PreviewContent::Error(err.to_string()),
                }
            }
            687078895 => {
                // SpriteAtlas
                decode_sprite_atlas_preview(catalog, asset_ref, obj, &summary.unity_version, ctx)
            }
            49 => {
                // TextAsset
                match decode_text_asset(obj) {
                    Ok(asset) => PreviewContent::Text { asset },
                    Err(err) => PreviewContent::Error(err.to_string()),
                }
            }
            48 => {
                // Shader
                match decode_shader(obj) {
                    Ok(mut shader) => {
                        let _ = self.plugins.post_process_shader(
                            &devx_plugin_api::ShaderPostProcessInput {
                                asset: summary.clone(),
                                stage: devx_plugin_api::ShaderPostProcessStage::AfterDecompile,
                            },
                            &mut shader.shaderlab_text,
                            &mut self.host,
                        );
                        PreviewContent::Shader { shader }
                    }
                    Err(err) => PreviewContent::Error(err.to_string()),
                }
            }
            128 => {
                // Font
                match decode_font(obj) {
                    Ok(font) => PreviewContent::Font { font },
                    Err(err) => PreviewContent::Error(err.to_string()),
                }
            }
            156 => {
                // TerrainData
                match decode_terrain_data(obj) {
                    Ok(terrain) => {
                        let scene = build_terrain_preview(&terrain, asset_ref);
                        PreviewContent::Terrain { terrain, scene }
                    }
                    Err(err) => PreviewContent::Error(err.to_string()),
                }
            }
            198 => {
                // ParticleSystem
                match decode_particle_system(obj) {
                    Ok(particle) => {
                        let scene =
                            build_particle_preview(&catalog.workspace, &particle, asset_ref, obj);
                        PreviewContent::ParticleSystem { particle, scene }
                    }
                    Err(err) => PreviewContent::Error(err.to_string()),
                }
            }
            _ => PreviewContent::Structured {
                note: format!(
                    "{} (class_id {}) — inspect TypeTree properties on the right, or view YAML / JSON / Hex RAW tabs.",
                    summary.class_name, summary.class_id
                ),
            },
        };
    }

    pub fn spawn_deep_search(&mut self) {
        let Some(catalog) = self.catalog.as_ref().map(Arc::clone) else {
            return;
        };
        if self.search_query.trim().is_empty() {
            self.search_hits.clear();
            return;
        }
        self.jobs.spawn_deep_search(
            catalog,
            self.search_query.clone(),
            self.search_raw_bytes,
        );
    }

    /// Exports one asset using `opts_override` (if provided) via an interactive Save File dialog.
    pub fn export_selected_interactive(
        &mut self,
        asset_ref: AssetRef,
        override_opts: Option<ExportAssetOptions>,
    ) {
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };
        let opts = override_opts.unwrap_or_else(|| self.export_opts.clone());
        match devx_exporter::build_export_artifact(
            catalog,
            asset_ref,
            &self.plugins,
            &mut self.host,
            &opts,
        ) {
            Ok(artifact) => {
                let default_name = Path::new(&artifact.relative_path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("asset.bin");
                if let Some(path) = rfd::FileDialog::new()
                    .set_file_name(default_name)
                    .save_file()
                {
                    if let Err(err) = std::fs::write(&path, &artifact.bytes) {
                        self.host.log_error(&format!(
                            "Failed writing {}: {err}",
                            path.display()
                        ));
                        self.status_banner = format!("Save failed: {err}");
                    } else {
                        let msg = format!(
                            "Saved {} ({} bytes).",
                            path.display(),
                            artifact.bytes.len()
                        );
                        self.host.log_info(&msg);
                        self.status_banner = msg;
                    }
                }
            }
            Err(err) => {
                self.host.log_error(&format!("Export failed: {err}"));
                self.status_banner = format!("Export failed: {err}");
            }
        }
    }

    /// Saves arbitrary bytes via an interactive Save File dialog.
    pub fn save_bytes_interactive(&mut self, suggested_name: &str, bytes: &[u8]) {
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name(suggested_name)
            .save_file()
        {
            match std::fs::write(&path, bytes) {
                Ok(()) => {
                    let msg = format!("Saved {} ({} bytes).", path.display(), bytes.len());
                    self.host.log_info(&msg);
                    self.status_banner = msg;
                }
                Err(err) => {
                    self.host
                        .log_error(&format!("Failed saving {}: {err}", path.display()));
                    self.status_banner = format!("Save failed: {err}");
                }
            }
        }
    }

    /// Spawns a non-blocking batch export job over the selected scope into `out_dir`.
    pub fn spawn_batch_export(&mut self, out_dir: PathBuf) {
        let Some(catalog) = self.catalog.as_ref().map(Arc::clone) else {
            return;
        };
        let assets = self.collect_batch_targets(&catalog);
        if assets.is_empty() {
            self.status_banner = "No matching assets in batch export scope.".to_owned();
            return;
        }

        let handle = self
            .jobs
            .alloc_handle(format!("Batch exporting {} asset(s)", assets.len()));
        let tx = self.jobs.sender();
        let plugins = Arc::new(default_plugin_registry());
        let opts = self.export_opts.clone();
        let id = handle.id;
        self.active_job_handle = Some(handle.clone());

        std::thread::spawn(move || {
            let mut host = InMemoryHostContext::default();
            let mut last_pct = 0usize;
            let result = export_batch_directory(
                &catalog,
                &assets,
                &out_dir,
                &plugins,
                &mut host,
                &opts,
                |done, total| {
                    let pct = (done * 20) / total.max(1);
                    if pct != last_pct || done == total {
                        last_pct = pct;
                        let _ = tx.send(JobEvent::Progress {
                            id,
                            fraction: done as f32 / total.max(1) as f32,
                            detail: format!("{done}/{total} assets"),
                        });
                    }
                },
            );
            match result {
                Ok(report) => {
                    let _ = tx.send(JobEvent::ExportFinished {
                        id,
                        output_dir: out_dir,
                        files_written: report.exported_files.len(),
                        errors: report.errors,
                    });
                }
                Err(err) => {
                    let _ = tx.send(JobEvent::Failed {
                        id,
                        error: err.to_string(),
                    });
                }
            }
        });
    }


    /// Exports the current scope into a `.unitypackage` archive asynchronously.
    pub fn spawn_unitypackage_export(&mut self, out_path: PathBuf) {
        let Some(catalog) = self.catalog.as_ref().map(Arc::clone) else {
            return;
        };
        let assets = self.collect_batch_targets(&catalog);
        if assets.is_empty() {
            self.status_banner = "No assets to pack into .unitypackage.".to_owned();
            return;
        }

        let handle = self
            .jobs
            .alloc_handle(format!("Packing .unitypackage ({} assets)", assets.len()));
        let tx = self.jobs.sender();
        let plugins = Arc::new(default_plugin_registry());
        let opts = self.export_opts.clone();
        let id = handle.id;
        self.active_job_handle = Some(handle);

        std::thread::spawn(move || {
            let mut host = InMemoryHostContext::default();
            match export_unitypackage_from_catalog(
                &catalog,
                &assets,
                &out_path,
                &plugins,
                &mut host,
                &opts,
            ) {
                Ok(count) => {
                    let _ = tx.send(JobEvent::ExportFinished {
                        id,
                        output_dir: out_path,
                        files_written: count,
                        errors: Vec::new(),
                    });
                }
                Err(err) => {
                    let _ = tx.send(JobEvent::Failed {
                        id,
                        error: err.to_string(),
                    });
                }
            }
        });
    }


    /// Repacks the currently loaded serialized files into a UnityFS v7 bundle via `rabex`.
    pub fn spawn_unityfs_repack(&mut self, out_path: PathBuf) {
        let Some(catalog) = self.catalog.as_ref().map(Arc::clone) else {
            return;
        };
        let entries: Vec<BundleFileEntry> = catalog
            .workspace
            .files
            .iter()
            .filter_map(|f| {
                let raw = f.raw_bytes.as_ref()?;
                if raw.is_empty() {
                    return None;
                }
                Some(BundleFileEntry {
                    name: Path::new(&f.virtual_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("CAB-repack")
                        .to_owned(),
                    bytes: raw.to_vec(),
                    is_serialized_file: true,
                })
            })
            .collect();
        if entries.is_empty() {
            self.status_banner = "No serialized file buffers available to repack.".to_owned();
            return;
        }

        let unity_ver = catalog
            .workspace
            .files
            .first()
            .map(|f| f.file.unity_version.clone())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "2022.3.0f1".to_owned());

        let handle = self
            .jobs
            .alloc_handle(format!("Repacking UnityFS ({} file(s))", entries.len()));
        let tx = self.jobs.sender();
        let id = handle.id;
        self.active_job_handle = Some(handle);

        std::thread::spawn(move || {
            let count = entries.len();
            match write_unityfs_bundle(&out_path, &unity_ver, &entries, CompressionType::Lz4hc) {
                Ok(()) => {
                    let _ = tx.send(JobEvent::ExportFinished {
                        id,
                        output_dir: out_path,
                        files_written: count,
                        errors: Vec::new(),
                    });
                }
                Err(err) => {
                    let _ = tx.send(JobEvent::Failed {
                        id,
                        error: err.to_string(),
                    });
                }
            }
        });
    }


    /// Streams raw `.ab` / `.bundle` / `.unity3d` files onto disk without parsing every entry.
    pub fn spawn_extract_raw_bundles(
        &mut self,
        source_path: PathBuf,
        output_dir: PathBuf,
        filter: Option<String>,
    ) {
        self.status_banner = format!("Extracting raw .ab bundles to {}...", output_dir.display());
        self.active_job_handle = Some(self.jobs.spawn_extract_raw_bundles(
            source_path,
            output_dir,
            filter,
        ));
    }

    /// Loads selected deferred `.ab` bundles asynchronously and updates the workspace catalog.
    pub fn load_deferred_bundles_async(&mut self, indices: Vec<usize>) {
        if indices.is_empty() {
            return;
        }
        if let Some(cat) = self.catalog.as_ref()
            && (Arc::strong_count(cat) > 1 || Arc::strong_count(&cat.workspace) > 1)
        {
            self.status_banner =
                "Background indexing task still running — retry loading .ab bundles in a moment."
                    .to_owned();
            return;
        }
        let Some(cat_arc) = self.catalog.take() else {
            return;
        };
        match Arc::try_unwrap(cat_arc).map(|c| Arc::try_unwrap(c.workspace)) {
            Ok(Ok(workspace)) => {
                self.status_banner = if self.load_deps_on_bundle_open {
                    format!("Loading {} .ab bundle(s) + transitive dependencies...", indices.len())
                } else {
                    format!("Loading {} .ab bundle(s)...", indices.len())
                };
                self.active_job_handle = Some(self.jobs.spawn_load_deferred_bundles_opts(
                    workspace,
                    indices,
                    self.load_deps_on_bundle_open,
                ));
            }
            _ => {
                self.status_banner =
                    "Workspace is busy in another task — retry loading .ab bundles.".to_owned();
            }
        }
    }

    /// Builds a [`BevyExportOptions`] snapshot from the current UI form state.
    pub fn build_bevy_export_options(&self) -> BevyExportOptions {
        let split_csv = |raw: &str| -> Vec<String> {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        };
        BevyExportOptions {
            project_name: if self.bevy_export_ui.project_name.trim().is_empty() {
                "bevy_zero_game".to_owned()
            } else {
                self.bevy_export_ui.project_name.trim().to_owned()
            },
            scene_filters: split_csv(&self.bevy_export_ui.scene_filters),
            character_filters: split_csv(&self.bevy_export_ui.char_filters),
            prop_filters: split_csv(&self.bevy_export_ui.prop_filters),
            anim_filters: split_csv(&self.bevy_export_ui.anim_filters),
            ui_panel_filters: split_csv(&self.bevy_export_ui.ui_panel_filters),
            max_scenes: self.bevy_export_ui.max_scenes.max(1),
            max_characters: self.bevy_export_ui.max_characters.max(1),
            max_props: self.bevy_export_ui.max_props.max(1),
            max_audio: self.bevy_export_ui.max_audio,
            max_fonts: 4,
            max_ui_sprites: self.bevy_export_ui.max_ui_sprites,
            max_ui_panels: self.bevy_export_ui.max_ui_panels,
            max_texture_dim: Some(1024),
            ..Default::default()
        }
    }

    fn list_workspace_assets_by_class(
        &self,
        class_id: i32,
        fallback_prefix: &str,
    ) -> Vec<(AssetRef, String)> {
        let Some(catalog) = self.catalog.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<(AssetRef, String)> = catalog
            .entries
            .iter()
            .filter(|e| e.summary.class_id == class_id)
            .map(|e| {
                let lbl = if e.summary.name.is_empty() {
                    format!("{fallback_prefix}_{}", e.asset_ref.path_id)
                } else {
                    e.summary.name.clone()
                };
                (e.asset_ref, lbl)
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1));
        out.truncate(256);
        out
    }

    /// Lists loaded `AnimationClip` assets in the current workspace for live preview binding.
    pub fn list_workspace_animation_clips(&self) -> Vec<(AssetRef, String)> {
        self.list_workspace_assets_by_class(74, "Clip")
    }

    /// Lists loaded `AnimatorController` assets in the current workspace for live state machine playback.
    pub fn list_workspace_animator_controllers(&self) -> Vec<(AssetRef, String)> {
        self.list_workspace_assets_by_class(91, "Controller")
    }

    /// Decodes an external `AnimatorController` (`class_id == 91`), links its clips into the
    /// currently open `Scene3D` preview, and activates the live state-machine simulator.
    pub fn attach_controller_to_active_scene(&mut self, ctrl_ref: AssetRef) {
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };
        let Some((loaded, info)) = catalog
            .workspace
            .object_info(ctrl_ref.file_index, ctrl_ref.path_id)
        else {
            return;
        };
        let Ok(obj) = read_object(loaded, info) else {
            return;
        };
        if obj.class_id() != 91 {
            return;
        }
        let Ok(controller) =
            decode_animator_controller(&obj, ctrl_ref.file_index, &catalog.workspace)
        else {
            return;
        };
        let ctrl_name = controller.name.clone();
        let layers = controller.layers.len();
        let clips = controller.clip_links.len();

        if let PreviewContent::Scene3D(ref mut scene) = self.preview {
            attach_controller_to_preview(scene, &catalog.workspace, controller);
            scene.animator_mode = true;
            scene.anim_playing = true;
            scene.last_ui_time = None;
            step_and_apply_animator_sim(scene, 0.0);
            self.status_banner = format!(
                "Attached AnimatorController `{ctrl_name}` ({layers} layers, {clips} linked clips)"
            );
        }
    }

    /// Decodes an external `AnimationClip` from the workspace and binds its curves onto the
    /// currently open `Scene3D` preview skeleton, switching playback to it immediately.
    pub fn attach_clip_to_active_scene(&mut self, clip_ref: AssetRef) {
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };
        let Some((loaded, info)) = catalog.workspace.object_info(clip_ref.file_index, clip_ref.path_id)
        else {
            return;
        };
        let Ok(obj) = read_object(loaded, info) else {
            return;
        };
        if obj.class_id() != 74 {
            return;
        }
        let Ok(decoded) = decode_animation_clip(&obj) else {
            return;
        };

        if let PreviewContent::Scene3D(ref mut scene) = self.preview
            && let Some(doc_arc) = scene.scene_doc.as_mut()
        {
            let doc = Arc::make_mut(doc_arc);
            let before = doc.animations.len();
            if let Some(existing_idx) = doc.animations.iter().position(|c| c.name == decoded.name) {
                scene.anim_clip_idx = Some(existing_idx);
            } else {
                doc.attach_decoded_clip(&decoded);
                if doc.animations.len() > before {
                    scene.anim_clip_idx = Some(doc.animations.len() - 1);
                }
            }
            scene.anim_time_secs = 0.0;
            scene.anim_playing = scene.anim_clip_idx.is_some();
            scene.last_ui_time = None;
            apply_scene_animation_pose(scene, scene.anim_clip_idx, 0.0);
            self.status_banner = format!(
                "Attached AnimationClip `{}` ({:.2}s)",
                decoded.name, decoded.duration_secs
            );
        }
    }

    /// Exports one `GameObject` / `Transform` / `SkinnedMeshRenderer` / `Mesh` hierarchy
    /// (plus matching animations & PBR textures) in any supported 3D format on a worker thread.
    pub fn spawn_export_hierarchy_format(
        &mut self,
        root_ref: AssetRef,
        out_path: PathBuf,
        format: HierarchyExportFormat,
    ) {
        let Some(catalog) = self.catalog.as_ref().map(Arc::clone) else {
            return;
        };
        let label = catalog
            .entry(root_ref)
            .map(|e| e.summary.name.clone())
            .unwrap_or_else(|| format!("Hierarchy_{}", root_ref.path_id));
        let handle = self
            .jobs
            .alloc_handle(format!("Exporting Hierarchy {} ({label})", format.label()));
        let tx = self.jobs.sender();
        let id = handle.id;
        self.active_job_handle = Some(handle);
        self.status_banner = format!("Exporting `{label}` as {}...", format.extension());

        std::thread::spawn(move || {
            let extra_clips = collect_loaded_animation_clips(&catalog.workspace);
            let opts = GltfSceneOptions::default();
            match export_hierarchy_with_format(
                &catalog.workspace,
                &[root_ref],
                &extra_clips,
                &out_path,
                format,
                &opts,
            ) {
                Ok(rep) => {
                    let msg = format!(
                        "{}: {} nodes, {} meshes, {} tris, {} materials, {} textures, {} skins, {} anims",
                        format.label(),
                        rep.summary.node_count,
                        rep.summary.mesh_count,
                        rep.summary.triangle_count,
                        rep.summary.material_count,
                        rep.summary.texture_count,
                        rep.summary.skin_count,
                        rep.summary.animation_count
                    );
                    let _ = tx.send(JobEvent::Progress {
                        id,
                        fraction: 1.0,
                        detail: msg,
                    });
                    let _ = tx.send(JobEvent::ExportFinished {
                        id,
                        output_dir: out_path,
                        files_written: 1,
                        errors: Vec::new(),
                    });
                }
                Err(err) => {
                    let _ = tx.send(JobEvent::Failed {
                        id,
                        error: err.to_string(),
                    });
                }
            }
        });
    }

    /// Backwards-compatible helper for GLB export.
    pub fn spawn_export_hierarchy_glb(&mut self, root_ref: AssetRef, out_path: PathBuf) {
        self.spawn_export_hierarchy_format(root_ref, out_path, HierarchyExportFormat::Glb);
    }

    /// Interactive Save File wrapper for exporting `root_ref` in any [`HierarchyExportFormat`].
    pub fn export_hierarchy_interactive(
        &mut self,
        root_ref: AssetRef,
        format: HierarchyExportFormat,
    ) {
        let stem = self
            .catalog
            .as_ref()
            .and_then(|c| c.entry(root_ref))
            .map(|e| e.summary.name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("hierarchy_{}", root_ref.path_id));
        let ext = format.extension();
        let suggested = format!("{}.{ext}", stem.replace(['/', '\\', ' '], "_"));
        if let Some(out_path) = rfd::FileDialog::new()
            .set_file_name(&suggested)
            .add_filter(format.label(), &[ext])
            .save_file()
        {
            self.spawn_export_hierarchy_format(root_ref, out_path, format);
        }
    }

    /// Interactive Save File wrapper for exporting `root_ref` as `.glb`.
    pub fn export_hierarchy_glb_interactive(&mut self, root_ref: AssetRef) {
        self.export_hierarchy_interactive(root_ref, HierarchyExportFormat::Glb);
    }

    /// Interactive Save / Folder dialog for `.prefab`, `.unity` scene, or `.unitypackage` export.
    pub fn export_prefab_interactive(
        &mut self,
        root_ref: AssetRef,
        is_scene: bool,
        as_unitypackage: bool,
    ) {
        let stem = self
            .catalog
            .as_ref()
            .and_then(|c| c.entry(root_ref))
            .map(|e| e.summary.name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("hierarchy_{}", root_ref.path_id))
            .replace(['/', '\\', ' '], "_");

        if as_unitypackage {
            if let Some(out_path) = rfd::FileDialog::new()
                .set_file_name(format!("{stem}.unitypackage"))
                .add_filter("Unity Package", &["unitypackage"])
                .save_file()
            {
                self.spawn_export_prefab(root_ref, stem, out_path, is_scene, true);
            }
        } else if let Some(out_dir) = rfd::FileDialog::new()
            .set_title(if is_scene {
                "Choose Output Folder for Unity Scene (.unity + assets)"
            } else {
                "Choose Output Folder for Unity Prefab (.prefab + assets)"
            })
            .pick_folder()
        {
            self.spawn_export_prefab(root_ref, stem, out_dir, is_scene, false);
        }
    }

    /// Worker-thread `.prefab`, `.unity`, or `.unitypackage` export.
    pub fn spawn_export_prefab(
        &mut self,
        root_ref: AssetRef,
        stem: String,
        out_target: PathBuf,
        is_scene: bool,
        as_unitypackage: bool,
    ) {
        let Some(catalog) = self.catalog.as_ref().map(Arc::clone) else {
            return;
        };
        let kind_lbl = if as_unitypackage {
            ".unitypackage"
        } else if is_scene {
            ".unity Scene"
        } else {
            ".prefab"
        };
        let handle = self
            .jobs
            .alloc_handle(format!("Exporting {kind_lbl} ({stem})"));
        let tx = self.jobs.sender();
        let id = handle.id;
        self.active_job_handle = Some(handle);
        self.status_banner = format!("Exporting `{stem}` as {kind_lbl}...");

        std::thread::spawn(move || {
            let opts = PrefabExportOptions {
                is_scene,
                include_companion_assets: true,
                ..Default::default()
            };
            let res = if as_unitypackage {
                export_prefab_unitypackage(&catalog.workspace, &[root_ref], &stem, &out_target, &opts)
            } else {
                export_prefab_to_dir(&catalog.workspace, &[root_ref], &stem, &out_target, &opts)
            };
            match res {
                Ok(b) => {
                    let msg = format!(
                        "{kind_lbl}: {} YAML objects, {} GameObjects, {} materials, {} textures, {} meshes",
                        b.object_count,
                        b.game_object_count,
                        b.material_count,
                        b.texture_count,
                        b.mesh_count
                    );
                    let _ = tx.send(JobEvent::Progress {
                        id,
                        fraction: 1.0,
                        detail: msg,
                    });
                    let _ = tx.send(JobEvent::ExportFinished {
                        id,
                        output_dir: out_target,
                        files_written: 1 + b.companion_files.len(),
                        errors: Vec::new(),
                    });
                }
                Err(err) => {
                    let _ = tx.send(JobEvent::Failed {
                        id,
                        error: err.to_string(),
                    });
                }
            }
        });
    }

    /// Exports all discovered scene/prefab hierarchies using any [`HierarchyExportFormat`].
    pub fn spawn_export_all_hierarchies_format(
        &mut self,
        out_dir: PathBuf,
        format: HierarchyExportFormat,
        filter: Option<String>,
    ) {
        let Some(catalog) = self.catalog.as_ref().map(Arc::clone) else {
            return;
        };
        let handle = self
            .jobs
            .alloc_handle(format!("Exporting All Hierarchies ({})", format.label()));
        let tx = self.jobs.sender();
        let id = handle.id;
        self.active_job_handle = Some(handle);

        std::thread::spawn(move || {
            let opts = GltfSceneOptions::default();
            match export_all_hierarchies_with_format(
                &catalog.workspace,
                &out_dir,
                format,
                filter.as_deref(),
                None,
                true,
                &opts,
            ) {
                Ok(reports) => {
                    let _ = tx.send(JobEvent::ExportFinished {
                        id,
                        output_dir: out_dir,
                        files_written: reports.len(),
                        errors: Vec::new(),
                    });
                }
                Err(err) => {
                    let _ = tx.send(JobEvent::Failed {
                        id,
                        error: err.to_string(),
                    });
                }
            }
        });
    }

    /// Exports all discovered scene/prefab hierarchy `.glb`s from loaded files into `out_dir`.
    pub fn spawn_export_all_hierarchies(&mut self, out_dir: PathBuf, filter: Option<String>) {
        self.spawn_export_all_hierarchies_format(out_dir, HierarchyExportFormat::Glb, filter);
    }

    /// Prompts for a folder and exports recovered `dump.cs`, `DiffableCs/`, `script.json`, and `il2cpp.h`.
    pub fn export_il2cpp_dump_interactive(&mut self) {
        if let Some(dir) = rfd::FileDialog::new()
            .set_title("Choose Output Directory for IL2CPP Dump (`dump.cs`, `script.json`, `il2cpp.h`)")
            .pick_folder()
        {
            self.spawn_export_il2cpp_dump(dir);
        }
    }

    /// Worker-thread execution of full `il2cpp_dumper` dump export into `out_dir`.
    pub fn spawn_export_il2cpp_dump(&mut self, out_dir: PathBuf) {
        let Some(report) = self
            .scripts
            .as_ref()
            .and_then(|s| s.il2cpp_recovered.clone())
        else {
            self.status_banner =
                "No recovered IL2CPP binary + global-metadata.dat session available.".to_owned();
            return;
        };
        let cfg = Il2CppExportConfig {
            write_dummy_dlls: self.il2cpp_include_dummy_dlls,
            include_pseudo_c: self.il2cpp_include_pseudo_c,
            include_disassembly: self.il2cpp_include_disasm,
            disasm: self.il2cpp_disasm_opts,
            ..Default::default()
        };
        let handle = self
            .jobs
            .alloc_handle(format!("Exporting IL2CPP Dump -> {}", out_dir.display()));
        let tx = self.jobs.sender();
        let id = handle.id;
        self.active_job_handle = Some(handle);
        self.status_banner = format!("Running il2cpp_dumper export into {}...", out_dir.display());

        std::thread::spawn(move || {
            let mut last_step = String::new();
            let res = report.write_full_dump(&out_dir, &cfg, |msg| {
                if msg != last_step {
                    last_step = msg.to_owned();
                    let _ = tx.send(JobEvent::Progress {
                        id,
                        fraction: 0.45,
                        detail: msg.to_owned(),
                    });
                }
            });
            match res {
                Ok(files) => {
                    let _ = tx.send(JobEvent::Progress {
                        id,
                        fraction: 1.0,
                        detail: format!("Generated: {}", files.join(", ")),
                    });
                    let _ = tx.send(JobEvent::ExportFinished {
                        id,
                        output_dir: out_dir,
                        files_written: files.len(),
                        errors: Vec::new(),
                    });
                }
                Err(err) => {
                    let _ = tx.send(JobEvent::Failed {
                        id,
                        error: err.to_string(),
                    });
                }
            }
        });
    }

    /// Generates a complete standalone Bevy `0.20.1` zero-logic showcase project into `out_dir`.
    pub fn spawn_export_bevy_project(&mut self, out_dir: PathBuf) {
        if let Some(cat) = self.catalog.as_ref()
            && (Arc::strong_count(cat) > 1 || Arc::strong_count(&cat.workspace) > 1)
        {
            self.status_banner =
                "Background task still active — retry Export Bevy 0.20 Project in a moment."
                    .to_owned();
            return;
        }
        let Some(cat_arc) = self.catalog.take() else {
            return;
        };
        let bevy_opts = self.build_bevy_export_options();
        match Arc::try_unwrap(cat_arc).map(|c| Arc::try_unwrap(c.workspace)) {
            Ok(Ok(mut workspace)) => {
                let handle = self
                    .jobs
                    .alloc_handle(format!("Generating Bevy 0.20 Project `{}`", bevy_opts.project_name));
                let tx = self.jobs.sender();
                let id = handle.id;
                self.active_job_handle = Some(handle);
                self.status_banner = format!(
                    "Exporting real assets & generating Bevy 0.20 project at {}...",
                    out_dir.display()
                );

                std::thread::spawn(move || {
                    let _ = tx.send(JobEvent::Progress {
                        id,
                        fraction: 0.25,
                        detail: "Resolving bundles & exporting GLB/PNG/WAV/TTF assets...".to_owned(),
                    });
                    let export_res = export_bevy_project(&mut workspace, &out_dir, &bevy_opts);
                    let catalog = Arc::new(AssetCatalog::from_workspace(Arc::new(workspace)));
                    let _ = tx.send(JobEvent::WorkspaceLoaded { id, catalog });
                    match export_res {
                        Ok(rep) => {
                            let written = rep.manifest.scenes.len()
                                + rep.manifest.characters.len()
                                + rep.manifest.props.len()
                                + rep.manifest.audio.len()
                                + rep.manifest.fonts.len()
                                + rep.manifest.ui_sprites.len()
                                + rep.manifest.ui_panels.len()
                                + 4;
                            let _ = tx.send(JobEvent::ExportFinished {
                                id,
                                output_dir: rep.output_dir,
                                files_written: written,
                                errors: rep.warnings,
                            });
                        }
                        Err(err) => {
                            let _ = tx.send(JobEvent::Failed {
                                id,
                                error: err.to_string(),
                            });
                        }
                    }
                });
            }
            _ => {
                self.status_banner =
                    "Workspace is busy in another task — cannot lock workspace for Bevy bundle export."
                        .to_owned();
            }
        }
    }

    /// Exports the loaded workspace into a full Unity Editor project directory in a background thread.
    pub fn spawn_export_unity_project(&mut self, out_dir: PathBuf) {
        if let Some(cat) = self.catalog.as_ref()
            && (Arc::strong_count(cat) > 1 || Arc::strong_count(&cat.workspace) > 1)
        {
            self.status_banner =
                "Background task still active — retry Export Unity Project in a moment."
                    .to_owned();
            return;
        }
        let Some(cat_arc) = self.catalog.take() else {
            return;
        };
        let opts = UnityProjectExportOptions::default();
        match Arc::try_unwrap(cat_arc).map(|c| Arc::try_unwrap(c.workspace)) {
            Ok(Ok(mut workspace)) => {
                let handle = self.jobs.alloc_handle("Exporting Unity Editor Project");
                let tx = self.jobs.sender();
                let id = handle.id;
                self.active_job_handle = Some(handle);
                self.status_banner = format!("Exporting Unity Editor project to {}...", out_dir.display());

                std::thread::spawn(move || {
                    let _ = tx.send(JobEvent::Progress {
                        id,
                        fraction: 0.25,
                        detail: "Generating ProjectSettings, MonoScripts, Prefabs, Scenes & Assets..."
                            .to_owned(),
                    });
                    let export_res = export_unity_project(&mut workspace, &out_dir, &opts);
                    let catalog = Arc::new(AssetCatalog::from_workspace(Arc::new(workspace)));
                    let _ = tx.send(JobEvent::WorkspaceLoaded { id, catalog });
                    match export_res {
                        Ok(rep) => {
                            let _ = tx.send(JobEvent::ExportFinished {
                                id,
                                output_dir: rep.output_dir,
                                files_written: rep.files_written,
                                errors: rep.warnings,
                            });
                        }
                        Err(err) => {
                            let _ = tx.send(JobEvent::Failed {
                                id,
                                error: err.to_string(),
                            });
                        }
                    }
                });
            }
            _ => {
                self.status_banner =
                    "Workspace is busy in another task — cannot lock workspace for Unity project export."
                        .to_owned();
            }
        }
    }

    /// Prompts for a raw replacement `.bin` payload and saves a modified SerializedFile or `.ab` bundle.
    pub fn replace_object_payload_interactive(&mut self, asset_ref: AssetRef) {
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };
        let Some(loaded) = catalog.workspace.files.get(asset_ref.file_index) else {
            return;
        };
        let Some(sf_bytes) = loaded.raw_bytes.as_ref() else {
            self.status_banner =
                "Selected container has no retained SerializedFile raw byte buffer.".to_owned();
            return;
        };

        let Some(bin_path) = rfd::FileDialog::new()
            .set_title("Pick Replacement Object Raw Payload (.bin)")
            .pick_file()
        else {
            return;
        };
        let Ok(replacement) = std::fs::read(&bin_path) else {
            self.status_banner = format!("Failed reading {}", bin_path.display());
            return;
        };

        // Check whether a matching deferred `.ab` archive entry exists so sibling streams & cabinets stay intact.
        let unity_ver = if loaded.file.unity_version.is_empty() {
            "2022.3.0f1".to_owned()
        } else {
            loaded.file.unity_version.clone()
        };
        let default_name = format!("patched_{}.ab", asset_ref.path_id);
        let Some(out_path) = rfd::FileDialog::new()
            .set_file_name(&default_name)
            .save_file()
        else {
            return;
        };

        // Try direct bundle extraction if the virtual path matches a deferred archive entry or standalone `.ab` on disk.
        let source_bundle_bytes = find_source_bundle_bytes(&catalog.workspace, loaded.file_index);
        let result = if let Some(bundle_bytes) = source_bundle_bytes {
            replace_object_payload_in_bundle(
                &bundle_bytes,
                asset_ref.path_id,
                &replacement,
                &unity_ver,
                CompressionType::Lz4hc,
            )
        } else {
            replace_object_payload_in_serialized_file(sf_bytes, asset_ref.path_id, &replacement)
                .and_then(|patched_sf| {
                    let cab_name = Path::new(&loaded.virtual_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("CAB-patched")
                        .to_owned();
                    devx_exporter::repack::build_unityfs_bundle(
                        &unity_ver,
                        &[BundleFileEntry {
                            name: cab_name,
                            bytes: patched_sf,
                            is_serialized_file: true,
                        }],
                        CompressionType::Lz4hc,
                    )
                })
        };

        match result {
            Ok(bytes) => {
                if let Err(err) = std::fs::write(&out_path, &bytes) {
                    self.status_banner = format!("Failed writing repacked bundle: {err}");
                    self.host.log_error(&self.status_banner);
                } else {
                    let msg = format!(
                        "Patched PathID {} ({} -> {} bytes) and repacked bundle to {}.",
                        asset_ref.path_id,
                        self.selected_raw_bytes.as_ref().map_or(0, Vec::len),
                        replacement.len(),
                        out_path.display()
                    );
                    self.status_banner = msg.clone();
                    self.last_report = Some(msg.clone());
                    self.host.log_info(&msg);
                }
            }
            Err(err) => {
                self.status_banner = format!("Repack failed: {err}");
                self.host.log_error(&self.status_banner);
            }
        }
    }

    fn collect_batch_targets(&self, catalog: &AssetCatalog) -> Vec<AssetRef> {
        match self.batch_scope {
            BatchScope::AllAssets => catalog.entries.iter().map(|e| e.asset_ref).collect(),
            BatchScope::FilteredTree => catalog
                .entries
                .iter()
                .filter(|e| self.filter.matches(e))
                .map(|e| e.asset_ref)
                .collect(),
            BatchScope::SelectedClass => {
                if let Some(cid) = self.selected.and_then(|r| catalog.entry(r)).map(|e| e.summary.class_id)
                {
                    catalog
                        .entries
                        .iter()
                        .filter(|e| e.summary.class_id == cid)
                        .map(|e| e.asset_ref)
                        .collect()
                } else {
                    catalog
                        .entries
                        .iter()
                        .filter(|e| self.filter.matches(e))
                        .map(|e| e.asset_ref)
                        .collect()
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct UiViewPrefs {
    root_ref: AssetRef,
    ref_w: f32,
    ref_h: f32,
    zoom: f32,
    fit_to_view: bool,
    pan_offset: [f32; 2],
    show_outlines: bool,
    show_inactive: bool,
    show_labels: bool,
    show_checkerboard: bool,
    clip_to_canvas: bool,
    anim_playing: bool,
    anim_time: f32,
}

fn upload_texture_handle(
    ctx: &egui::Context,
    asset_ref: AssetRef,
    img: &DecodedImage,
    nearest: bool,
) -> egui::TextureHandle {
    let color_img = egui::ColorImage::from_rgba_unmultiplied(
        [img.width as usize, img.height as usize],
        &img.rgba8,
    );
    let opts = if nearest {
        egui::TextureOptions::NEAREST
    } else {
        egui::TextureOptions::LINEAR
    };
    ctx.load_texture(
        format!("devx_tex_{}_{}", asset_ref.file_index, asset_ref.path_id),
        color_img,
        opts,
    )
}

fn flatten_nodes(
    nodes: &[AssetTreeNode],
    depth: usize,
    prefix: &str,
    expanded_nodes: &HashSet<String>,
    force_expand: bool,
    out: &mut Vec<FlatTreeRow>,
) {
    for (idx, node) in nodes.iter().enumerate() {
        let key = format!("{prefix}{depth}/{idx}:{}", node.label);
        let has_children = !node.children.is_empty();
        let expanded = has_children && (force_expand || expanded_nodes.contains(&key));
        out.push(FlatTreeRow {
            key: key.clone(),
            depth,
            label: node.label.clone(),
            subtitle: node.subtitle.clone(),
            kind: node.kind.clone(),
            expanded,
            has_children,
        });
        if expanded {
            let child_prefix = format!("{key}|");
            flatten_nodes(
                &node.children,
                depth + 1,
                &child_prefix,
                expanded_nodes,
                force_expand,
                out,
            );
        }
    }
}

fn decode_material_preview(
    catalog: &AssetCatalog,
    mat_ref: AssetRef,
    material: DecodedMaterial,
    ctx: Option<&egui::Context>,
) -> PreviewContent {
    let shader_ref = (material.shader_path_id != 0).then(|| AssetRef {
        file_index: catalog
            .workspace
            .resolve_external_file(mat_ref.file_index, material.shader_file_id)
            .unwrap_or(mat_ref.file_index),
        path_id: material.shader_path_id,
    });
    let shader_name = shader_ref
        .and_then(|r| catalog.entry(r))
        .map(|e| e.summary.name.clone());

    let mut textures = Vec::with_capacity(material.textures.len().min(24));
    for slot in material.textures.iter().take(24) {
        let resolved_ref = if slot.texture_path_id != 0 {
            catalog
                .workspace
                .resolve_external_file(mat_ref.file_index, slot.texture_file_id)
                .map(|fid| AssetRef {
                    file_index: fid,
                    path_id: slot.texture_path_id,
                })
        } else {
            None
        };
        let mut texture_name = if slot.texture_path_id == 0 {
            "<Unbound>".to_owned()
        } else {
            format!("External ({}:{})", slot.texture_file_id, slot.texture_path_id)
        };
        let mut image = None;
        let mut texture = None;
        if let Some(tr) = resolved_ref {
            if let Some(entry) = catalog.entry(tr) {
                texture_name = entry.summary.name.clone();
            }
            if let Some((tl, ti)) = catalog.workspace.object_info(tr.file_index, tr.path_id)
                && ti.type_id == 28
                && let Ok(tobj) = read_object(tl, ti)
                && let Ok(img) = decode_texture2d(
                    &tobj,
                    &tl.file.unity_version,
                    &catalog.workspace.streams,
                    TextureDecodeOptions::default(),
                )
            {
                texture = ctx.map(|c| upload_texture_handle(c, tr, &img, false));
                image = Some(img);
            }
        }
        textures.push(MaterialTexturePreview {
            slot: slot.clone(),
            asset_ref: resolved_ref,
            texture_name,
            image,
            texture,
        });
    }

    PreviewContent::Material {
        material,
        shader_ref,
        shader_name,
        textures,
    }
}

fn resolve_clip_track_names(workspace: &UnityWorkspaceData, clip: &mut DecodedAnimationClip) {
    if clip.tracks.iter().all(|t| t.path_name.is_some()) {
        return;
    }
    let mut tos: HashMap<u32, String> = HashMap::new();
    for loaded in &workspace.files {
        for info in &loaded.file.objects {
            if info.type_id != 90 {
                continue;
            }
            if let Ok(obj) = read_object(loaded, info)
                && let Some(arr) = obj.class.properties().get("m_TOS").and_then(|v| v.as_array())
            {
                for pair in arr {
                    if let Some(m) = pair.as_object()
                        && let Some(hash) = m.get("first").and_then(extract_i64_field)
                        && let Some(path) = m.get("second").and_then(|v| v.as_str())
                    {
                        tos.entry(hash as u32).or_insert_with(|| path.to_owned());
                    }
                }
            }
        }
    }
    if tos.is_empty() {
        return;
    }
    for track in &mut clip.tracks {
        if track.path_name.is_none()
            && let Some(path) = tos.get(&track.path_hash)
        {
            track.path_name = Some(path.clone());
        }
    }
}

fn decode_sprite_atlas_preview(
    catalog: &AssetCatalog,
    atlas_ref: AssetRef,
    atlas_obj: &UnityObject,
    unity_version: &str,
    ctx: Option<&egui::Context>,
) -> PreviewContent {
    let props = atlas_obj.class.properties();
    let name = props
        .get("m_Name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| atlas_obj.name())
        .unwrap_or_else(|| "SpriteAtlas".to_owned());

    let mut tags = Vec::new();
    if let Some(tag) = props.get("m_Tag").and_then(|v| v.as_str())
        && !tag.is_empty()
    {
        tags.push(tag.to_owned());
    }

    let mut pages = Vec::new();
    let mut seen_pages = HashSet::new();
    if let Some(render_map) = props.get("m_RenderDataMap").and_then(|v| v.as_array()) {
        for pair_val in render_map {
            let Some(pair) = pair_val.as_array() else {
                continue;
            };
            let Some(entry_obj) = pair.get(1).and_then(|v| v.as_object()) else {
                continue;
            };
            let Some(tex_obj) = entry_obj.get("texture").and_then(|v| v.as_object()) else {
                continue;
            };
            let Some(fid) = tex_obj.get("m_FileID").and_then(extract_i32_field) else {
                continue;
            };
            let Some(pid) = tex_obj.get("m_PathID").and_then(extract_i64_field) else {
                continue;
            };
            if pid == 0 {
                continue;
            }
            let Some(target_file) = catalog
                .workspace
                .resolve_external_file(atlas_ref.file_index, fid)
            else {
                continue;
            };
            let tex_ref = AssetRef {
                file_index: target_file,
                path_id: pid,
            };
            if !seen_pages.insert(tex_ref) || pages.len() >= 8 {
                continue;
            }
            if let Some((tl, ti)) = catalog.workspace.object_info(tex_ref.file_index, tex_ref.path_id)
                && let Ok(tobj) = read_object(tl, ti)
                && let Ok(img) = decode_texture2d(
                    &tobj,
                    &tl.file.unity_version,
                    &catalog.workspace.streams,
                    TextureDecodeOptions::default(),
                )
            {
                let tex_name = tobj.name().unwrap_or_else(|| format!("Page_{pid}"));
                let handle = ctx.map(|c| upload_texture_handle(c, tex_ref, &img, false));
                pages.push(SpriteAtlasPagePreview {
                    name: tex_name,
                    asset_ref: Some(tex_ref),
                    image: img,
                    texture: handle,
                });
            }
        }
    }

    let packed_arr = props
        .get("m_PackedSprites")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let packed_count = packed_arr.len();
    let mut sprites = Vec::new();
    let tpk = shared_tpk_registry();
    let mut tex_cache: HashMap<(usize, i64), DecodedImage> = HashMap::new();

    for p in packed_arr.iter().take(48) {
        let Some(pobj) = p.as_object() else {
            continue;
        };
        let (Some(fid), Some(pid)) = (
            pobj.get("m_FileID").and_then(extract_i32_field),
            pobj.get("m_PathID").and_then(extract_i64_field),
        ) else {
            continue;
        };
        if pid == 0 {
            continue;
        }
        let Some(target_file) = catalog
            .workspace
            .resolve_external_file(atlas_ref.file_index, fid)
        else {
            continue;
        };
        let sp_ref = AssetRef {
            file_index: target_file,
            path_id: pid,
        };
        let sp_name = catalog
            .entry(sp_ref)
            .map(|e| e.summary.name.clone())
            .unwrap_or_else(|| format!("Sprite_{pid}"));
        let decoded = catalog
            .workspace
            .object_info(sp_ref.file_index, sp_ref.path_id)
            .and_then(|(sl, si)| read_object(sl, si).ok())
            .and_then(|sobj| {
                decode_sprite_cached(
                    &sobj,
                    sp_ref.file_index,
                    unity_version,
                    &catalog.workspace,
                    &*tpk,
                    &mut tex_cache,
                )
                .ok()
            });
        let (image, texture) = if let Some(sp) = decoded {
            let handle = ctx.map(|c| upload_texture_handle(c, sp_ref, &sp.image, false));
            (Some(sp.image), handle)
        } else {
            (None, None)
        };
        sprites.push(SpriteAtlasItemPreview {
            name: sp_name,
            asset_ref: sp_ref,
            image,
            texture,
        });
    }

    PreviewContent::SpriteAtlas {
        name,
        tags,
        packed_count,
        pages,
        sprites,
    }
}

fn find_source_bundle_bytes(workspace: &UnityWorkspaceData, file_index: usize) -> Option<Vec<u8>> {
    let loaded = workspace.files.get(file_index)?;
    // Case 1: standalone `.ab` / `.bundle` / `.unity3d` opened directly.
    if workspace.root_path.is_file() {
        let bytes = std::fs::read(&workspace.root_path).ok()?;
        if bytes.starts_with(b"UnityFS\0") {
            return Some(bytes);
        }
    }
    // Case 2: `.ab` file inside an unpacked directory workspace.
    if workspace.root_path.is_dir() {
        let vpath = Path::new(&loaded.virtual_path);
        for ancestor in vpath.ancestors() {
            let cand = workspace.root_path.join(ancestor);
            if cand.is_file() {
                let bytes = std::fs::read(&cand).ok()?;
                if bytes.starts_with(b"UnityFS\0") {
                    return Some(bytes);
                }
            }
        }
    }
    None
}
