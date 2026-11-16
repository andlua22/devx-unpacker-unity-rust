//! Main `DevXApp` workbench shell, `egui_tiles` docking layout, menu bar, toolbar,
//! status bar, drag-and-drop file loading, and headless UI tests.

use crate::state::{BatchScope, BottomTab, DevXState, DockPane};
use crate::views;
use devx_exporter::hierarchy::HierarchyExportFormat;
use egui::{Color32, MenuBar, RichText, ViewportCommand};
use egui_tiles::{Behavior, Linear, LinearDir, SimplificationOptions, Tile, TileId, Tiles, Tree, UiResponse};
use std::path::PathBuf;

/// Root desktop workbench application implementing `eframe::App`.
pub struct DevXApp {
    pub state: DevXState,
    pub dock_tree: Tree<DockPane>,
    last_dark_mode: Option<bool>,
}

impl DevXApp {
    /// Initializes workbench state, binds the `egui::Context` repaint hook, and optionally opens `initial_path`.
    pub fn new(ctx: &egui::Context, initial_path: Option<PathBuf>) -> Self {
        let mut state = DevXState::new();
        state.bind_repaint_hook(ctx);
        if let Some(path) = initial_path {
            state.open_path_async(path);
        }

        let dock_tree = build_default_dock_tree();
        Self {
            state,
            dock_tree,
            last_dark_mode: None,
        }
    }

    /// Creates a headless instance for automated UI & state testing.
    pub fn new_headless() -> Self {
        Self {
            state: DevXState::new(),
            dock_tree: build_default_dock_tree(),
            last_dark_mode: None,
        }
    }

    pub fn reset_dock_layout(&mut self) {
        self.dock_tree = build_default_dock_tree();
    }

    /// Renders a complete frame of the workbench UI into `ui`.
    pub fn render_ui(&mut self, ui: &mut egui::Ui) {
        self.state.poll_background_events();

        if self.last_dark_mode != Some(self.state.dark_mode) {
            ui.ctx().set_visuals(if self.state.dark_mode {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            self.last_dark_mode = Some(self.state.dark_mode);
        }

        self.handle_drag_and_drop(ui);
        self.render_top_bars(ui);
        self.render_status_bar(ui);

        egui::CentralPanel::default().show(ui, |ui| {
            let mut behavior = DockBehavior {
                state: &mut self.state,
            };
            self.dock_tree.ui(&mut behavior, ui);
        });
    }

    fn handle_drag_and_drop(&mut self, ui: &egui::Ui) {
        let dropped = ui.ctx().input(|i| i.raw.dropped_files.clone());
        if let Some(path) = dropped.into_iter().next().map(|f| f.path().to_path_buf()) {
            self.state.open_path_async(path);
        }

        let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            let rect = ui.ctx().content_rect();
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::unique("drop_overlay"),
            ));
            painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(15, 45, 85, 130));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop Unity game folder, .assets, .unity3d/.bundle, .apk/.ipa/.unitypackage to open",
                egui::FontId::proportional(20.0),
                Color32::WHITE,
            );
        }
    }

    fn render_top_bars(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("devx_top_bar").show(ui, |ui| {
            MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open File / Bundle / APK...").clicked() {
                        ui.close();
                        if let Some(p) = rfd::FileDialog::new().pick_file() {
                            self.state.open_path_async(p);
                        }
                    }
                    if ui.button("Open Game Directory...").clicked() {
                        ui.close();
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            self.state.open_path_async(p);
                        }
                    }
                    if ui.button("Extract Raw .ab from APK/Archive...").clicked() {
                        ui.close();
                        if let Some(src) = rfd::FileDialog::new().pick_file()
                            && let Some(dst) = rfd::FileDialog::new().pick_folder()
                        {
                            self.state.spawn_extract_raw_bundles(src, dst, None);
                        }
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                    }
                });

                ui.menu_button("View", |ui| {
                    for pane in [
                        DockPane::AssetTree,
                        DockPane::Preview,
                        DockPane::Inspector,
                        DockPane::BottomTools,
                    ] {
                        if let Some(tile_id) = self.find_pane_tile(pane) {
                            let mut visible = self.dock_tree.is_visible(tile_id);
                            if ui.checkbox(&mut visible, pane.title()).changed() {
                                self.dock_tree.set_visible(tile_id, visible);
                            }
                        }
                    }
                    ui.separator();
                    ui.checkbox(&mut self.state.dark_mode, "Dark Theme");
                    if ui.button("Reset Dock Layout").clicked() {
                        ui.close();
                        self.reset_dock_layout();
                    }
                });

                ui.menu_button("Export", |ui| {
                    let has_catalog = self.state.catalog.is_some();
                    if ui
                        .add_enabled(self.state.selected.is_some(), egui::Button::new("Export Selected Asset..."))
                        .clicked()
                    {
                        ui.close();
                        if let Some(r) = self.state.selected {
                            self.state.export_selected_interactive(r, None);
                        }
                    }
                    let is_hier = self.state.selected.and_then(|r| {
                        self.state
                            .catalog
                            .as_ref()?
                            .entry(r)
                            .map(|e| matches!(e.summary.class_id, 1 | 4 | 23 | 33 | 43 | 137 | 142 | 224))
                    }) == Some(true);
                    ui.add_enabled_ui(is_hier, |ui| {
                        ui.menu_button("Export Hierarchy / Scene", |ui| {
                            for (fmt, label) in [
                                (HierarchyExportFormat::Fbx, "Binary FBX (.fbx)..."),
                                (HierarchyExportFormat::Glb, "Binary glTF (.glb)..."),
                                (HierarchyExportFormat::Gltf, "glTF Folder (.gltf)..."),
                                (HierarchyExportFormat::Dae, "COLLADA (.dae)..."),
                                (HierarchyExportFormat::ObjMtl, "Wavefront OBJ (.obj + .mtl)..."),
                            ] {
                                if ui.button(label).clicked() {
                                    ui.close();
                                    if let Some(r) = self.state.selected {
                                        self.state.export_hierarchy_interactive(r, fmt);
                                    }
                                }
                            }
                            ui.separator();
                            for (is_scene, as_pkg, label) in [
                                (false, false, "Unity Prefab (.prefab)..."),
                                (true, false, "Unity Scene (.unity)..."),
                                (false, true, "UnityPackage (.unitypackage)..."),
                            ] {
                                if ui.button(label).clicked() {
                                    ui.close();
                                    if let Some(r) = self.state.selected {
                                        self.state.export_prefab_interactive(r, is_scene, as_pkg);
                                    }
                                }
                            }
                        });
                    });
                    ui.separator();
                    ui.add_enabled_ui(has_catalog, |ui| {
                        if ui.button("Export Bevy 0.20 Zero-Logic Project...").clicked() {
                            ui.close();
                            if let Some(dir) = rfd::FileDialog::new()
                                .set_title("Choose Output Directory for Bevy 0.20 Project")
                                .pick_folder()
                            {
                                self.state.spawn_export_bevy_project(dir);
                            }
                        }
                        if ui.button("Export Unity Editor Project...").clicked() {
                            ui.close();
                            if let Some(dir) = rfd::FileDialog::new()
                                .set_title("Choose Output Directory for Unity Editor Project")
                                .pick_folder()
                            {
                                self.state.spawn_export_unity_project(dir);
                            }
                        }
                        for (fmt, label) in [
                            (HierarchyExportFormat::Glb, "Export All Hierarchy GLBs..."),
                            (HierarchyExportFormat::Fbx, "Export All Hierarchy FBXs..."),
                            (HierarchyExportFormat::Dae, "Export All Hierarchy DAEs..."),
                        ] {
                            if ui.button(label).clicked() {
                                ui.close();
                                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                    self.state.spawn_export_all_hierarchies_format(dir, fmt, None);
                                }
                            }
                        }
                        ui.separator();
                        if ui.button("Batch Export All Assets...").clicked() {
                            ui.close();
                            self.state.batch_scope = BatchScope::AllAssets;
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.state.spawn_batch_export(dir);
                            }
                        }
                        if ui.button("Batch Export Filtered Assets...").clicked() {
                            ui.close();
                            self.state.batch_scope = BatchScope::FilteredTree;
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.state.spawn_batch_export(dir);
                            }
                        }
                        if ui.button("Extract Raw .ab Bundles...").clicked() {
                            ui.close();
                            if let Some(src) = self.state.workspace_path.clone()
                                && let Some(dir) = rfd::FileDialog::new().pick_folder()
                            {
                                self.state.spawn_extract_raw_bundles(src, dir, None);
                            }
                        }
                        if ui.button("Export .unitypackage...").clicked() {
                            ui.close();
                            if let Some(path) = rfd::FileDialog::new()
                                .set_file_name("exported_assets.unitypackage")
                                .save_file()
                            {
                                self.state.spawn_unitypackage_export(path);
                            }
                        }
                        if ui.button("Repack UnityFS Bundle (.unity3d)...").clicked() {
                            ui.close();
                            if let Some(path) = rfd::FileDialog::new()
                                .set_file_name("repacked.unity3d")
                                .save_file()
                            {
                                self.state.spawn_unityfs_repack(path);
                            }
                        }
                    });
                });
            });

            ui.separator();

            // Quick toolbar strip
            ui.horizontal_wrapped(|ui| {
                if ui.button("📂 Open File").clicked()
                    && let Some(p) = rfd::FileDialog::new().pick_file()
                {
                    self.state.open_path_async(p);
                }
                if ui.button("📁 Open Game Folder").clicked()
                    && let Some(p) = rfd::FileDialog::new().pick_folder()
                {
                    self.state.open_path_async(p);
                }
                if ui.button("📦 Extract .ab").clicked()
                    && let Some(src) = self
                        .state
                        .workspace_path
                        .clone()
                        .or_else(|| rfd::FileDialog::new().pick_file())
                    && let Some(dst) = rfd::FileDialog::new().pick_folder()
                {
                    self.state.spawn_extract_raw_bundles(src, dst, None);
                }

                ui.separator();

                if ui.button("🔍 Deep Search").clicked() {
                    self.state.bottom_tab = BottomTab::DeepSearch;
                    if let Some(id) = self.find_pane_tile(DockPane::BottomTools) {
                        self.dock_tree.set_visible(id, true);
                    }
                }
                if ui.button("📦 Batch Export").clicked() {
                    self.state.bottom_tab = BottomTab::BatchExport;
                    if let Some(id) = self.find_pane_tile(DockPane::BottomTools) {
                        self.dock_tree.set_visible(id, true);
                    }
                }
                if ui.button("🧬 Scripts & IL2CPP").clicked() {
                    self.state.bottom_tab = BottomTab::Scripts;
                    if let Some(id) = self.find_pane_tile(DockPane::BottomTools) {
                        self.dock_tree.set_visible(id, true);
                    }
                }

                if let Some((_, label, frac, detail)) = self.state.active_job.clone() {
                    ui.separator();
                    ui.add(
                        egui::ProgressBar::new(frac)
                            .desired_width(160.0)
                            .text(format!("{label}: {detail}")),
                    );
                    if let Some(ref handle) = self.state.active_job_handle
                        && ui.small_button("Cancel").clicked()
                    {
                        handle.cancel();
                        self.state.active_job = None;
                        self.state.status_banner = "Job cancellation requested.".to_owned();
                    }
                }
            });
        });
    }

    fn render_status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("devx_status_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&self.state.status_banner).small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("TPK Classes: {}", self.state.tpk_class_count));
                    if let Some(ref cat) = self.state.catalog {
                        ui.weak(format!(
                            "Assets: {} | Files: {} |",
                            cat.entries.len(),
                            cat.workspace.files.len()
                        ));
                    }
                });
            });
        });
    }

    fn find_pane_tile(&self, target: DockPane) -> Option<TileId> {
        self.dock_tree.tiles.iter().find_map(|(&id, tile)| {
            if matches!(tile, Tile::Pane(p) if *p == target) {
                Some(id)
            } else {
                None
            }
        })
    }
}

impl eframe::App for DevXApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render_ui(ui);
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        if let Some(gl) = gl {
            self.state.mesh_view.gpu.destroy(gl);
        }
    }
}

struct DockBehavior<'a> {
    state: &'a mut DevXState,
}

impl<'a> Behavior<DockPane> for DockBehavior<'a> {
    fn tab_title_for_pane(&mut self, pane: &DockPane) -> egui::WidgetText {
        pane.title().into()
    }

    fn simplification_options(&self) -> SimplificationOptions {
        SimplificationOptions {
            all_panes_must_have_tabs: true,
            ..Default::default()
        }
    }

    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        _tile_id: TileId,
        pane: &mut DockPane,
    ) -> UiResponse {
        match pane {
            DockPane::AssetTree => views::asset_tree::show(ui, self.state),
            DockPane::Preview => views::preview::show(ui, self.state),
            DockPane::Inspector => views::inspector::show(ui, self.state),
            DockPane::BottomTools => views::console_search::show(ui, self.state),
        }
        UiResponse::None
    }
}

fn build_default_dock_tree() -> Tree<DockPane> {
    let mut tiles = Tiles::default();
    let left_id = tiles.insert_pane(DockPane::AssetTree);
    let preview_id = tiles.insert_pane(DockPane::Preview);
    let bottom_id = tiles.insert_pane(DockPane::BottomTools);
    let right_id = tiles.insert_pane(DockPane::Inspector);

    let mut center_col = Linear::new(LinearDir::Vertical, vec![preview_id, bottom_id]);
    center_col.shares.set_share(preview_id, 0.72);
    center_col.shares.set_share(bottom_id, 0.28);
    let center_id = tiles.insert_container(center_col);

    let mut root_row = Linear::new(LinearDir::Horizontal, vec![left_id, center_id, right_id]);
    root_row.shares.set_share(left_id, 0.22);
    root_row.shares.set_share(center_id, 0.52);
    root_row.shares.set_share(right_id, 0.26);
    let root_id = tiles.insert_container(root_row);

    Tree::new(egui::Id::unique("devx_dock_tree"), root_id, tiles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_preview::{
        HierarchyPreviewData, ViewportMeshPart, attach_controller_to_preview,
        step_and_apply_animator_sim,
    };
    use crate::state::{
        MaterialTexturePreview, PreviewContent, PreviewTab, SpriteAtlasItemPreview,
        SpriteAtlasPagePreview,
    };
    use devx_codecs::animation::{
        DecodedAnimationClip, DecodedAnimationTrack, TrackProperty,
    };
    use devx_codecs::animator::{
        AnimatorClipLink, AnimatorLayerDef, AnimatorParamDef, AnimatorStateDef,
        AnimatorStateMachineDef, AnimatorTransitionCondition, AnimatorTransitionDef,
        DecodedAnimatorController,
    };
    use devx_codecs::audio::{DecodedAudioClip, decode_audio_clip};
    use devx_codecs::material::{DecodedCubemap, DecodedMaterial, MaterialTextureSlot};
    use devx_codecs::mesh::DecodedBlendShape;
    use devx_codecs::scene_extract::{
        SceneAnimClip, SceneAnimTrack, SceneDocument, SceneMesh, SceneNode,
    };
    use devx_plugin_api::{AssetRef, DecodedAudio, DecodedImage, DecodedMesh, SubMeshSlice};
    use devx_unity_io::{UnityWorkspaceData, load_path};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn step_headless(app: &mut DevXApp, ctx: &egui::Context) {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.render_ui(ui);
        });
        out.textures_delta.clear();
    }

    #[test]
    fn headless_workbench_renders_welcome_and_previews_without_panicking() {
        let mut app = DevXApp::new_headless();
        let ctx = egui::Context::default();

        // Frame 1: Empty welcome dashboard
        step_headless(&mut app, &ctx);

        // Exercise Texture preview + channels + YAML/JSON tabs
        let asset_ref = AssetRef {
            file_index: 0,
            path_id: 42,
        };
        app.state.selected = Some(asset_ref);
        app.state.selected_yaml =
            Some("%YAML 1.1\n--- !u!28 &42\nTexture2D:\n  m_Name: TestTex\n".into());
        app.state.selected_json = Some("{\"m_Name\": \"TestTex\"}".into());
        app.state.selected_raw_bytes = Some((0u8..=63).collect());
        app.state.preview = PreviewContent::Texture {
            image: DecodedImage {
                width: 4,
                height: 4,
                rgba8: vec![255; 4 * 4 * 4],
                format_name: "RGBA32".into(),
                mip_count: 1,
            },
            texture: None,
            dds_hint: None,
            sprite_meta: None,
        };

        for tab in PreviewTab::ALL {
            app.state.preview_tab = tab;
            step_headless(&mut app, &ctx);
        }

        // Verify GPU texture upload succeeded lazily in Media view
        assert!(matches!(
            app.state.preview,
            PreviewContent::Texture {
                texture: Some(_),
                ..
            }
        ));

        // Exercise interactive 3D mesh + textured Scene3D + bones viewport frames
        app.state.preview_tab = PreviewTab::Media;
        let sample_mesh = DecodedMesh {
            name: "Box".into(),
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            indices: vec![0, 1, 2],
            submeshes: vec![SubMeshSlice {
                first_index: 0,
                index_count: 3,
                topology: 0,
            }],
            bounds_min: [0.0, 0.0, 0.0],
            bounds_max: [1.0, 1.0, 0.0],
        };
        let morph_shapes = vec![DecodedBlendShape {
            name: "Smile".into(),
            vertex_indices: vec![2],
            position_deltas: vec![[0.0, 0.25, 0.0]],
            normal_deltas: vec![[0.0, 0.0, 0.0]],
            has_normals: false,
            default_weight: 0.5,
        }];
        app.state.preview = PreviewContent::Mesh {
            mesh: sample_mesh.clone(),
            base_mesh: Some(sample_mesh.clone()),
            blend_shapes: morph_shapes.clone(),
            blend_weights: vec![0.5],
        };
        app.state.mesh_view.use_gpu = true;
        step_headless(&mut app, &ctx);
        app.state.mesh_view.use_gpu = false;
        step_headless(&mut app, &ctx);

        let white_img = DecodedImage {
            width: 2,
            height: 2,
            rgba8: vec![255; 16],
            format_name: "RGBA32".into(),
            mip_count: 1,
        };
        let mut hidden_part = ViewportMeshPart::static_part(
            "Weapon".into(),
            sample_mesh.clone(),
            None,
            [0.8, 0.8, 0.9, 1.0],
        );
        hidden_part.visible = false;
        let sample_ctrl = DecodedAnimatorController {
            name: "HeroController".into(),
            parameters: vec![
                AnimatorParamDef {
                    name: "Speed".into(),
                    kind_label: "Float",
                    default_display: "0.00".into(),
                },
                AnimatorParamDef {
                    name: "Jump".into(),
                    kind_label: "Trigger",
                    default_display: "false".into(),
                },
            ],
            layers: vec![AnimatorLayerDef {
                name: "Base Layer".into(),
                state_machine_idx: 0,
                default_weight: 1.0,
                blending_mode: 0,
            }],
            state_machines: vec![AnimatorStateMachineDef {
                default_state: 0,
                states: vec![
                    AnimatorStateDef {
                        index: 0,
                        name: "Idle".into(),
                        tag: String::new(),
                        speed: 1.0,
                        is_loop: true,
                        is_default: true,
                        leaf_clip_indices: vec![0],
                        blend_tree: Vec::new(),
                        transitions: vec![AnimatorTransitionDef {
                            destination_state_idx: Some(1),
                            is_exit: false,
                            is_any_state: false,
                            has_exit_time: false,
                            exit_time: 0.9,
                            duration: 0.20,
                            conditions: vec![AnimatorTransitionCondition {
                                param_name: "Speed".into(),
                                mode_label: ">",
                                threshold: 0.1,
                            }],
                        }],
                    },
                    AnimatorStateDef {
                        index: 1,
                        name: "Run".into(),
                        tag: "Move".into(),
                        speed: 1.0,
                        is_loop: true,
                        is_default: false,
                        leaf_clip_indices: vec![1],
                        blend_tree: Vec::new(),
                        transitions: Vec::new(),
                    },
                ],
                any_state_transitions: Vec::new(),
            }],
            clip_links: vec![
                AnimatorClipLink {
                    index: 0,
                    asset_ref: Some(asset_ref),
                    name_hint: "Idle".into(),
                },
                AnimatorClipLink {
                    index: 1,
                    asset_ref: None,
                    name_hint: "Run".into(),
                },
            ],
        };
        let scene_doc = Arc::new(SceneDocument {
            name: "RiggedModel".into(),
            roots: vec![0],
            nodes: vec![SceneNode {
                name: "Body".into(),
                unity_ref: Some(asset_ref),
                parent: None,
                children: Vec::new(),
                translation: [0.0, 0.0, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [1.0, 1.0, 1.0],
                mesh_idx: Some(0),
                skin_idx: None,
                is_bone: false,
            }],
            meshes: vec![SceneMesh {
                name: "BodyMesh".into(),
                positions: sample_mesh.positions.clone(),
                normals: sample_mesh.normals.clone(),
                uvs: sample_mesh.uvs.clone(),
                indices: vec![0, 2, 1],
                submeshes: vec![devx_codecs::scene_extract::SceneSubMesh {
                    index_start: 0,
                    index_count: 3,
                    material_idx: None,
                }],
                joints: Vec::new(),
                weights: Vec::new(),
                bind_poses_row_major: Vec::new(),
                blend_shapes: morph_shapes.clone(),
            }],
            materials: Vec::new(),
            textures: Vec::new(),
            skins: Vec::new(),
            animations: vec![
                SceneAnimClip {
                    name: "Idle".into(),
                    duration_secs: 1.0,
                    sample_rate: 30.0,
                    tracks: vec![SceneAnimTrack {
                        node_idx: 0,
                        property: TrackProperty::Translation,
                        times: vec![0.0, 1.0],
                        values: vec![0.0, 0.0, 0.0, 0.0, 0.2, 0.0],
                    }],
                },
                SceneAnimClip {
                    name: "Run".into(),
                    duration_secs: 0.8,
                    sample_rate: 30.0,
                    tracks: vec![SceneAnimTrack {
                        node_idx: 0,
                        property: TrackProperty::Translation,
                        times: vec![0.0, 0.8],
                        values: vec![0.0, 1.0, 0.0, 0.0, 1.5, 0.0],
                    }],
                },
            ],
            avatar_tos: HashMap::new(),
        });
        let mut body_part = ViewportMeshPart::static_part(
            "Body".into(),
            sample_mesh,
            Some(Arc::new(white_img.clone())),
            [1.0, 0.9, 0.8, 1.0],
        )
        .with_blend_shapes(morph_shapes, Some(vec![1.0]));
        body_part.scene_node_idx = Some(0);
        let mut scene_preview = HierarchyPreviewData::static_from_parts(
            "RiggedModel".into(),
            asset_ref,
            vec![body_part, hidden_part],
            vec![([0.0, 0.0, 0.0], [0.0, 1.0, 0.0])],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
        );
        scene_preview.scene_doc = Some(scene_doc);
        scene_preview.show_parts_panel = true;
        scene_preview.show_blendshapes_panel = true;
        attach_controller_to_preview(
            &mut scene_preview,
            &UnityWorkspaceData::new(PathBuf::from("test")),
            sample_ctrl.clone(),
        );
        scene_preview.animator_mode = true;
        if let Some(ref mut sim) = scene_preview.animator_sim {
            sim.params
                .insert("Speed".into(), devx_codecs::animator::SimParamValue::Float(0.5));
        }
        let step = step_and_apply_animator_sim(&mut scene_preview, 0.05)
            .expect("animator sim step should succeed");
        assert_eq!(step.transition_target_idx, Some(1));
        // Blended Y with morph weight 1.0 (base y=1.0 + morph 0.25 + anim > 1.25)
        assert!(scene_preview.parts[0].mesh.positions[2][1] > 1.25);

        app.state.preview = PreviewContent::Scene3D(scene_preview);
        app.state.mesh_view.use_gpu = true;
        step_headless(&mut app, &ctx);
        app.state.mesh_view.use_gpu = false;
        step_headless(&mut app, &ctx);

        app.state.preview = PreviewContent::AnimatorController {
            controller: sample_ctrl,
            selected_layer: 0,
        };
        step_headless(&mut app, &ctx);

        app.state.preview = PreviewContent::Material {
            material: DecodedMaterial {
                name: "HeroMat".into(),
                shader_file_id: 0,
                shader_path_id: 10,
                render_queue: 2000,
                valid_keywords: vec!["_EMISSION".into()],
                string_tags: vec![("RenderType".into(), "Opaque".into())],
                textures: vec![MaterialTextureSlot {
                    property_name: "_MainTex".into(),
                    texture_file_id: 0,
                    texture_path_id: 11,
                    scale: [1.0, 1.0],
                    offset: [0.0, 0.0],
                }],
                colors: vec![("_Color".into(), [1.0, 1.0, 1.0, 1.0])],
                floats: vec![("_Metallic".into(), 0.2)],
                ints: Vec::new(),
            },
            shader_ref: None,
            shader_name: Some("Standard".into()),
            textures: vec![MaterialTexturePreview {
                slot: MaterialTextureSlot {
                    property_name: "_MainTex".into(),
                    texture_file_id: 0,
                    texture_path_id: 11,
                    scale: [1.0, 1.0],
                    offset: [0.0, 0.0],
                },
                asset_ref: None,
                texture_name: "Albedo".into(),
                image: Some(white_img.clone()),
                texture: None,
            }],
        };
        step_headless(&mut app, &ctx);

        app.state.preview = PreviewContent::AnimationClip {
            clip: DecodedAnimationClip {
                name: "Idle".into(),
                duration_secs: 1.0,
                sample_rate: 30.0,
                wrap_mode: 2,
                tracks: vec![DecodedAnimationTrack {
                    path_hash: 0x1234_5678,
                    path_name: Some("Root/Spine".into()),
                    property: TrackProperty::Translation,
                    times: vec![0.0, 0.5, 1.0],
                    values: vec![0.0, 0.0, 0.0, 0.0, 0.2, 0.0, 0.0, 0.0, 0.0],
                }],
                total_curves: 3,
                streamed_curves: 0,
                dense_curves: 3,
                constant_curves: 0,
                events: vec![(0.5, "Step".into())],
            },
            selected_track: 0,
        };
        step_headless(&mut app, &ctx);

        app.state.preview = PreviewContent::Cubemap {
            cubemap: DecodedCubemap {
                name: "Sky".into(),
                face_size: 2,
                format_name: "RGBA32".into(),
                cross_image: white_img.clone(),
                faces: vec![white_img.clone(); 6],
            },
            cross_texture: None,
            face_textures: Vec::new(),
            selected_face: Some(0),
        };
        step_headless(&mut app, &ctx);

        app.state.preview = PreviewContent::SpriteAtlas {
            name: "HudAtlas".into(),
            tags: vec!["UI".into()],
            packed_count: 1,
            pages: vec![SpriteAtlasPagePreview {
                name: "Page0".into(),
                asset_ref: None,
                image: white_img.clone(),
                texture: None,
            }],
            sprites: vec![SpriteAtlasItemPreview {
                name: "Btn".into(),
                asset_ref,
                image: Some(white_img.clone()),
                texture: None,
            }],
        };
        step_headless(&mut app, &ctx);

        // Exercise interactive 2D uGUI Canvas preview (9-slice Image, Text, outlines)
        {
            use devx_exporter::ui_canvas::{
                ExtractedUiCanvas, UiCanvasDocument, UiCanvasNode, UiCanvasStats, UiGraphic,
                UiImageType, UiScaleMode, UiTextAlignment, UiTextureAssetMeta,
                recompute_ui_layout,
            };
            let btn_key = "ui_0_1".to_owned();
            let mut ui_doc = UiCanvasDocument {
                name: "TestPanel".into(),
                root_ref: asset_ref,
                selected_ref: Some(asset_ref),
                source_bundle: "ui/testpanel.ab".into(),
                reference_width: 1920.0,
                reference_height: 1080.0,
                scale_mode: UiScaleMode::ScaleWithScreenSize,
                match_width_or_height: 0.5,
                root: UiCanvasNode {
                    name: "TestPanel".into(),
                    go_ref: asset_ref,
                    anchor_min: [0.0, 0.0],
                    anchor_max: [1.0, 1.0],
                    size_delta: [0.0, 0.0],
                    canvas_rect: [0.0, 0.0, 1920.0, 1080.0],
                    local_rect: [0.0, 0.0, 1920.0, 1080.0],
                    has_canvas: true,
                    canvas_sorting_order: Some(0),
                    children: vec![UiCanvasNode {
                        name: "PlayBtn".into(),
                        go_ref: AssetRef {
                            file_index: 0,
                            path_id: 2,
                        },
                        size_delta: [320.0, 90.0],
                        is_interactive: true,
                        widget_kind: Some("Button".into()),
                        graphic: Some(UiGraphic::Image {
                            color: [0.2, 0.6, 1.0, 1.0],
                            texture_key: Some(btn_key.clone()),
                            missing_texture: false,
                            image_type: UiImageType::Sliced,
                            preserve_aspect: false,
                            fill_amount: 1.0,
                            border: [1.0, 1.0, 1.0, 1.0],
                            raw_name: None,
                        }),
                        children: vec![UiCanvasNode {
                            name: "Label".into(),
                            go_ref: AssetRef {
                                file_index: 0,
                                path_id: 3,
                            },
                            anchor_min: [0.0, 0.0],
                            anchor_max: [1.0, 1.0],
                            size_delta: [0.0, 0.0],
                            graphic: Some(UiGraphic::Text {
                                text: "<b>PLAY</b>".into(),
                                color: [1.0, 1.0, 1.0, 1.0],
                                font_size: 32.0,
                                alignment: UiTextAlignment::MiddleCenter,
                                rich_text: true,
                                best_fit: false,
                                min_size: 10.0,
                                max_size: 40.0,
                                line_spacing: 1.0,
                                font_name: None,
                                font_ref: None,
                                outline_color: Some([0.0, 0.0, 0.0, 0.8]),
                            }),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                textures: vec![UiTextureAssetMeta {
                    key: btn_key.clone(),
                    name: "BtnTex".into(),
                    source_ref: asset_ref,
                    is_sprite: true,
                    width: white_img.width,
                    height: white_img.height,
                    border: [1.0, 1.0, 1.0, 1.0],
                    asset_path: None,
                }],
                script_bindings: Vec::new(),
                animations: Vec::new(),
                warnings: Vec::new(),
                stats: UiCanvasStats {
                    node_count: 3,
                    active_node_count: 3,
                    rect_transform_count: 3,
                    has_canvas_component: true,
                    graphic_count: 2,
                    active_graphic_count: 2,
                    image_count: 1,
                    text_count: 1,
                    interactive_count: 1,
                    resolved_texture_count: 1,
                    ..Default::default()
                },
            };
            recompute_ui_layout(&mut ui_doc, 1920.0, 1080.0);
            let mut images = std::collections::HashMap::new();
            images.insert(btn_key, white_img.clone());
            let mut ui_prev = crate::state::UiCanvasPreviewState::from_extracted(
                ExtractedUiCanvas {
                    document: ui_doc,
                    images,
                },
                Some(&ctx),
                false,
            );
            ui_prev.show_outlines = true;
            app.state.preview = PreviewContent::UiCanvas(Box::new(ui_prev));
            step_headless(&mut app, &ctx);
        }

        // Exercise Audio waveform + bottom tabs
        app.state.preview = PreviewContent::Audio {
            clip: DecodedAudioClip {
                name: "Beep".into(),
                primary: DecodedAudio {
                    sample_rate: 44100,
                    channels: 1,
                    samples_i16: vec![0, 16000, -16000, 0],
                    source_format: "PCM".into(),
                    duration_secs: 0.1,
                    raw_container: None,
                },
                sub_streams: Vec::new(),
                waveform_peaks: vec![(-0.5, 0.5), (-0.8, 0.8), (-0.2, 0.2)],
            },
            selected_stream: 0,
        };
        for btab in BottomTab::ALL {
            app.state.bottom_tab = btab;
            step_headless(&mut app, &ctx);
        }

        let _ = decode_audio_clip;
    }

    #[test]
    fn loads_unitypackage_and_renders_tree_headlessly() {
        let dir = std::env::temp_dir().join("devx_app_headless_pkg_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pkg_path = dir.join("sample.unitypackage");
        devx_exporter::unitypackage::write_unitypackage(
            &pkg_path,
            &[devx_exporter::unitypackage::UnityPackageInputEntry {
                asset_path: "Assets/Hello.txt".to_owned(),
                guid: None,
                asset_bytes: b"hello devx rust".to_vec(),
                meta_text: None,
            }],
        )
        .unwrap();

        let ws = load_path(&pkg_path).expect("load_path unitypackage");
        assert!(!ws.unitypackage_entries.is_empty());

        let catalog = Arc::new(devx_core::AssetCatalog::from_workspace(Arc::new(ws)));
        let mut app = DevXApp::new_headless();
        app.state.install_catalog(catalog, false);

        let ctx = egui::Context::default();
        step_headless(&mut app, &ctx);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
