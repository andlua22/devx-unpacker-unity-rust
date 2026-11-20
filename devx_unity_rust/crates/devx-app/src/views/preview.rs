//! Center Preview & 3D Viewport pane for Texture2D/Sprite, Mesh, Hierarchies,
//! Material, AnimationClip, Cubemap, SpriteAtlas, AudioClip, TextAsset, Shader, Font,
//! YAML/JSON, and virtualized Hex RAW views.

use crate::scene_preview::{
    HierarchyPreviewData, ViewportMeshPart, apply_scene_animation_pose,
    step_and_apply_animator_sim,
};
use crate::state::{
    DevXState, MeshViewportState, PreviewContent, PreviewTab, ShaderViewMode, UiCanvasPreviewState,
};
use devx_exporter::ui_canvas::{
    UiCanvasNode, UiGraphic, UiImageType, UiTextAlignment, recompute_ui_layout,
};
use devx_codecs::animation::{
    DecodedAnimationClip, DecodedUiAnimationClip, TrackProperty, UiAnimChannel,
};
use devx_codecs::animator::{
    AnimatorStateMachineDef, AnimatorTransitionDef, BlendTreeKind, DecodedAnimatorController,
    SimParamValue,
};
use devx_codecs::material::MaterialAlphaMode;
use devx_codecs::mesh::{DecodedBlendShape, apply_blend_shapes};
use devx_codecs::particle::DecodedParticleSystem;
use devx_codecs::terrain::DecodedTerrainData;
use devx_codecs::text_font::TextAssetKind;
use devx_codecs::texture::ChannelMask;
use devx_exporter::ExportAssetOptions;
use devx_exporter::hierarchy::HierarchyExportFormat;
use devx_plugin_api::{AssetRef, DecodedImage};
use egui::{Color32, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, pos2, vec2};
use egui_extras::syntax_highlighting::{CodeTheme, highlight};

enum PreviewAction {
    NavigateBack,
    SelectAsset(AssetRef),
    RefreshPreview,
    ExportInteractive(AssetRef, Option<ExportAssetOptions>),
    ExportHierarchyFormat(AssetRef, HierarchyExportFormat),
    ExportPrefab(AssetRef, bool, bool),
    AttachClipToScene(AssetRef),
    AttachControllerToScene(AssetRef),
    ExportBevyProject,
    ExportUnityProject,
    ExportAllHierarchies,
    ExportAllHierarchiesFormat(HierarchyExportFormat),
    ExtractRawBundles,
    BatchExportAll,
    SaveBytes(String, Vec<u8>),
    OpenFileDialog,
    OpenFolderDialog,
}

/// Renders the central Preview & Viewport pane.
pub fn show(ui: &mut egui::Ui, state: &mut DevXState) {
    let mut deferred: Option<PreviewAction> = None;

    // Header bar
    ui.horizontal(|ui| {
        if !state.selection_history.is_empty() && ui.button("← Back").clicked() {
            deferred = Some(PreviewAction::NavigateBack);
        }

        if let (Some(catalog), Some(asset_ref)) = (&state.catalog, state.selected)
            && let Some(entry) = catalog.entry(asset_ref)
        {
            ui.label(RichText::new(&entry.summary.name).strong().size(15.0));
            ui.weak(format!(
                "{} • #{}:{}",
                entry.summary.class_name, asset_ref.file_index, asset_ref.path_id
            ));
        } else {
            ui.label(RichText::new("DevX Unity Rust Workbench").strong().size(15.0));
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(asset_ref) = state.selected
                && ui.button("Export Asset...").clicked()
            {
                deferred = Some(PreviewAction::ExportInteractive(asset_ref, None));
            }
            for tab in PreviewTab::ALL.iter().rev() {
                ui.selectable_value(&mut state.preview_tab, *tab, tab.label());
            }
        });
    });

    ui.separator();

    if state.selected.is_none() {
        show_workspace_dashboard(ui, state, &mut deferred);
        apply_action(ui, state, deferred);
        return;
    }

    match state.preview_tab {
        PreviewTab::Auto => {
            show_auto_view(ui, state, &mut deferred);
        }
        PreviewTab::Overview => {
            show_overview_card(ui, state);
        }
        PreviewTab::Media => {
            show_media_view(ui, state, &mut deferred);
        }
        PreviewTab::YamlJson => {
            show_yaml_json_view(ui, state, &mut deferred);
        }
        PreviewTab::HexRaw => {
            show_hex_raw_view(ui, state, &mut deferred);
        }
    }

    apply_action(ui, state, deferred);
}

fn apply_action(ui: &egui::Ui, state: &mut DevXState, action: Option<PreviewAction>) {
    let ctx = ui.ctx();
    match action {
        Some(PreviewAction::NavigateBack) => state.navigate_back(Some(ctx)),
        Some(PreviewAction::SelectAsset(r)) => state.select_asset(r, true, Some(ctx)),
        Some(PreviewAction::RefreshPreview) => state.refresh_preview(Some(ctx)),
        Some(PreviewAction::ExportInteractive(r, opts)) => state.export_selected_interactive(r, opts),
        Some(PreviewAction::ExportHierarchyFormat(r, fmt)) => {
            state.export_hierarchy_interactive(r, fmt);
        }
        Some(PreviewAction::ExportPrefab(r, is_scene, as_pkg)) => {
            state.export_prefab_interactive(r, is_scene, as_pkg);
        }
        Some(PreviewAction::AttachClipToScene(clip_ref)) => {
            state.attach_clip_to_active_scene(clip_ref);
        }
        Some(PreviewAction::AttachControllerToScene(ctrl_ref)) => {
            state.attach_controller_to_active_scene(ctrl_ref);
        }
        Some(PreviewAction::ExportBevyProject) => {
            if let Some(dir) = rfd::FileDialog::new()
                .set_title("Choose Output Directory for Bevy 0.20 Project")
                .pick_folder()
            {
                state.spawn_export_bevy_project(dir);
            }
        }
        Some(PreviewAction::ExportUnityProject) => {
            if let Some(dir) = rfd::FileDialog::new()
                .set_title("Choose Output Directory for Unity Editor Project")
                .pick_folder()
            {
                state.spawn_export_unity_project(dir);
            }
        }
        Some(PreviewAction::ExportAllHierarchies) => {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                state.spawn_export_all_hierarchies(dir, None);
            }
        }
        Some(PreviewAction::ExportAllHierarchiesFormat(fmt)) => {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                state.spawn_export_all_hierarchies_format(dir, fmt, None);
            }
        }
        Some(PreviewAction::ExtractRawBundles) => {
            if let Some(src) = state.workspace_path.clone()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                state.spawn_extract_raw_bundles(src, dir, None);
            }
        }
        Some(PreviewAction::BatchExportAll) => {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                state.batch_scope = crate::state::BatchScope::AllAssets;
                state.spawn_batch_export(dir);
            }
        }
        Some(PreviewAction::SaveBytes(name, bytes)) => state.save_bytes_interactive(&name, &bytes),
        Some(PreviewAction::OpenFileDialog) => {
            if let Some(path) = rfd::FileDialog::new().pick_file() {
                state.open_path_async(path);
            }
        }
        Some(PreviewAction::OpenFolderDialog) => {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                state.open_path_async(path);
            }
        }
        None => {}
    }
}

fn show_workspace_dashboard(
    ui: &mut egui::Ui,
    state: &DevXState,
    deferred: &mut Option<PreviewAction>,
) {
    let Some(catalog) = state.catalog.as_ref() else {
        ui.vertical_centered(|ui| {
            ui.add_space(48.0);
            ui.heading("DevX Unpacker — Clean-Room Rust Edition");
            ui.add_space(8.0);
            ui.label("Open a Unity build folder, .assets, .splitN, .unity3d/.bundle, .apk/.xapk/.obb/.ipa, WebGL .unityweb, or .unitypackage.");
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.add_space(ui.available_width() * 0.25);
                if ui.button("Open File / Bundle / APK...").clicked() {
                    *deferred = Some(PreviewAction::OpenFileDialog);
                }
                if ui.button("Open Game Directory...").clicked() {
                    *deferred = Some(PreviewAction::OpenFolderDialog);
                }
            });
            ui.add_space(16.0);
            ui.weak("Tip: You can also drag and drop any Unity file or build folder directly onto this window.");
            ui.weak(format!(
                "Loaded embedded TPK database: {} Unity classes | {} registered plugins.",
                state.tpk_class_count,
                state.plugins.plugins().len()
            ));
        });
        return;
    };

    ScrollArea::vertical()
        .id_salt("dashboard_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.heading("Workspace Overview");
            let deferred_total = catalog.workspace.deferred_bundles.len();
            let deferred_loaded = catalog
                .workspace
                .deferred_bundles
                .iter()
                .filter(|d| d.loaded)
                .count();
            ui.label(format!(
                "Loaded Files: {} | Indexed Objects: {} | Deferred .ab: {}/{} | Script Artifacts: {} | TPK Classes: {}",
                catalog.workspace.files.len(),
                catalog.entries.len(),
                deferred_loaded,
                deferred_total,
                catalog.workspace.script_artifacts.len(),
                state.tpk_class_count
            ));
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button(RichText::new("🚀 Export Bevy 0.20 Project...").strong())
                    .clicked()
                {
                    *deferred = Some(PreviewAction::ExportBevyProject);
                }
                if ui.button("Export Unity Project...").clicked() {
                    *deferred = Some(PreviewAction::ExportUnityProject);
                }
                if ui.button("Export Hierarchy GLBs...").clicked() {
                    *deferred = Some(PreviewAction::ExportAllHierarchies);
                }
                if ui.button("Export Hierarchy FBXs...").clicked() {
                    *deferred = Some(PreviewAction::ExportAllHierarchiesFormat(
                        HierarchyExportFormat::Fbx,
                    ));
                }
                if deferred_total > 0 && ui.button("Extract All .ab...").clicked() {
                    *deferred = Some(PreviewAction::ExtractRawBundles);
                }
                if ui.button("Batch Export All...").clicked() {
                    *deferred = Some(PreviewAction::BatchExportAll);
                }
            });
            ui.separator();

            ui.label(RichText::new("Asset Class Distribution").strong());
            let mut classes: Vec<((i32, &str), usize)> = catalog
                .class_counts
                .iter()
                .map(|((id, name), &c)| ((*id, name.as_str()), c))
                .collect();
            classes.sort_by_key(|a| std::cmp::Reverse(a.1));
            let max_count = classes.first().map(|c| c.1).unwrap_or(1).max(1) as f32;

            egui::Grid::new("ws_class_counts")
                .striped(true)
                .num_columns(3)
                .show(ui, |ui| {
                    for ((cid, cls), count) in classes {
                        ui.label(format!("{cls} ({cid})"));
                        ui.monospace(count.to_string());
                        let frac = (count as f32 / max_count).clamp(0.02, 1.0);
                        ui.add(egui::ProgressBar::new(frac).desired_width(180.0));
                        ui.end_row();
                    }
                });

            ui.add_space(12.0);
            ui.label(RichText::new("Loaded Asset Containers").strong());
            egui::Grid::new("ws_files_grid")
                .striped(true)
                .num_columns(4)
                .show(ui, |ui| {
                    ui.strong("Index");
                    ui.strong("Virtual Path");
                    ui.strong("Unity Version");
                    ui.strong("Objects");
                    ui.end_row();
                    for f in &catalog.workspace.files {
                        ui.monospace(format!("#{}", f.file_index));
                        ui.monospace(&f.virtual_path);
                        ui.label(&f.file.unity_version);
                        ui.label(f.file.objects.len().to_string());
                        ui.end_row();
                    }
                });
        });
}

fn show_overview_card(ui: &mut egui::Ui, state: &DevXState) {
    let (Some(catalog), Some(asset_ref)) = (&state.catalog, state.selected) else {
        return;
    };
    let Some(entry) = catalog.entry(asset_ref) else {
        return;
    };
    let s = &entry.summary;
    egui::Grid::new("asset_summary_grid")
        .striped(true)
        .num_columns(2)
        .show(ui, |ui| {
            ui.strong("Name:");
            ui.label(&s.name);
            ui.end_row();
            ui.strong("Class:");
            ui.label(format!("{} (ClassID {})", s.class_name, s.class_id));
            ui.end_row();
            ui.strong("Location:");
            ui.monospace(format!(
                "File #{} ({}) : PathID {}",
                asset_ref.file_index, s.source_file, asset_ref.path_id
            ));
            ui.end_row();
            ui.strong("Container Path:");
            ui.monospace(s.container_path.as_deref().unwrap_or("<none>"));
            ui.end_row();
            ui.strong("Unity Version:");
            ui.label(&s.unity_version);
            ui.end_row();
            ui.strong("Serialized Size:");
            ui.label(format!("{} bytes (offset {})", s.byte_size, entry.byte_start));
            ui.end_row();
        });
}

fn show_auto_view(
    ui: &mut egui::Ui,
    state: &mut DevXState,
    deferred: &mut Option<PreviewAction>,
) {
    match &state.preview {
        PreviewContent::Structured { .. } => {
            show_overview_card(ui, state);
            ui.separator();
            show_yaml_json_view(ui, state, deferred);
        }
        _ => show_media_view(ui, state, deferred),
    }
}

fn show_media_view(
    ui: &mut egui::Ui,
    state: &mut DevXState,
    deferred: &mut Option<PreviewAction>,
) {
    let selected_ref = state.selected;
    let nearest = state.tex_nearest;
    let tex_opts = if nearest {
        egui::TextureOptions::NEAREST
    } else {
        egui::TextureOptions::LINEAR
    };

    match &mut state.preview {
        PreviewContent::Texture { image, texture, .. } => {
            if texture.is_none()
                && let Some(r) = selected_ref
            {
                let id = format!("devx_tex_{}_{}", r.file_index, r.path_id);
                *texture = Some(upload_decoded_tex(ui.ctx(), &id, image, tex_opts));
            }
        }
        PreviewContent::Scene3D(scene)
        | PreviewContent::Terrain { scene, .. }
        | PreviewContent::ParticleSystem { scene, .. } => {
            upload_scene_textures(ui.ctx(), scene, tex_opts);
        }
        PreviewContent::Material { textures, .. } => {
            for (idx, slot) in textures.iter_mut().enumerate() {
                if slot.texture.is_none()
                    && let Some(ref img) = slot.image
                {
                    let id = format!("devx_mat_slot_{}_{}", idx, slot.slot.property_name);
                    slot.texture = Some(upload_decoded_tex(ui.ctx(), &id, img, tex_opts));
                }
            }
        }
        PreviewContent::Cubemap {
            cubemap,
            cross_texture,
            face_textures,
            ..
        } => {
            if cross_texture.is_none() {
                *cross_texture = Some(upload_decoded_tex(
                    ui.ctx(),
                    &format!("devx_cubemap_cross_{}", cubemap.name),
                    &cubemap.cross_image,
                    tex_opts,
                ));
            }
            if face_textures.len() != cubemap.faces.len() {
                *face_textures = cubemap
                    .faces
                    .iter()
                    .enumerate()
                    .map(|(i, img)| {
                        Some(upload_decoded_tex(
                            ui.ctx(),
                            &format!("devx_cubemap_face_{}_{i}", cubemap.name),
                            img,
                            tex_opts,
                        ))
                    })
                    .collect();
            }
        }
        PreviewContent::SpriteAtlas { pages, sprites, .. } => {
            for (i, p) in pages.iter_mut().enumerate() {
                if p.texture.is_none() {
                    p.texture = Some(upload_decoded_tex(
                        ui.ctx(),
                        &format!("devx_atlas_page_{i}_{}", p.name),
                        &p.image,
                        tex_opts,
                    ));
                }
            }
            for (i, s) in sprites.iter_mut().enumerate() {
                if s.texture.is_none()
                    && let Some(ref img) = s.image
                {
                    s.texture = Some(upload_decoded_tex(
                        ui.ctx(),
                        &format!("devx_atlas_spr_{i}_{}", s.asset_ref.path_id),
                        img,
                        tex_opts,
                    ));
                }
            }
        }
        PreviewContent::UiCanvas(ui_state) => {
            for (key, img) in &ui_state.images {
                if !ui_state.textures.contains_key(key) && img.width > 0 && img.height > 0 {
                    ui_state.textures.insert(
                        key.clone(),
                        upload_decoded_tex(ui.ctx(), &format!("devx_ui_{key}"), img, tex_opts),
                    );
                }
            }
        }
        _ => {}
    }

    let is_scene = matches!(state.preview, PreviewContent::Scene3D(_));
    let workspace_clips = if is_scene {
        state.list_workspace_animation_clips()
    } else {
        Vec::new()
    };
    let workspace_controllers = if is_scene {
        state.list_workspace_animator_controllers()
    } else {
        Vec::new()
    };

    match &mut state.preview {
        PreviewContent::Empty => {
            ui.weak("No preview available.");
        }
        PreviewContent::Error(err) => {
            ui.colored_label(Color32::from_rgb(255, 110, 110), format!("Decode error: {err}"));
            ui.add_space(6.0);
            ui.weak("Check the YAML/JSON or Hex RAW tab for underlying bytes.");
        }
        PreviewContent::Structured { note } => {
            let note = note.clone();
            show_overview_card(ui, state);
            ui.add_space(8.0);
            ui.weak(note);
        }
        PreviewContent::Texture {
            image,
            texture,
            dds_hint,
            sprite_meta,
        } => {
            let mut need_refresh = false;
            ui.horizontal_wrapped(|ui| {
                ui.strong(format!(
                    "{}x{} • {} ({} mips)",
                    image.width, image.height, image.format_name, image.mip_count
                ));
                if let Some(hint) = dds_hint {
                    ui.weak(hint);
                }
                ui.separator();
                ui.checkbox(&mut state.tex_fit, "Fit");
                if ui.small_button("-").clicked() {
                    state.tex_fit = false;
                    state.tex_zoom = (state.tex_zoom * 0.8).max(0.1);
                }
                ui.label(format!("{:.0}%", state.tex_zoom * 100.0));
                if ui.small_button("+").clicked() {
                    state.tex_fit = false;
                    state.tex_zoom = (state.tex_zoom * 1.25).min(16.0);
                }
                if ui.checkbox(&mut state.tex_nearest, "Nearest").changed() {
                    need_refresh = true;
                }
                if ui
                    .checkbox(&mut state.tex_options.flip_vertical, "Flip Y")
                    .changed()
                {
                    need_refresh = true;
                }
                if ui
                    .checkbox(&mut state.tex_options.invert_alpha, "Invert Alpha")
                    .changed()
                {
                    need_refresh = true;
                }

                ui.separator();
                for (mask, lbl) in [
                    (ChannelMask::All, "RGBA"),
                    (ChannelMask::Red, "R"),
                    (ChannelMask::Green, "G"),
                    (ChannelMask::Blue, "B"),
                    (ChannelMask::Alpha, "A"),
                ] {
                    if ui
                        .selectable_value(&mut state.tex_options.channel_mask, mask, lbl)
                        .changed()
                    {
                        need_refresh = true;
                    }
                }

                if let Some(r) = selected_ref {
                    ui.separator();
                    for (id, lbl) in [
                        ("builtin.png", "PNG"),
                        ("builtin.jpg", "JPG"),
                        ("builtin.webp", "WEBP"),
                        ("builtin.bmp", "BMP"),
                        ("builtin.tga", "TGA"),
                        ("builtin.dds", "DDS"),
                    ] {
                        if ui.button(lbl).clicked() {
                            let mut opts = state.export_opts.clone();
                            opts.texture_exporter_id = id.to_owned();
                            opts.texture_flip_vertical = state.tex_options.flip_vertical;
                            *deferred = Some(PreviewAction::ExportInteractive(r, Some(opts)));
                        }
                    }
                }
            });

            if let Some(sm) = sprite_meta {
                ui.horizontal(|ui| {
                    ui.weak(format!(
                        "Sprite rect: [{:.0}, {:.0}, {:.0}x{:.0}] | Pivot: ({:.2}, {:.2}) | PPU: {:.1}",
                        sm.rect[0], sm.rect[1], sm.rect[2], sm.rect[3], sm.pivot[0], sm.pivot[1], sm.pixels_to_units
                    ));
                    if let Some(src) = sm.source_texture
                        && src.path_id != 0
                        && ui.link("Jump to Source Texture2D").clicked()
                    {
                        *deferred = Some(PreviewAction::SelectAsset(src));
                    }
                });
            }

            if need_refresh && deferred.is_none() {
                *deferred = Some(PreviewAction::RefreshPreview);
            }

            ui.separator();

            if let Some(tex) = texture {
                let avail = ui.available_size();
                let w = image.width.max(1) as f32;
                let h = image.height.max(1) as f32;
                let disp = if state.tex_fit {
                    let scale = (avail.x / w).min(avail.y / h).max(0.05);
                    vec2(w * scale, h * scale)
                } else {
                    vec2(w * state.tex_zoom, h * state.tex_zoom)
                };

                ScrollArea::both()
                    .id_salt("texture_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let (rect, _) = ui.allocate_exact_size(disp, Sense::hover());
                        paint_checkerboard(ui.painter(), rect, 10.0);
                        ui.painter().image(
                            tex.id(),
                            rect,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    });
            }
        }
        PreviewContent::Audio {
            clip,
            selected_stream,
        } => {
            let mut active_audio = &clip.primary;
            if let Some(sub) = clip.sub_streams.get(*selected_stream)
                && let Some(ref d) = sub.decoded
            {
                active_audio = d;
            }

            ui.horizontal_wrapped(|ui| {
                ui.strong(format!(
                    "{} • {} Hz • {} ch • {:.2}s",
                    active_audio.source_format,
                    active_audio.sample_rate,
                    active_audio.channels,
                    active_audio.duration_secs
                ));
                if clip.sub_streams.len() > 1 {
                    egui::ComboBox::from_id_salt("fsb_substream_combo")
                        .selected_text(format!("Stream #{}", *selected_stream))
                        .show_ui(ui, |ui| {
                            for (idx, sub) in clip.sub_streams.iter().enumerate() {
                                let label = sub
                                    .name
                                    .as_deref()
                                    .unwrap_or("Stream");
                                ui.selectable_value(
                                    selected_stream,
                                    idx,
                                    format!("#{idx}: {label} (.{})", sub.extension),
                                );
                            }
                        });
                }

                if let Some(r) = selected_ref {
                    ui.separator();
                    let playing_this =
                        state.audio_engine.active_asset() == Some(r) && state.audio_engine.is_active();
                    if ui.button("▶ Play").clicked() {
                        state.audio_engine.play_i16(
                            r,
                            active_audio.sample_rate,
                            active_audio.channels,
                            &active_audio.samples_i16,
                        );
                    }
                    if playing_this {
                        let lbl = if state.audio_engine.is_paused() {
                            "Resume"
                        } else {
                            "⏸ Pause"
                        };
                        if ui.button(lbl).clicked() {
                            state.audio_engine.toggle_pause();
                        }
                        if ui.button("⏹ Stop").clicked() {
                            state.audio_engine.stop();
                        }
                        ui.ctx().request_repaint();
                    }
                    let mut vol = state.audio_engine.volume();
                    if ui
                        .add(egui::Slider::new(&mut vol, 0.0..=1.2).text("Vol"))
                        .changed()
                    {
                        state.audio_engine.set_volume(vol);
                    }

                    ui.separator();
                    if ui.button("Export WAV").clicked() {
                        let mut opts = state.export_opts.clone();
                        opts.audio_exporter_id = "builtin.wav".to_owned();
                        *deferred = Some(PreviewAction::ExportInteractive(r, Some(opts)));
                    }
                    if ui.button("Export Original").clicked() {
                        let mut opts = state.export_opts.clone();
                        opts.audio_exporter_id = "builtin.audio_native".to_owned();
                        *deferred = Some(PreviewAction::ExportInteractive(r, Some(opts)));
                    }
                }
            });

            if let Some(err) = state.audio_engine.last_error() {
                ui.colored_label(Color32::YELLOW, err);
            }

            ui.separator();
            let wave_size = vec2(ui.available_width().max(120.0), 180.0);
            let (rect, _) = ui.allocate_exact_size(wave_size, Sense::hover());
            paint_waveform(ui.painter(), rect, &clip.waveform_peaks);
        }
        PreviewContent::Mesh {
            mesh,
            base_mesh,
            blend_shapes,
            blend_weights,
        } => {
            ui.horizontal_wrapped(|ui| {
                let morph_suffix = if blend_shapes.is_empty() {
                    String::new()
                } else {
                    format!(" • {} morph(s)", blend_shapes.len())
                };
                ui.strong(format!(
                    "{} • {} verts • {} tris • {} submesh(es){morph_suffix}",
                    mesh.name,
                    mesh.positions.len(),
                    mesh.indices.len() / 3,
                    mesh.submeshes.len()
                ));
                ui.separator();
                ui.checkbox(&mut state.mesh_view.use_gpu, "GPU");
                ui.checkbox(&mut state.mesh_view.shaded, "Shaded");
                ui.checkbox(&mut state.mesh_view.wireframe, "Wireframe");
                ui.checkbox(&mut state.mesh_view.backface_cull, "Cull Back");
                ui.checkbox(&mut state.mesh_view.show_axes, "Axes");
                if ui.button("Reset View").clicked() {
                    state.mesh_view.reset_camera();
                }

                if let Some(r) = selected_ref {
                    ui.separator();
                    for (id, lbl) in [
                        ("builtin.obj", "OBJ"),
                        ("builtin.fbx", "FBX"),
                        ("builtin.glb", "GLB"),
                        ("builtin.dae", "DAE"),
                        ("builtin.ply", "PLY"),
                        ("builtin.stl", "STL"),
                    ] {
                        if ui.button(lbl).clicked() {
                            let mut opts = state.export_opts.clone();
                            opts.mesh_exporter_id = id.to_owned();
                            *deferred = Some(PreviewAction::ExportInteractive(r, Some(opts)));
                        }
                    }
                }
            });

            if !blend_shapes.is_empty()
                && let Some(orig) = base_mesh.as_ref()
            {
                let mut changed = false;
                egui::CollapsingHeader::new(format!("BlendShapes ({})", blend_shapes.len()))
                    .id_salt("mesh_blendshapes_header")
                    .default_open(true)
                    .show(ui, |ui| {
                        changed = render_blendshape_sliders(
                            ui,
                            "mesh_morphs_scroll",
                            blend_shapes,
                            blend_weights,
                        );
                    });
                if changed {
                    let (pos, norm) = apply_blend_shapes(
                        &orig.positions,
                        &orig.normals,
                        blend_shapes,
                        blend_weights,
                    );
                    mesh.positions = pos;
                    mesh.normals = norm;
                }
            }

            ui.weak(format!(
                "Bounds: min({:.2}, {:.2}, {:.2}) max({:.2}, {:.2}, {:.2}) | Drag: Orbit • Shift/Right-Drag: Pan • Wheel: Zoom • Double-click: Reset",
                mesh.bounds_min[0],
                mesh.bounds_min[1],
                mesh.bounds_min[2],
                mesh.bounds_max[0],
                mesh.bounds_max[1],
                mesh.bounds_max[2],
            ));

            let single_part = [ViewportMeshPart::static_part(
                mesh.name.clone(),
                mesh.clone(),
                None,
                [0.85, 0.90, 1.0, 1.0],
            )];
            render_scene_viewport(
                ui,
                &single_part,
                &[],
                mesh.bounds_min,
                mesh.bounds_max,
                &mut state.mesh_view,
            );
        }
        PreviewContent::Scene3D(scene) => {
            render_scene_3d_view(
                ui,
                scene,
                &mut state.mesh_view,
                &workspace_clips,
                &workspace_controllers,
                deferred,
            );
        }
        PreviewContent::Material {
            material,
            shader_ref,
            shader_name,
            textures,
        } => {
            ScrollArea::vertical()
                .id_salt("material_preview_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.heading(&material.name);
                        ui.separator();
                        ui.label(format!("Queue: {}", material.render_queue));
                        let alpha_lbl = match material.alpha_mode() {
                            MaterialAlphaMode::Opaque => "Opaque".to_owned(),
                            MaterialAlphaMode::Mask { cutoff } => format!("Cutout ({cutoff:.2})"),
                            MaterialAlphaMode::Blend => "Transparent".to_owned(),
                        };
                        ui.weak(format!(
                            "Alpha: {alpha_lbl} | Metallic: {:.2} | Roughness: {:.2}{}",
                            material.metallic_factor(),
                            material.roughness_factor(),
                            if material.double_sided() { " | Double-Sided" } else { "" }
                        ));
                    });

                    ui.horizontal(|ui| {
                        ui.strong("Shader:");
                        if let Some(sref) = *shader_ref {
                            let label = shader_name
                                .as_deref()
                                .unwrap_or("Shader");
                            if ui
                                .link(format!("{label} (#{}:{})", sref.file_index, sref.path_id))
                                .clicked()
                            {
                                *deferred = Some(PreviewAction::SelectAsset(sref));
                            }
                        } else {
                            ui.weak("<Standard / Builtin or External>");
                        }
                    });

                    if !material.valid_keywords.is_empty() {
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.strong("Keywords:");
                            for kw in &material.valid_keywords {
                                ui.monospace(kw);
                            }
                        });
                    }

                    ui.separator();
                    ui.label(RichText::new(format!("Texture Slots ({})", textures.len())).strong());
                    ui.horizontal_wrapped(|ui| {
                        for slot_pv in textures {
                            ui.group(|ui| {
                                ui.set_min_width(175.0);
                                ui.vertical(|ui| {
                                    ui.strong(&slot_pv.slot.property_name);
                                    if let Some(tex) = &slot_pv.texture {
                                        let size = vec2(96.0, 96.0);
                                        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                                        paint_checkerboard(ui.painter(), rect, 8.0);
                                        ui.painter().image(
                                            tex.id(),
                                            rect,
                                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                            Color32::WHITE,
                                        );
                                    } else {
                                        ui.weak("<No Texture>");
                                    }
                                    if let Some(tr) = slot_pv.asset_ref {
                                        if ui.link(&slot_pv.texture_name).clicked() {
                                            *deferred = Some(PreviewAction::SelectAsset(tr));
                                        }
                                    } else {
                                        ui.weak(&slot_pv.texture_name);
                                    }
                                    ui.small(format!(
                                        "Tile ({:.2},{:.2}) Off ({:.2},{:.2})",
                                        slot_pv.slot.scale[0],
                                        slot_pv.slot.scale[1],
                                        slot_pv.slot.offset[0],
                                        slot_pv.slot.offset[1]
                                    ));
                                });
                            });
                        }
                    });

                    ui.separator();
                    ui.columns(2, |cols| {
                        cols[0].strong("Colors");
                        egui::Grid::new("mat_colors_grid")
                            .striped(true)
                            .show(&mut cols[0], |ui| {
                                for (k, c) in &material.colors {
                                    let rgba = Color32::from_rgba_unmultiplied(
                                        (c[0].clamp(0.0, 1.0) * 255.0) as u8,
                                        (c[1].clamp(0.0, 1.0) * 255.0) as u8,
                                        (c[2].clamp(0.0, 1.0) * 255.0) as u8,
                                        (c[3].clamp(0.0, 1.0) * 255.0) as u8,
                                    );
                                    let (r, _) = ui.allocate_exact_size(vec2(18.0, 14.0), Sense::hover());
                                    ui.painter().rect_filled(r, 2.0, rgba);
                                    ui.monospace(k);
                                    ui.monospace(format!("({:.2}, {:.2}, {:.2}, {:.2})", c[0], c[1], c[2], c[3]));
                                    ui.end_row();
                                }
                            });

                        cols[1].strong("Scalars & Tags");
                        egui::Grid::new("mat_floats_grid")
                            .striped(true)
                            .show(&mut cols[1], |ui| {
                                for (k, v) in &material.floats {
                                    ui.monospace(k);
                                    ui.monospace(format!("{v:.4}"));
                                    ui.end_row();
                                }
                                for (k, v) in &material.ints {
                                    ui.monospace(k);
                                    ui.monospace(format!("{v}"));
                                    ui.end_row();
                                }
                                for (k, v) in &material.string_tags {
                                    ui.monospace(format!("Tag:{k}"));
                                    ui.label(v);
                                    ui.end_row();
                                }
                            });
                    });
                });
        }
        PreviewContent::AnimationClip {
            clip,
            selected_track,
        } => {
            ui.horizontal_wrapped(|ui| {
                ui.strong(format!(
                    "{} • {:.3}s • {:.1} Hz • {} track(s) ({} total curves: S={} D={} C={})",
                    clip.name,
                    clip.duration_secs,
                    clip.sample_rate,
                    clip.tracks.len(),
                    clip.total_curves,
                    clip.streamed_curves,
                    clip.dense_curves,
                    clip.constant_curves
                ));
            });
            ui.separator();

            if clip.tracks.is_empty() {
                ui.weak("No decoded TRS tracks in this clip (inspect YAML/JSON for muscle/float curves).");
            } else {
                let clamped = (*selected_track).min(clip.tracks.len().saturating_sub(1));
                *selected_track = clamped;

                ui.columns(2, |cols| {
                    cols[0].strong("Animated Tracks (select to plot)");
                    ScrollArea::vertical()
                        .id_salt("anim_tracks_scroll")
                        .max_height(320.0)
                        .show(&mut cols[0], |ui| {
                            for (idx, tr) in clip.tracks.iter().enumerate() {
                                let target = tr
                                    .path_name
                                    .as_deref()
                                    .unwrap_or("<hash>");
                                let lbl = format!(
                                    "[{}] {target} ({:#010x}) — {} keys",
                                    tr.property.label(),
                                    tr.path_hash,
                                    tr.times.len()
                                );
                                if ui.selectable_label(*selected_track == idx, lbl).clicked() {
                                    *selected_track = idx;
                                }
                            }
                        });

                    if let Some(track) = clip.tracks.get(*selected_track) {
                        let target = track.path_name.as_deref().unwrap_or("<unresolved path>");
                        cols[1].strong(format!("{} — {target}", track.property.label()));
                        let plot_size = vec2(cols[1].available_width().max(180.0), 220.0);
                        let (rect, _) = cols[1].allocate_exact_size(plot_size, Sense::hover());
                        paint_anim_curves(cols[1].painter(), rect, clip, *selected_track);
                        cols[1].small(format!(
                            "Samples: {} | Legend: X=Red, Y=Green, Z=Blue{}",
                            track.times.len(),
                            if track.property == TrackProperty::Rotation {
                                ", W=Gold"
                            } else {
                                ""
                            }
                        ));
                    }
                });

                if !clip.events.is_empty() {
                    ui.separator();
                    ui.strong(format!("Animation Events ({})", clip.events.len()));
                    ui.horizontal_wrapped(|ui| {
                        for (t, fn_name) in &clip.events {
                            ui.monospace(format!("{t:.3}s -> {fn_name}"));
                        }
                    });
                }
            }
        }
        PreviewContent::AnimatorController {
            controller,
            selected_layer,
        } => {
            render_animator_controller_view(ui, controller, selected_layer, deferred);
        }
        PreviewContent::Cubemap {
            cubemap,
            cross_texture,
            face_textures,
            selected_face,
        } => {
            ui.horizontal_wrapped(|ui| {
                ui.strong(format!(
                    "{} • {}px/face • {} • {} face(s)",
                    cubemap.name,
                    cubemap.face_size,
                    cubemap.format_name,
                    cubemap.faces.len()
                ));
                ui.separator();
                if ui.selectable_label(selected_face.is_none(), "Horizontal Cross (4x3)").clicked() {
                    *selected_face = None;
                }
                let face_labels = ["+X", "-X", "+Y", "-Y", "+Z", "-Z"];
                for (i, label) in face_labels
                    .iter()
                    .enumerate()
                    .take(face_textures.len().min(6))
                {
                    if ui
                        .selectable_label(*selected_face == Some(i), *label)
                        .clicked()
                    {
                        *selected_face = Some(i);
                    }
                }
            });
            ui.separator();

            let active_tex = match *selected_face {
                Some(i) => face_textures.get(i).and_then(Option::as_ref),
                None => cross_texture.as_ref(),
            };
            let active_img = match *selected_face {
                Some(i) => cubemap.faces.get(i).unwrap_or(&cubemap.cross_image),
                None => &cubemap.cross_image,
            };
            if let Some(tex) = active_tex {
                let avail = ui.available_size();
                let w = active_img.width.max(1) as f32;
                let h = active_img.height.max(1) as f32;
                let scale = (avail.x / w).min(avail.y / h).max(0.05);
                let disp = vec2(w * scale, h * scale);
                let (rect, _) = ui.allocate_exact_size(disp, Sense::hover());
                paint_checkerboard(ui.painter(), rect, 10.0);
                ui.painter().image(
                    tex.id(),
                    rect,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        }
        PreviewContent::SpriteAtlas {
            name,
            tags,
            packed_count,
            pages,
            sprites,
        } => {
            ScrollArea::vertical()
                .id_salt("sprite_atlas_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.heading(name.as_str());
                        ui.separator();
                        ui.label(format!("Pages: {} | Packed Sprites: {packed_count}", pages.len()));
                        if !tags.is_empty() {
                            ui.weak(format!("Tag: {}", tags.join(", ")));
                        }
                    });
                    ui.separator();

                    if !pages.is_empty() {
                        ui.strong("Atlas Pages (click link to open Texture2D)");
                        ui.horizontal_wrapped(|ui| {
                            for page in pages {
                                ui.group(|ui| {
                                    ui.vertical(|ui| {
                                        if let Some(tex) = &page.texture {
                                            let (rect, _) = ui.allocate_exact_size(vec2(140.0, 140.0), Sense::hover());
                                            paint_checkerboard(ui.painter(), rect, 8.0);
                                            ui.painter().image(
                                                tex.id(),
                                                rect,
                                                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                                Color32::WHITE,
                                            );
                                        }
                                        if let Some(pr) = page.asset_ref {
                                            if ui.link(&page.name).clicked() {
                                                *deferred = Some(PreviewAction::SelectAsset(pr));
                                            }
                                        } else {
                                            ui.label(&page.name);
                                        }
                                    });
                                });
                            }
                        });
                        ui.separator();
                    }

                    ui.strong(format!("Packed Sprites Gallery (showing {} of {packed_count})", sprites.len()));
                    ui.horizontal_wrapped(|ui| {
                        for sp in sprites {
                            ui.group(|ui| {
                                ui.set_min_width(116.0);
                                ui.vertical(|ui| {
                                    if let Some(tex) = &sp.texture {
                                        let (rect, _) = ui.allocate_exact_size(vec2(72.0, 72.0), Sense::hover());
                                        paint_checkerboard(ui.painter(), rect, 6.0);
                                        ui.painter().image(
                                            tex.id(),
                                            rect,
                                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                            Color32::WHITE,
                                        );
                                    }
                                    if ui.link(&sp.name).clicked() {
                                        *deferred = Some(PreviewAction::SelectAsset(sp.asset_ref));
                                    }
                                });
                            });
                        }
                    });
                });
        }
        PreviewContent::Text { asset } => {
            ui.horizontal(|ui| {
                ui.strong(format!(
                    "{} ({}) • {} bytes",
                    asset.name,
                    asset.kind.label(),
                    asset.raw_bytes.len()
                ));
                if ui.button("Save File...").clicked() {
                    *deferred = Some(PreviewAction::SaveBytes(
                        format!("{}.{}", asset.name, asset.suggested_extension()),
                        asset.raw_bytes.clone(),
                    ));
                }
            });
            ui.separator();
            if let Some(text) = asset.text.as_deref()
                && asset.kind != TextAssetKind::Binary
            {
                let lang = match asset.kind {
                    TextAssetKind::Json => "json",
                    TextAssetKind::Xml => "xml",
                    TextAssetKind::Yaml => "yaml",
                    TextAssetKind::Lua => "lua",
                    TextAssetKind::CSharp | TextAssetKind::ShaderLab => "cs",
                    TextAssetKind::PlainText | TextAssetKind::Binary => "txt",
                };
                show_highlighted_code(ui, text, lang, "text_asset_scroll");
            } else {
                ui.weak("Binary TextAsset payload detected — switch to Hex RAW tab to inspect bytes.");
            }
        }
        PreviewContent::Shader { shader } => {
            ui.horizontal_wrapped(|ui| {
                ui.strong(format!("Shader: {}", shader.name));
                ui.weak(format!(
                    "{} platform(s) • {} subprogram(s)",
                    shader.platforms.len(),
                    shader.total_subprograms
                ));
                if let Some(ref fb) = shader.fallback_name {
                    ui.weak(format!("Fallback \"{fb}\""));
                }
                if let Some(ref ed) = shader.custom_editor {
                    ui.weak(format!("CustomEditor \"{ed}\""));
                }
                if ui.button("Export .shader").clicked() {
                    let export_text = if shader.decompiled_cg_text.is_empty() {
                        &shader.shaderlab_text
                    } else {
                        &shader.decompiled_cg_text
                    };
                    *deferred = Some(PreviewAction::SaveBytes(
                        format!("{}.shader", shader.name),
                        export_text.as_bytes().to_vec(),
                    ));
                }
            });

            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut state.shader_view_mode,
                    ShaderViewMode::DecompiledCg,
                    "Decompiled ShaderLab + CG/HLSL",
                );
                ui.selectable_value(
                    &mut state.shader_view_mode,
                    ShaderViewMode::ShaderLabMeta,
                    "ShaderLab Render States",
                );
                ui.selectable_value(
                    &mut state.shader_view_mode,
                    ShaderViewMode::SubPrograms,
                    format!("Subprograms ({})", shader.total_subprograms),
                );
            });

            if !shader.platforms.is_empty() {
                egui::CollapsingHeader::new(format!(
                    "Compiled Platform Blobs ({})",
                    shader.platforms.len()
                ))
                .default_open(false)
                .show(ui, |ui| {
                    for blob in &shader.platforms {
                        let kinds: Vec<&str> =
                            blob.detected_kinds.iter().map(|k| k.label()).collect();
                        ui.label(format!(
                            "{} (id={}): {} bytes • {} subprograms • {}",
                            blob.platform_name,
                            blob.platform_id,
                            blob.decompressed_bytes.len(),
                            blob.subprograms.len(),
                            kinds.join(", ")
                        ));
                    }
                });
            }
            ui.separator();

            match state.shader_view_mode {
                ShaderViewMode::DecompiledCg => {
                    let code = if shader.decompiled_cg_text.is_empty() {
                        &shader.shaderlab_text
                    } else {
                        &shader.decompiled_cg_text
                    };
                    show_highlighted_code(ui, code, "cs", "shader_cg_scroll");
                }
                ShaderViewMode::ShaderLabMeta => {
                    show_highlighted_code(ui, &shader.shaderlab_text, "cs", "shader_scroll");
                }
                ShaderViewMode::SubPrograms => {
                    ui.horizontal(|ui| {
                        ui.label("Filter:");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.shader_subprogram_filter)
                                .hint_text("keyword, Vertex, Fragment, SPIR-V, GLSL, DXBC..."),
                        );
                    });
                    let needle = state.shader_subprogram_filter.trim().to_ascii_lowercase();
                    ScrollArea::vertical()
                        .id_salt("shader_subprogs_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for plat in &shader.platforms {
                                let matching: Vec<_> = plat
                                    .subprograms
                                    .iter()
                                    .filter(|sp| {
                                        if needle.is_empty() {
                                            return true;
                                        }
                                        sp.stage_hint.to_ascii_lowercase().contains(&needle)
                                            || sp.gpu_program_name.to_ascii_lowercase().contains(&needle)
                                            || sp.bytecode_kind.label().to_ascii_lowercase().contains(&needle)
                                            || sp.keywords.iter().any(|k| k.to_ascii_lowercase().contains(&needle))
                                            || plat.platform_name.to_ascii_lowercase().contains(&needle)
                                    })
                                    .take(60)
                                    .collect();
                                if matching.is_empty() {
                                    continue;
                                }
                                egui::CollapsingHeader::new(format!(
                                    "{} — {} matching / {} total",
                                    plat.platform_name,
                                    matching.len(),
                                    plat.subprograms.len()
                                ))
                                .default_open(true)
                                .show(ui, |ui| {
                                    for sp in matching {
                                        let kw = if sp.keywords.is_empty() {
                                            "<default>".to_owned()
                                        } else {
                                            sp.keywords.join(" ")
                                        };
                                        let hdr = format!(
                                            "[seg {} blob {}] {} • {} ({}) • kw: {}",
                                            sp.segment_index,
                                            sp.blob_index,
                                            sp.stage_hint,
                                            sp.gpu_program_name,
                                            sp.bytecode_kind.label(),
                                            kw
                                        );
                                        egui::CollapsingHeader::new(hdr).show(ui, |ui| {
                                            if let Some(ref cg) = sp.cg_text {
                                                ui.strong("Approximated CG/HLSL:");
                                                ui.monospace(truncate_utf8_preview(cg, 16 * 1024));
                                                ui.separator();
                                                ui.strong("Original GLSL:");
                                            }
                                            ui.monospace(truncate_utf8_preview(&sp.source_text, 16 * 1024));
                                        });
                                    }
                                });
                            }
                        });
                }
            }
        }
        PreviewContent::Font { font } => {
            ui.horizontal(|ui| {
                ui.strong(format!(
                    "{} ({}) • size {:.1} • {} bytes",
                    font.name,
                    font.format.label(),
                    font.font_size,
                    font.data.len()
                ));
                if !font.data.is_empty() && ui.button("Save Font...").clicked() {
                    *deferred = Some(PreviewAction::SaveBytes(
                        format!("{}.{}", font.name, font.format.extension()),
                        font.data.clone(),
                    ));
                }
            });
            if !font.font_names.is_empty() {
                ui.label(format!("Fallback families: {}", font.font_names.join(", ")));
            }
            ui.add_space(16.0);
            ui.label(
                RichText::new("AaBbCcDdEeFfGg 0123456789 !@#$%^&*()")
                    .size(22.0),
            );
            ui.label("The quick brown fox jumps over the lazy dog.");
        }
        PreviewContent::Terrain { terrain, scene } => {
            render_terrain_view(ui, terrain, scene, &mut state.mesh_view);
        }
        PreviewContent::ParticleSystem { particle, scene } => {
            render_particle_system_view(ui, particle, scene, &mut state.mesh_view);
        }
        PreviewContent::UiCanvas(ui_state) => {
            render_ui_canvas_view(ui, ui_state, deferred);
        }
    }
}

fn render_ui_canvas_view(
    ui: &mut egui::Ui,
    ui_state: &mut UiCanvasPreviewState,
    deferred: &mut Option<PreviewAction>,
) {
    let doc = &mut ui_state.document;
    let s = &doc.stats;
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!(
            "uGUI Canvas {} ({}x{})",
            doc.name, doc.reference_width as u32, doc.reference_height as u32
        ));
        ui.separator();
        ui.weak(format!(
            "Nodes: {} ({} active) • Graphics: {} (Sprites/Tex: {}/{}, Text: {}) • Widgets: {} • Bindings: {}",
            s.node_count,
            s.active_node_count,
            s.graphic_count,
            s.resolved_texture_count,
            s.resolved_texture_count + s.missing_texture_count,
            s.text_count,
            s.interactive_count,
            doc.script_bindings.len()
        ));
        if s.truncated {
            ui.colored_label(Color32::from_rgb(245, 190, 80), "Truncated");
        }
        if !doc.warnings.is_empty() {
            ui.colored_label(
                Color32::from_rgb(245, 185, 90),
                format!("⚠ {} ext ref missing", doc.warnings.len()),
            )
            .on_hover_text(doc.warnings.join("\n"));
        }
    });

    ui.horizontal_wrapped(|ui| {
        let presets: &[(&str, f32, f32)] = &[
            ("1920x1080 (FHD)", 1920.0, 1080.0),
            ("1280x720 (HD)", 1280.0, 720.0),
            ("2560x1440 (QHD)", 2560.0, 1440.0),
            ("1080x1920 (Portrait)", 1080.0, 1920.0),
            ("800x600", 800.0, 600.0),
        ];
        let cur_label = format!(
            "{}x{}",
            doc.reference_width.round() as u32,
            doc.reference_height.round() as u32
        );
        egui::ComboBox::from_id_salt("ui_canvas_res_preset")
            .selected_text(cur_label)
            .show_ui(ui, |ui| {
                for &(label, w, h) in presets {
                    let selected = (doc.reference_width - w).abs() < 1.0
                        && (doc.reference_height - h).abs() < 1.0;
                    if ui.selectable_label(selected, label).clicked() {
                        recompute_ui_layout(doc, w, h);
                    }
                }
            });

        ui.checkbox(&mut ui_state.fit_to_view, "Fit");
        if !ui_state.fit_to_view {
            ui.add(
                egui::Slider::new(&mut ui_state.zoom, 0.1..=4.0)
                    .logarithmic(true)
                    .text("Zoom"),
            );
        }
        if ui.button("100%").clicked() {
            ui_state.fit_to_view = false;
            ui_state.zoom = 1.0;
            ui_state.pan_offset = [0.0, 0.0];
        }
        if ui.button("Reset View").clicked() {
            ui_state.fit_to_view = true;
            ui_state.zoom = 1.0;
            ui_state.pan_offset = [0.0, 0.0];
        }
        ui.checkbox(&mut ui_state.show_outlines, "Outlines");
        ui.checkbox(&mut ui_state.show_inactive, "Inactive");
        ui.checkbox(&mut ui_state.show_labels, "Text");
        ui.checkbox(&mut ui_state.show_checkerboard, "Checker");
        ui.checkbox(&mut ui_state.clip_to_canvas, "Clip Canvas");

        if ui.button("Save Layout .json").clicked()
            && let Ok(json_bytes) = serde_json::to_vec_pretty(doc)
        {
            let clean = doc
                .name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
                .collect::<String>();
            *deferred = Some(PreviewAction::SaveBytes(
                format!("{clean}_ui_panel.json"),
                json_bytes,
            ));
        }
    });

    let mut relayout = false;
    if let Some(sel_ref) = ui_state.document.selected_ref {
        if let Some(node) = find_ui_node_mut(&mut ui_state.document.root, sel_ref) {
            ui.horizontal_wrapped(|ui| {
                ui.strong(format!("Node: {}", node.name));
                if let Some(ref wk) = node.widget_kind {
                    ui.weak(format!("[{wk}]"));
                }
                if let Some(ref lg) = node.layout_group {
                    ui.weak(format!(
                        "Layout:{:?} sp=({:.0},{:.0})",
                        lg.kind, lg.spacing[0], lg.spacing[1]
                    ));
                }
                if ui.checkbox(&mut node.active_self, "Active").changed() {
                    relayout = true;
                }
                ui.label("Pos:");
                if ui
                    .add(egui::DragValue::new(&mut node.anchored_position[0]).speed(1.0))
                    .changed()
                    | ui
                        .add(egui::DragValue::new(&mut node.anchored_position[1]).speed(1.0))
                        .changed()
                {
                    relayout = true;
                }
                ui.label("Size:");
                if ui
                    .add(egui::DragValue::new(&mut node.size_delta[0]).speed(1.0))
                    .changed()
                    | ui
                        .add(egui::DragValue::new(&mut node.size_delta[1]).speed(1.0))
                        .changed()
                {
                    relayout = true;
                }
                if let Some(UiGraphic::Text { ref mut text, .. }) = node.graphic {
                    ui.label("Text:");
                    if ui
                        .add(egui::TextEdit::singleline(text).desired_width(180.0))
                        .changed()
                    {
                        relayout = true;
                    }
                }
                ui.weak(format!(
                    "rect=({:.0},{:.0} {:.0}x{:.0})",
                    node.canvas_rect[0],
                    node.canvas_rect[1],
                    node.canvas_rect[2],
                    node.canvas_rect[3]
                ));
            });
        }
        if !ui_state.document.script_bindings.is_empty() {
            egui::CollapsingHeader::new(format!(
                "Script Bindings ({})",
                ui_state.document.script_bindings.len()
            ))
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for b in ui_state.document.script_bindings.iter().take(128) {
                        let lbl = if let Some(slot) = b.slot_index {
                            format!("{}.{}[{}] -> {}", b.script_name, b.field_name, slot, b.target_name)
                        } else {
                            format!("{}.{} -> {}", b.script_name, b.field_name, b.target_name)
                        };
                        if ui.small_button(lbl).clicked() {
                            ui_state.document.selected_ref = Some(b.target_go_ref);
                            *deferred = Some(PreviewAction::SelectAsset(b.target_go_ref));
                        }
                    }
                });
            });
        }
        if let Some(first_clip) = ui_state.document.animations.first() {
            let dur = first_clip.duration_secs.max(0.05);
            if ui_state.anim_playing {
                let dt = ui.input(|i| i.stable_dt.clamp(0.0, 0.05));
                ui_state.anim_time = (ui_state.anim_time + dt) % dur;
                ui.ctx().request_repaint();
            }
            ui.horizontal_wrapped(|ui| {
                if ui
                    .small_button(if ui_state.anim_playing { "Pause UI Anim" } else { "Play UI Anim" })
                    .clicked()
                {
                    ui_state.anim_playing = !ui_state.anim_playing;
                }
                ui.add(
                    egui::Slider::new(&mut ui_state.anim_time, 0.0..=dur)
                        .fixed_decimals(2)
                        .text(format!("{} ({:.2}s)", first_clip.name, dur)),
                );
            });
        }
    }
    if relayout {
        let rw = ui_state.document.reference_width;
        let rh = ui_state.document.reference_height;
        recompute_ui_layout(&mut ui_state.document, rw, rh);
    }

    ui.separator();

    let status_reserve = 24.0;
    let avail = ui.available_size();
    let canvas_view_size = vec2(avail.x.max(160.0), (avail.y - status_reserve).max(160.0));
    let (viewport_rect, resp) = ui.allocate_exact_size(canvas_view_size, Sense::click_and_drag());

    let ref_w = ui_state.document.reference_width.max(1.0);
    let ref_h = ui_state.document.reference_height.max(1.0);
    let fit_scale = ((viewport_rect.width() - 24.0) / ref_w)
        .min((viewport_rect.height() - 24.0) / ref_h)
        .max(0.05);

    if ui_state.fit_to_view {
        ui_state.zoom = fit_scale;
        ui_state.pan_offset = [0.0, 0.0];
    }

    if resp.hovered() {
        let scroll_y: f32 = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll_y.abs() > 0.1 {
            ui_state.fit_to_view = false;
            let factor = (1.0_f32 + scroll_y * 0.002).clamp(0.75, 1.35);
            ui_state.zoom = (ui_state.zoom * factor).clamp(0.05, 8.0);
        }
    }
    if resp.dragged() {
        ui_state.fit_to_view = false;
        let delta = resp.drag_delta();
        ui_state.pan_offset[0] += delta.x;
        ui_state.pan_offset[1] += delta.y;
    }

    let scale = ui_state.zoom;
    let frame_size = vec2(ref_w * scale, ref_h * scale);
    let frame_center = viewport_rect.center() + vec2(ui_state.pan_offset[0], ui_state.pan_offset[1]);
    let frame_rect = Rect::from_center_size(frame_center, frame_size);

    let vp_painter = ui.painter_at(viewport_rect);
    vp_painter.rect_filled(viewport_rect, 0.0, Color32::from_rgb(24, 27, 32));
    if ui_state.show_checkerboard {
        let checker_rect = frame_rect.intersect(viewport_rect);
        if checker_rect.is_positive() {
            paint_checkerboard(&vp_painter, checker_rect, 14.0);
        }
    } else {
        vp_painter.rect_filled(frame_rect, 0.0, Color32::from_rgb(36, 40, 48));
    }
    vp_painter.rect_stroke(
        frame_rect,
        0.0,
        Stroke::new(1.5, Color32::from_rgb(95, 130, 175)),
        egui::StrokeKind::Outside,
    );

    let canvas_painter_storage;
    let canvas_painter = if ui_state.clip_to_canvas {
        canvas_painter_storage = vp_painter.with_clip_rect(viewport_rect.intersect(frame_rect));
        &canvas_painter_storage
    } else {
        &vp_painter
    };

    let ctx_view = UiPaintCtx {
        frame_min: frame_rect.min,
        scale,
        show_outlines: ui_state.show_outlines,
        show_inactive: ui_state.show_inactive,
        show_labels: ui_state.show_labels,
        selected_ref: ui_state.document.selected_ref,
        textures: &ui_state.textures,
        animations: &ui_state.document.animations,
        anim_time: ui_state.anim_time,
    };
    let mut hits: Vec<UiNodeHit> = Vec::new();
    paint_ui_node_recursive(
        canvas_painter,
        &ui_state.document.root,
        "",
        &ctx_view,
        &mut hits,
    );

    // Interactive hover & click selection (prefer topmost graphic/widget, else last drawn node)
    let pointer_pos = resp.hover_pos().or_else(|| {
        if resp.clicked() {
            resp.interact_pointer_pos()
        } else {
            None
        }
    });
    ui_state.hovered_info = None;
    let mut hit_target: Option<&UiNodeHit> = None;
    if let Some(pos) = pointer_pos {
        hit_target = hits
            .iter()
            .rev()
            .find(|h| h.has_visual_or_widget && h.rect.contains(pos))
            .or_else(|| hits.iter().rev().find(|h| h.rect.contains(pos)));
        if let Some(hit) = hit_target {
            vp_painter.rect_stroke(
                hit.rect,
                0.0,
                Stroke::new(1.5, Color32::from_rgb(90, 220, 255)),
                egui::StrokeKind::Inside,
            );
            ui_state.hovered_info = Some(hit.summary.clone());
        }
    }
    if resp.clicked()
        && let Some(hit) = hit_target
    {
        ui_state.document.selected_ref = Some(hit.go_ref);
        *deferred = Some(PreviewAction::SelectAsset(hit.go_ref));
    }

    ui.horizontal(|ui| {
        if let Some(ref info) = ui_state.hovered_info {
            ui.label(RichText::new(info).small());
        } else if let Some(sel_ref) = ui_state.document.selected_ref {
            ui.weak(format!(
                "Selected node #{}:{} — drag to pan, scroll to zoom, click any UI element to inspect.",
                sel_ref.file_index, sel_ref.path_id
            ));
        } else {
            ui.weak("Drag to pan, scroll to zoom, click any UI element to inspect.");
        }
    });
}

struct UiPaintCtx<'a> {
    frame_min: Pos2,
    scale: f32,
    show_outlines: bool,
    show_inactive: bool,
    show_labels: bool,
    selected_ref: Option<AssetRef>,
    textures: &'a std::collections::HashMap<String, egui::TextureHandle>,
    animations: &'a [DecodedUiAnimationClip],
    anim_time: f32,
}

#[derive(Clone, Copy)]
struct SampledUiNodeAnim {
    dx: f32,
    dy: f32,
    rot_z_deg: f32,
    sx: f32,
    sy: f32,
    alpha: Option<f32>,
    active: Option<bool>,
}

fn eval_ui_track_scalar(times: &[f32], values: &[f32], t: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    if times.len() <= 1 || values.len() <= 1 {
        return values[0];
    }
    let idx = times.partition_point(|&kt| kt <= t).saturating_sub(1);
    let i0 = idx.min(values.len() - 1);
    let i1 = (i0 + 1).min(values.len() - 1);
    if i0 == i1 {
        return values[i0];
    }
    let dt = (times[i1] - times[i0]).max(1e-6);
    let f = ((t - times[i0]) / dt).clamp(0.0, 1.0);
    values[i0] + (values[i1] - values[i0]) * f
}

fn sample_ui_anim_for_path(
    clips: &[DecodedUiAnimationClip],
    path: &str,
    t: f32,
    node: &UiCanvasNode,
) -> SampledUiNodeAnim {
    let mut out = SampledUiNodeAnim {
        dx: 0.0,
        dy: 0.0,
        rot_z_deg: 0.0,
        sx: 1.0,
        sy: 1.0,
        alpha: None,
        active: None,
    };
    let Some(clip) = clips.first() else {
        return out;
    };
    let clip_t = if clip.duration_secs > 1e-4 {
        t % clip.duration_secs
    } else {
        0.0
    };
    for tr in &clip.tracks {
        if tr.path_name.as_deref() != Some(path) {
            continue;
        }
        let v = eval_ui_track_scalar(&tr.times, &tr.values, clip_t);
        match tr.channel {
            // Native curves contain absolute local property values. Convert to differences
            // from the solved prefab before mapping them through its parent's canvas basis.
            UiAnimChannel::AnchoredPosX => out.dx = v - node.anchored_position[0],
            UiAnimChannel::AnchoredPosY => out.dy = v - node.anchored_position[1],
            UiAnimChannel::LocalEulerZDeg => out.rot_z_deg = -v - node.rotation_z_deg,
            UiAnimChannel::LocalScaleX => out.sx = if node.scale[0].abs() > 1e-5 { v / node.scale[0] } else { 1.0 },
            UiAnimChannel::LocalScaleY => out.sy = if node.scale[1].abs() > 1e-5 { v / node.scale[1] } else { 1.0 },
            UiAnimChannel::ColorAlpha => out.alpha = Some(v.clamp(0.0, 1.0)),
            UiAnimChannel::Active => out.active = Some(v >= 0.5),
            UiAnimChannel::SizeDeltaX | UiAnimChannel::SizeDeltaY => {}
        }
    }
    out
}

struct UiNodePaintGeometry {
    bounds: Rect,
    artwork: Rect,
    rotation_deg: f32,
    flip_x: bool,
    flip_y: bool,
}

fn ui_node_paint_geometry(node: &UiCanvasNode, anim: SampledUiNodeAnim) -> UiNodePaintGeometry {
    let rect = |[x, y, w, h]: [f32; 4]| Rect::from_min_size(pos2(x, y), vec2(w, h));
    let mut bounds = rect(node.canvas_rect);
    // The bounds are for picking and clipping. Rotating their dimensions again distorts
    // non-square artwork; the exporter retains the original axis lengths separately.
    let mut artwork = rect(node.canvas_render_rect.unwrap_or(node.canvas_rect));
    let base_rotation = node.canvas_rotation_z_deg.unwrap_or(node.rotation_z_deg);
    let [bx, by] = node.canvas_parent_basis.unwrap_or([[1.0, 0.0], [0.0, -1.0]]);
    let translation = vec2(bx[0] * anim.dx + by[0] * anim.dy, bx[1] * anim.dx + by[1] * anim.dy);
    // A reflected parent reverses the direction of a local rotation in screen space.
    let rotation_sign = if bx[0] * by[1] - bx[1] * by[0] > 0.0 { -1.0 } else { 1.0 };
    let rotation_deg = base_rotation + rotation_sign * anim.rot_z_deg;
    if (anim.sx - 1.0).abs() > 1e-3 || (anim.sy - 1.0).abs() > 1e-3 || anim.rot_z_deg.abs() > 0.01 {
        let pivot_offset = vec2(
            artwork.width() * (node.pivot[0] - 0.5) * if node.flip_x { -1.0 } else { 1.0 },
            artwork.height() * (0.5 - node.pivot[1]) * if node.flip_y { -1.0 } else { 1.0 },
        );
        let pivot = artwork.center() + rotate_ui_vector(pivot_offset, base_rotation);
        let center = pivot - rotate_ui_vector(vec2(pivot_offset.x * anim.sx, pivot_offset.y * anim.sy), rotation_deg);
        artwork = Rect::from_center_size(center, vec2(artwork.width() * anim.sx.abs(), artwork.height() * anim.sy.abs()));
        let (sin, cos) = rotation_deg.to_radians().sin_cos();
        bounds = Rect::from_center_size(center, vec2(
            artwork.width() * cos.abs() + artwork.height() * sin.abs(),
            artwork.width() * sin.abs() + artwork.height() * cos.abs(),
        ));
    }
    UiNodePaintGeometry {
        bounds: bounds.translate(translation),
        artwork: artwork.translate(translation),
        rotation_deg,
        flip_x: node.flip_x ^ (anim.sx < 0.0),
        flip_y: node.flip_y ^ (anim.sy < 0.0),
    }
}

fn rotate_ui_vector(delta: egui::Vec2, rotation_deg: f32) -> egui::Vec2 {
    let (sin, cos) = rotation_deg.to_radians().sin_cos();
    vec2(delta.x * cos - delta.y * sin, delta.x * sin + delta.y * cos)
}

struct UiNodeHit {
    go_ref: AssetRef,
    rect: Rect,
    has_visual_or_widget: bool,
    summary: String,
}

fn find_ui_node_mut(node: &mut UiCanvasNode, target: AssetRef) -> Option<&mut UiCanvasNode> {
    if node.go_ref == target {
        return Some(node);
    }
    for ch in &mut node.children {
        if let Some(found) = find_ui_node_mut(ch, target) {
            return Some(found);
        }
    }
    None
}

fn union_active_child_canvas_rect(node: &UiCanvasNode) -> Option<[f32; 4]> {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for ch in &node.children {
        if !ch.active_self {
            continue;
        }
        let [cx, cy, cw, ch_h] = if ch.canvas_rect[2] >= 1.0 && ch.canvas_rect[3] >= 1.0 {
            ch.canvas_rect
        } else if let Some(sub) = union_active_child_canvas_rect(ch) {
            sub
        } else {
            continue;
        };
        min_x = min_x.min(cx);
        min_y = min_y.min(cy);
        max_x = max_x.max(cx + cw);
        max_y = max_y.max(cy + ch_h);
    }
    if min_x < max_x && min_y < max_y {
        Some([min_x, min_y, max_x - min_x, max_y - min_y])
    } else {
        None
    }
}

fn paint_ui_node_recursive(
    painter: &egui::Painter,
    node: &UiCanvasNode,
    path: &str,
    ctx: &UiPaintCtx<'_>,
    hits: &mut Vec<UiNodeHit>,
) {
    let anim = sample_ui_anim_for_path(ctx.animations, path, ctx.anim_time, node);
    let node_active = anim.active.unwrap_or(node.active_in_hierarchy);
    if !ctx.show_inactive && !node_active {
        return;
    }

    let geometry = ui_node_paint_geometry(node, anim);
    let mut bounds = geometry.bounds;
    if (node.canvas_rect[2] < 1.0 || node.canvas_rect[3] < 1.0)
        && node.is_interactive
        && node.graphic.is_none()
        && let Some([x, y, w, h]) = union_active_child_canvas_rect(node)
    {
        bounds = Rect::from_min_size(pos2(x, y), vec2(w, h));
    }
    let to_screen = |rect: Rect| Rect::from_min_size(
        ctx.frame_min + rect.min.to_vec2() * ctx.scale,
        rect.size() * ctx.scale,
    );
    let screen_rect = to_screen(bounds);
    let artwork_rect = to_screen(geometry.artwork);
    let [rx, ry, rw, rh] = [bounds.min.x, bounds.min.y, bounds.width(), bounds.height()];
    let eff_alpha = if node_active {
        anim.alpha.unwrap_or(node.effective_alpha)
    } else {
        (node.effective_alpha * 0.35).max(0.15)
    };
    let draw_graphic = node.graphic_enabled && (!node.clips_children || node.show_mask_graphic);
    if draw_graphic && let Some(ref g) = node.graphic {
        match g {
            UiGraphic::Image {
                color,
                texture_key,
                missing_texture,
                image_type,
                preserve_aspect,
                fill_amount,
                border,
                raw_name,
            } => {
                paint_ui_image(
                    painter,
                    artwork_rect,
                    *color,
                    eff_alpha,
                    texture_key.as_deref().and_then(|k| ctx.textures.get(k)),
                    *missing_texture,
                    *image_type,
                    *preserve_aspect,
                    *fill_amount,
                    *border,
                    [0.0, 0.0, 1.0, 1.0],
                    geometry.flip_x,
                    geometry.flip_y,
                    false,
                    raw_name.as_deref().or(Some(node.name.as_str())),
                    ctx.scale,
                    geometry.rotation_deg,
                );
            }
            UiGraphic::RawImage {
                color,
                texture_key,
                missing_texture,
                uv_rect,
                raw_name,
            } => {
                paint_ui_image(
                    painter,
                    artwork_rect,
                    *color,
                    eff_alpha,
                    texture_key.as_deref().and_then(|k| ctx.textures.get(k)),
                    *missing_texture,
                    UiImageType::Simple,
                    false,
                    1.0,
                    [0.0; 4],
                    *uv_rect,
                    geometry.flip_x,
                    geometry.flip_y,
                    true,
                    raw_name.as_deref().or(Some(node.name.as_str())),
                    ctx.scale,
                    geometry.rotation_deg,
                );
            }
            UiGraphic::Text {
                text,
                color,
                font_size,
                alignment,
                rich_text,
                outline_color,
                ..
            } => {
                if ctx.show_labels && !text.is_empty() {
                    paint_ui_text(
                        painter,
                        artwork_rect,
                        text,
                        *color,
                        eff_alpha,
                        *font_size * ctx.scale,
                        *alignment,
                        *rich_text,
                        *outline_color,
                        geometry.rotation_deg,
                    );
                }
            }
        }
    }

    if screen_rect.width() >= 1.0 && screen_rect.height() >= 1.0 {
        let is_selected = ctx.selected_ref == Some(node.go_ref);
        if is_selected {
            painter.rect_stroke(
                screen_rect,
                0.0,
                Stroke::new(2.0, Color32::from_rgb(255, 190, 50)),
                egui::StrokeKind::Inside,
            );
        } else if ctx.show_outlines {
            let stroke_color = if node.is_interactive {
                Color32::from_rgba_unmultiplied(90, 235, 140, 200)
            } else if matches!(node.graphic, Some(UiGraphic::Text { .. })) {
                Color32::from_rgba_unmultiplied(120, 195, 255, 180)
            } else if node.graphic.is_some() {
                Color32::from_rgba_unmultiplied(170, 220, 195, 140)
            } else {
                Color32::from_rgba_unmultiplied(140, 145, 160, 80)
            };
            painter.rect_stroke(
                screen_rect,
                0.0,
                Stroke::new(1.0, stroke_color),
                egui::StrokeKind::Inside,
            );
        }

        let kind_label = match (&node.widget_kind, &node.graphic) {
            (Some(w), _) => w.as_str(),
            (None, Some(UiGraphic::Image { .. })) => "Image",
            (None, Some(UiGraphic::RawImage { .. })) => "RawImage",
            (None, Some(UiGraphic::Text { .. })) => "Text",
            _ => "Container",
        };
        let extra = match &node.graphic {
            Some(UiGraphic::Text { text, .. }) if !text.is_empty() => {
                let first_line = text.lines().next().unwrap_or("");
                format!(" • \"{first_line}\"")
            }
            _ => String::new(),
        };
        hits.push(UiNodeHit {
            go_ref: node.go_ref,
            rect: screen_rect,
            has_visual_or_widget: node.is_interactive || node.graphic.is_some(),
            summary: format!(
                "{} [{}] ({:.0},{:.0} {:.0}x{:.0}){}",
                node.name, kind_label, rx, ry, rw, rh, extra
            ),
        });
    }

    let child_painter_storage;
    let child_painter = if node.clips_children && screen_rect.is_positive() {
        child_painter_storage = painter.with_clip_rect(painter.clip_rect().intersect(screen_rect));
        &child_painter_storage
    } else {
        painter
    };

    let hide_toggle_checkmark = !ctx.show_inactive
        && node.widget_kind.as_deref() == Some("Toggle")
        && node.widget_props.as_ref().and_then(|p| p.is_on) == Some(false);
    let is_radio = !ctx.show_inactive && node.widget_kind.as_deref() == Some("RadioButton");
    let radio_has_both = is_radio
        && node.children.iter().any(|c| c.name == "On" && c.active_self)
        && node.children.iter().any(|c| c.name == "Off" && c.active_self);
    let radio_default_on = !node.name.to_ascii_lowercase().contains("female");

    // Ancestor transforms are baked into static canvas geometry. Animated parent transforms
    // still need a separate relayout pass before they can affect descendant geometry here.
    for child in &node.children {
        if hide_toggle_checkmark
            && matches!(child.name.as_str(), "Checkmark" | "m_mark" | "m_select")
        {
            continue;
        }
        if radio_has_both {
            if radio_default_on && child.name == "Off" {
                continue;
            }
            if !radio_default_on && child.name == "On" {
                continue;
            }
        }
        let child_path = if path.is_empty() {
            child.name.clone()
        } else {
            format!("{path}/{}", child.name)
        };
        paint_ui_node_recursive(child_painter, child, &child_path, ctx, hits);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_ui_image(
    painter: &egui::Painter,
    rect: Rect,
    color: [f32; 4],
    effective_alpha: f32,
    texture: Option<&egui::TextureHandle>,
    missing_texture: bool,
    image_type: UiImageType,
    preserve_aspect: bool,
    fill_amount: f32,
    border: [f32; 4],
    uv_rect: [f32; 4],
    flip_x: bool,
    flip_y: bool,
    is_raw_image: bool,
    placeholder_label: Option<&str>,
    scale: f32,
    rot_deg: f32,
) {
    let alpha = (color[3] * effective_alpha).clamp(0.0, 1.0);
    if alpha <= 0.004 || !rect.is_positive() {
        return;
    }
    let tint = Color32::from_rgba_unmultiplied(
        (color[0] * 255.0).round() as u8,
        (color[1] * 255.0).round() as u8,
        (color[2] * 255.0).round() as u8,
        (alpha * 255.0).round() as u8,
    );

    let Some(tex) = texture else {
        let is_untextured_white_block = !is_raw_image
            && color[0] > 0.90
            && color[1] > 0.90
            && color[2] > 0.90
            && alpha > 0.85
            && rect.width() > 48.0 * scale
            && rect.height() > 48.0 * scale;
        if missing_texture {
            let ph_alpha = ((alpha * 48.0).round() as u8).max(18);
            painter.rect_filled(
                rect,
                2.0,
                Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), ph_alpha),
            );
            painter.rect_stroke(
                rect,
                2.0,
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(230, 175, 95, 160)),
                egui::StrokeKind::Inside,
            );
            if rect.width() > 36.0 && rect.height() > 14.0
                && let Some(lbl) = placeholder_label
            {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    lbl,
                    egui::FontId::proportional((11.0 * scale).clamp(8.0, 13.0)),
                    Color32::from_rgba_unmultiplied(235, 200, 140, 200),
                );
            }
        } else if is_raw_image || is_untextured_white_block {
            // Runtime RenderTexture slots (`m_hero_tex`, minimap feed) and large untextured
            // white placeholders must never paint an opaque white slab across underlying panels.
            painter.rect_filled(rect, 2.0, Color32::from_rgba_unmultiplied(45, 90, 130, 20));
            painter.rect_stroke(
                rect,
                2.0,
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(95, 185, 235, 110)),
                egui::StrokeKind::Inside,
            );
            if is_raw_image
                && rect.width() > 64.0
                && rect.height() > 24.0
                && let Some(lbl) = placeholder_label
            {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("[RenderTarget: {lbl}]"),
                    egui::FontId::proportional((11.0 * scale).clamp(8.0, 13.0)),
                    Color32::from_rgba_unmultiplied(150, 215, 245, 175),
                );
            }
        } else {
            painter.rect_filled(rect, 0.0, tint);
        }
        return;
    };

    let tex_sz = tex.size_vec2();
    let mut draw_rect = rect;
    if preserve_aspect && tex_sz.y > 0.0 && draw_rect.height() > 0.0 {
        let tex_ar = tex_sz.x / tex_sz.y;
        let box_ar = draw_rect.width() / draw_rect.height();
        if box_ar > tex_ar {
            let new_w = draw_rect.height() * tex_ar;
            draw_rect = Rect::from_center_size(draw_rect.center(), vec2(new_w, draw_rect.height()));
        } else {
            let new_h = draw_rect.width() / tex_ar;
            draw_rect = Rect::from_center_size(draw_rect.center(), vec2(draw_rect.width(), new_h));
        }
    }

    if image_type == UiImageType::Filled && fill_amount < 0.999 {
        let fill_w = draw_rect.width() * fill_amount.clamp(0.0, 1.0);
        draw_rect.max.x = draw_rect.min.x + fill_w;
    }

    if image_type == UiImageType::Sliced
        && !flip_x
        && !flip_y
        && tex_sz.x > 1.0
        && tex_sz.y > 1.0
        && border.iter().any(|&b| b > 0.5)
    {
        paint_9_sliced_image(painter, tex.id(), tex_sz, draw_rect, border, scale, tint, rot_deg);
        return;
    }

    let u0 = if flip_x { uv_rect[0] + uv_rect[2] } else { uv_rect[0] };
    let u1 = if flip_x { uv_rect[0] } else { uv_rect[0] + uv_rect[2] };
    // Unity RawImage UV y=0 is bottom, egui UV y=0 is top.
    let v_top_unity = 1.0 - (uv_rect[1] + uv_rect[3]);
    let v_bot_unity = 1.0 - uv_rect[1];
    let (v0, v1) = if flip_y {
        (v_bot_unity, v_top_unity)
    } else {
        (v_top_unity, v_bot_unity)
    };
    let uv = Rect::from_min_max(pos2(u0, v0), pos2(u1, v1));
    paint_ui_image_quad(painter, tex.id(), draw_rect, uv, tint, rect.center(), rot_deg);
}

fn paint_ui_image_quad(
    painter: &egui::Painter,
    tex_id: egui::TextureId,
    rect: Rect,
    uv: Rect,
    tint: Color32,
    rotation_center: Pos2,
    rot_deg: f32,
) {
    if rot_deg.abs() > 0.25 {
        let mut mesh = egui::Mesh::with_texture(tex_id);
        let corners = [
            (rect.left_top(), pos2(uv.min.x, uv.min.y)),
            (rect.right_top(), pos2(uv.max.x, uv.min.y)),
            (rect.right_bottom(), pos2(uv.max.x, uv.max.y)),
            (rect.left_bottom(), pos2(uv.min.x, uv.max.y)),
        ];
        for (p, t) in corners {
            mesh.vertices.push(egui::epaint::Vertex {
                pos: rotation_center + rotate_ui_vector(p - rotation_center, rot_deg),
                uv: t,
                color: tint,
            });
        }
        mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
        painter.add(egui::Shape::mesh(mesh));
    } else {
        painter.image(tex_id, rect, uv, tint);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_9_sliced_image(
    painter: &egui::Painter,
    tex_id: egui::TextureId,
    tex_sz: egui::Vec2,
    dst: Rect,
    border_px: [f32; 4], // [left, top, right, bottom] in sprite pixels
    scale: f32,
    tint: Color32,
    rot_deg: f32,
) {
    let bl = (border_px[0] * scale).min(dst.width() * 0.5);
    let bt = (border_px[1] * scale).min(dst.height() * 0.5);
    let br = (border_px[2] * scale).min(dst.width() * 0.5);
    let bb = (border_px[3] * scale).min(dst.height() * 0.5);

    let ul = (border_px[0] / tex_sz.x).clamp(0.0, 0.5);
    let ut = (border_px[1] / tex_sz.y).clamp(0.0, 0.5);
    let ur = (1.0 - border_px[2] / tex_sz.x).clamp(0.5, 1.0);
    let ub = (1.0 - border_px[3] / tex_sz.y).clamp(0.5, 1.0);

    let xs = [dst.min.x, dst.min.x + bl, (dst.max.x - br).max(dst.min.x + bl), dst.max.x];
    let ys = [dst.min.y, dst.min.y + bt, (dst.max.y - bb).max(dst.min.y + bt), dst.max.y];
    let us = [0.0, ul, ur, 1.0];
    let vs = [0.0, ut, ub, 1.0];

    for row in 0..3 {
        if ys[row + 1] <= ys[row] + 0.01 {
            continue;
        }
        for col in 0..3 {
            if xs[col + 1] <= xs[col] + 0.01 {
                continue;
            }
            let r = Rect::from_min_max(pos2(xs[col], ys[row]), pos2(xs[col + 1], ys[row + 1]));
            let uv = Rect::from_min_max(pos2(us[col], vs[row]), pos2(us[col + 1], vs[row + 1]));
            paint_ui_image_quad(painter, tex_id, r, uv, tint, dst.center(), rot_deg);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_ui_text(
    painter: &egui::Painter,
    rect: Rect,
    raw_text: &str,
    color: [f32; 4],
    effective_alpha: f32,
    font_px: f32,
    alignment: UiTextAlignment,
    rich_text: bool,
    outline_color: Option<[f32; 4]>,
    rotation_deg: f32,
) {
    let alpha = (color[3] * effective_alpha).clamp(0.0, 1.0);
    if alpha <= 0.004 || rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }
    let clean_owned;
    let display = if rich_text && raw_text.contains('<') {
        clean_owned = strip_unity_rich_text(raw_text);
        clean_owned.as_str()
    } else {
        raw_text
    };
    if display.is_empty() {
        return;
    }

    let font_id = egui::FontId::proportional(font_px.clamp(6.0, 180.0));
    let fg = Color32::from_rgba_unmultiplied(
        (color[0] * 255.0).round() as u8,
        (color[1] * 255.0).round() as u8,
        (color[2] * 255.0).round() as u8,
        (alpha * 255.0).round() as u8,
    );

    let egui_align = match alignment {
        UiTextAlignment::UpperLeft => egui::Align2::LEFT_TOP,
        UiTextAlignment::UpperCenter => egui::Align2::CENTER_TOP,
        UiTextAlignment::UpperRight => egui::Align2::RIGHT_TOP,
        UiTextAlignment::MiddleLeft => egui::Align2::LEFT_CENTER,
        UiTextAlignment::MiddleCenter => egui::Align2::CENTER_CENTER,
        UiTextAlignment::MiddleRight => egui::Align2::RIGHT_CENTER,
        UiTextAlignment::LowerLeft => egui::Align2::LEFT_BOTTOM,
        UiTextAlignment::LowerCenter => egui::Align2::CENTER_BOTTOM,
        UiTextAlignment::LowerRight => egui::Align2::RIGHT_BOTTOM,
    };
    let anchor = pos2(
        rect.min.x + rect.width() * alignment.horizontal_factor(),
        rect.min.y + rect.height() * alignment.vertical_factor(),
    );

    let (sin, cos) = rotation_deg.to_radians().sin_cos();
    let text_bounds = Rect::from_center_size(rect.center(), vec2(
        rect.width() * cos.abs() + rect.height() * sin.abs(),
        rect.width() * sin.abs() + rect.height() * cos.abs(),
    ));
    let clip = painter.with_clip_rect(painter.clip_rect().intersect(text_bounds.expand(8.0)));
    let galley = clip.layout_no_wrap(display.to_owned(), font_id, fg);
    let origin = egui_align.anchor_size(anchor, galley.size()).min;
    let paint_text = |offset: egui::Vec2, tint: Color32| {
        let rotated_origin = rect.center() + rotate_ui_vector(origin + offset - rect.center(), rotation_deg);
        clip.add(egui::epaint::TextShape::new(rotated_origin, galley.clone(), tint)
            .with_override_text_color(tint)
            .with_angle(rotation_deg.to_radians()));
    };
    if let Some(oc) = outline_color {
        let oa = (oc[3] * effective_alpha * 255.0).round() as u8;
        if oa > 4 {
            let shadow = Color32::from_rgba_unmultiplied(
                (oc[0] * 255.0).round() as u8,
                (oc[1] * 255.0).round() as u8,
                (oc[2] * 255.0).round() as u8,
                oa,
            );
            paint_text(vec2(1.0, 1.0), shadow);
        }
    }
    paint_text(egui::Vec2::ZERO, fg);
}

fn strip_unity_rich_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag = String::new();
            let mut closed = false;
            for tc in chars.by_ref() {
                if tc == '>' {
                    closed = true;
                    break;
                }
                tag.push(tc);
                if tag.len() > 32 {
                    break;
                }
            }
            if !closed {
                out.push('<');
                out.push_str(&tag);
            } else {
                let low = tag.trim().to_ascii_lowercase();
                let known_tag = low == "b"
                    || low == "/b"
                    || low == "i"
                    || low == "/i"
                    || low == "/color"
                    || low == "/size"
                    || low.starts_with("color=")
                    || low.starts_with("size=");
                if !known_tag {
                    out.push('<');
                    out.push_str(&tag);
                    out.push('>');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn show_yaml_json_view(
    ui: &mut egui::Ui,
    state: &DevXState,
    deferred: &mut Option<PreviewAction>,
) {
    let base_name = state
        .selected
        .and_then(|r| state.catalog.as_ref()?.entry(r))
        .map_or_else(|| "object".to_owned(), |e| e.summary.name.clone());

    ui.horizontal(|ui| {
        if let Some(ref yaml) = state.selected_yaml
            && ui.button("Save .yaml").clicked()
        {
            *deferred = Some(PreviewAction::SaveBytes(
                format!("{base_name}.yaml"),
                yaml.as_bytes().to_vec(),
            ));
        }
        if let Some(ref json) = state.selected_json
            && ui.button("Save .json").clicked()
        {
            *deferred = Some(PreviewAction::SaveBytes(
                format!("{base_name}.json"),
                json.as_bytes().to_vec(),
            ));
        }
    });
    ui.separator();

    ScrollArea::vertical()
        .id_salt("yaml_json_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if let Some(ref yaml) = state.selected_yaml {
                egui::CollapsingHeader::new("Unity YAML")
                    .default_open(true)
                    .show(ui, |ui| {
                        let slice = truncate_utf8_preview(yaml, 48 * 1024);
                        let theme = CodeTheme::from_style(ui.style());
                        let job = highlight(ui.ctx(), ui.style(), &theme, slice, "yaml");
                        ui.label(job);
                    });
            }
            if let Some(ref json) = state.selected_json {
                egui::CollapsingHeader::new("TypeTree JSON")
                    .default_open(state.selected_yaml.is_none())
                    .show(ui, |ui| {
                        let slice = truncate_utf8_preview(json, 48 * 1024);
                        let theme = CodeTheme::from_style(ui.style());
                        let job = highlight(ui.ctx(), ui.style(), &theme, slice, "json");
                        ui.label(job);
                    });
            }
        });
}

fn show_hex_raw_view(
    ui: &mut egui::Ui,
    state: &DevXState,
    deferred: &mut Option<PreviewAction>,
) {
    let Some(bytes) = state.selected_raw_bytes.as_ref() else {
        ui.weak("No raw object bytes available for current selection.");
        return;
    };
    let name = state
        .selected
        .and_then(|r| state.catalog.as_ref()?.entry(r))
        .map_or_else(|| "asset".to_owned(), |e| e.summary.name.clone());

    ui.horizontal(|ui| {
        ui.strong(format!("Raw Object Payload: {} bytes", bytes.len()));
        if ui.button("Save .raw").clicked() {
            *deferred = Some(PreviewAction::SaveBytes(
                format!("{name}.raw"),
                bytes.clone(),
            ));
        }
    });
    ui.separator();

    let total_rows = bytes.len().div_ceil(16);
    ScrollArea::vertical()
        .id_salt("hex_raw_scroll")
        .auto_shrink([false, false])
        .show_rows(ui, 18.0, total_rows, |ui, range| {
            let mut line = String::with_capacity(80);
            for row in range {
                let offset = row * 16;
                let end = (offset + 16).min(bytes.len());
                let slice = &bytes[offset..end];
                line.clear();
                use std::fmt::Write as _;
                let _ = write!(line, "{offset:08X}: ");
                for i in 0..16 {
                    if i == 8 {
                        line.push(' ');
                    }
                    if let Some(&b) = slice.get(i) {
                        let _ = write!(line, "{b:02X} ");
                    } else {
                        line.push_str("   ");
                    }
                }
                line.push_str(" |");
                for &b in slice {
                    let ch = if b.is_ascii_graphic() || b == b' ' {
                        b as char
                    } else {
                        '.'
                    };
                    line.push(ch);
                }
                line.push('|');
                ui.label(RichText::new(&line).monospace());
            }
        });
}

fn show_highlighted_code(ui: &mut egui::Ui, text: &str, lang: &str, scroll_id: &str) {
    let preview_slice = truncate_utf8_preview(text, 64 * 1024);
    if preview_slice.len() < text.len() {
        ui.weak(format!(
            "Showing first {} of {} bytes — click Save File to export complete content.",
            preview_slice.len(),
            text.len()
        ));
    }
    ScrollArea::both()
        .id_salt(scroll_id)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let theme = CodeTheme::from_style(ui.style());
            let job = highlight(ui.ctx(), ui.style(), &theme, preview_slice, lang);
            ui.label(job);
        });
}

fn truncate_utf8_preview(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    &text[..end]
}

fn paint_checkerboard(painter: &egui::Painter, rect: Rect, tile: f32) {
    let c_dark = Color32::from_gray(42);
    let c_light = Color32::from_gray(64);
    painter.rect_filled(rect, 0.0, c_dark);

    let cols = (rect.width() / tile).ceil() as i32;
    let rows = (rect.height() / tile).ceil() as i32;
    // Cap checkerboard tessellation for extreme zoom ratios.
    if cols.saturating_mul(rows) > 4000 {
        return;
    }
    for r in 0..rows {
        for c in 0..cols {
            if (r + c) % 2 == 0 {
                let min = pos2(rect.left() + c as f32 * tile, rect.top() + r as f32 * tile);
                let max = pos2(
                    (min.x + tile).min(rect.right()),
                    (min.y + tile).min(rect.bottom()),
                );
                painter.rect_filled(Rect::from_min_max(min, max), 0.0, c_light);
            }
        }
    }
}

fn paint_waveform(painter: &egui::Painter, rect: Rect, peaks: &[(f32, f32)]) {
    painter.rect_filled(rect, 4.0, Color32::from_rgb(20, 24, 30));
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, Color32::from_gray(70)),
        egui::StrokeKind::Inside,
    );

    let mid_y = rect.center().y;
    painter.line_segment(
        [pos2(rect.left(), mid_y), pos2(rect.right(), mid_y)],
        Stroke::new(1.0, Color32::from_gray(55)),
    );

    if peaks.is_empty() {
        return;
    }
    let half_h = rect.height() * 0.44;
    let n = peaks.len() as f32;
    let bar_stroke = Stroke::new((rect.width() / n).clamp(1.0, 4.0), Color32::from_rgb(95, 220, 145));
    for (i, &(min_v, max_v)) in peaks.iter().enumerate() {
        let x = rect.left() + ((i as f32 + 0.5) / n) * rect.width();
        let y_top = mid_y - max_v.clamp(-1.0, 1.0) * half_h;
        let y_bot = mid_y - min_v.clamp(-1.0, 1.0) * half_h;
        painter.line_segment([pos2(x, y_top), pos2(x, y_bot.max(y_top + 1.0))], bar_stroke);
    }
}

fn upload_decoded_tex(
    ctx: &egui::Context,
    id: &str,
    img: &DecodedImage,
    opts: egui::TextureOptions,
) -> egui::TextureHandle {
    let color_img = egui::ColorImage::from_rgba_unmultiplied(
        [img.width as usize, img.height as usize],
        &img.rgba8,
    );
    ctx.load_texture(id, color_img, opts)
}

/// Uploads any pending albedo, normal, emissive, or metallic-gloss textures on a `HierarchyPreviewData`.
fn upload_scene_textures(
    ctx: &egui::Context,
    scene: &mut HierarchyPreviewData,
    tex_opts: egui::TextureOptions,
) {
    let root_file = scene.root_ref.file_index;
    let root_path = scene.root_ref.path_id;
    let upload_if_needed = |handle: &mut Option<egui::TextureHandle>,
                            img: Option<&DecodedImage>,
                            suffix: &str,
                            part_idx: usize,
                            slot_idx: usize| {
        if handle.is_none()
            && let Some(im) = img
        {
            let id = format!(
                "devx_scene_tex_{root_file}_{root_path}_{part_idx}_{slot_idx}_{suffix}"
            );
            *handle = Some(upload_decoded_tex(ctx, &id, im, tex_opts));
        }
    };
    for (part_idx, part) in scene.parts.iter_mut().enumerate() {
        for (slot_idx, slot) in part.materials.iter_mut().enumerate() {
            upload_if_needed(
                &mut slot.albedo_texture,
                slot.albedo_image.as_deref(),
                "alb",
                part_idx,
                slot_idx,
            );
            upload_if_needed(
                &mut slot.normal_texture,
                slot.normal_image.as_deref(),
                "nrm",
                part_idx,
                slot_idx,
            );
            upload_if_needed(
                &mut slot.emissive_texture,
                slot.emissive_image.as_deref(),
                "emi",
                part_idx,
                slot_idx,
            );
            upload_if_needed(
                &mut slot.metallic_gloss_texture,
                slot.metallic_gloss_image.as_deref(),
                "met",
                part_idx,
                slot_idx,
            );
        }
        if part.albedo_texture.is_none() {
            part.albedo_texture = part
                .materials
                .first()
                .and_then(|s| s.albedo_texture.clone())
                .or_else(|| {
                    part.albedo_image.as_ref().map(|img| {
                        let id = format!(
                            "devx_scene_tex_{root_file}_{root_path}_{part_idx}_legacy"
                        );
                        upload_decoded_tex(ctx, &id, img, tex_opts)
                    })
                });
        }
    }
}

/// Renders metadata summary, layer details, and interactive 3D heightmap viewport for `TerrainData`.
fn render_terrain_view(
    ui: &mut egui::Ui,
    terrain: &DecodedTerrainData,
    scene: &HierarchyPreviewData,
    cam: &mut MeshViewportState,
) {
    ui.horizontal_wrapped(|ui| {
        ui.heading(&terrain.name);
        ui.separator();
        ui.label(format!("Grid: {}x{}", terrain.width, terrain.height));
        ui.separator();
        ui.label(format!(
            "Size: {:.1}x{:.1}x{:.1}m",
            terrain.size[0], terrain.size[1], terrain.size[2]
        ));
        ui.separator();
        ui.label(format!(
            "Height: [{:.1}..{:.1}]m",
            terrain.min_height, terrain.max_height
        ));
        if terrain.hole_count > 0 {
            ui.separator();
            ui.label(format!("Holes: {}", terrain.hole_count));
        }
        ui.separator();
        ui.label(format!(
            "Layers: {} | Splatmaps: {} | Trees: {} ({} inst) | Details: {}",
            terrain.layers.len(),
            terrain.alpha_texture_count,
            terrain.tree_prototype_count,
            terrain.tree_instance_count,
            terrain.detail_prototype_count,
        ));
    });

    if !terrain.layers.is_empty() {
        egui::CollapsingHeader::new(format!("Terrain Layers ({})", terrain.layers.len()))
            .default_open(false)
            .show(ui, |ui| {
                for layer in &terrain.layers {
                    ui.label(format!(
                        "#{} {} — tile {:.2}x{:.2}, offset ({:.2}, {:.2})",
                        layer.index,
                        layer.label,
                        layer.tile_size[0],
                        layer.tile_size[1],
                        layer.tile_offset[0],
                        layer.tile_offset[1],
                    ));
                }
            });
    }

    ui.separator();
    if scene.parts.is_empty() {
        ui.weak("No terrain heightmap samples available to render.");
        return;
    }
    render_scene_viewport(
        ui,
        &scene.parts,
        &scene.bone_segments,
        scene.bounds_min,
        scene.bounds_max,
        cam,
    );
}

/// Renders module/curve telemetry and an interactive 3D emitter+cloud preview for `ParticleSystem`.
fn render_particle_system_view(
    ui: &mut egui::Ui,
    particle: &DecodedParticleSystem,
    scene: &HierarchyPreviewData,
    cam: &mut MeshViewportState,
) {
    ui.horizontal_wrapped(|ui| {
        ui.heading(&particle.name);
        ui.separator();
        ui.label(format!("Duration: {:.2}s", particle.duration_secs));
        if particle.looping {
            ui.label("• Loop");
        }
        if particle.prewarm {
            ui.label("• Prewarm");
        }
        if particle.play_on_awake {
            ui.label("• PlayOnAwake");
        }
        ui.separator();
        ui.label(format!("Max: {}", particle.max_particles));
        ui.separator();
        ui.label(format!(
            "Shape: {}{}",
            particle.shape.shape_label(),
            if particle.shape.enabled { "" } else { " (off)" }
        ));
        ui.separator();
        ui.label(format!(
            "Rate: {}/s",
            particle.rate_over_time.display_value()
        ));
    });

    egui::CollapsingHeader::new(format!(
        "Particle Curves, Bursts ({}) & Modules ({}/{})",
        particle.bursts.len(),
        particle.modules.iter().filter(|m| m.enabled).count(),
        particle.modules.len(),
    ))
    .default_open(false)
    .show(ui, |ui| {
        egui::Grid::new("particle_curves_grid")
            .num_columns(4)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                let mut row = |k1: &str, v1: String, k2: &str, v2: String| {
                    ui.weak(k1);
                    ui.label(v1);
                    ui.weak(k2);
                    ui.label(v2);
                    ui.end_row();
                };
                row(
                    "Start Delay",
                    particle.start_delay.display_value(),
                    "Simulation Speed",
                    format!("{:.2}x", particle.simulation_speed),
                );
                row(
                    "Start Lifetime",
                    format!("{}s", particle.start_lifetime.display_value()),
                    "Start Speed",
                    format!("{} m/s", particle.start_speed.display_value()),
                );
                row(
                    "Start Size",
                    particle.start_size.display_value(),
                    "Gravity Modifier",
                    particle.gravity_modifier.display_value(),
                );
                row(
                    "Rate / Distance",
                    particle.rate_over_distance.display_value(),
                    "Emitter Radius / Angle",
                    format!(
                        "r={:.2}, ang={:.1}°, len={:.2}",
                        particle.shape.radius, particle.shape.angle_deg, particle.shape.length
                    ),
                );
            });

        if !particle.bursts.is_empty() {
            ui.add_space(4.0);
            ui.strong("Emission Bursts:");
            for b in &particle.bursts {
                ui.label(format!(
                    "  t={:.2}s — count {}..={} (cycles: {}, interval: {:.2}s)",
                    b.time, b.min_count, b.max_count, b.cycle_count, b.repeat_interval
                ));
            }
        }

        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for m in &particle.modules {
                let color = if m.enabled {
                    Color32::from_rgb(120, 220, 140)
                } else {
                    Color32::from_gray(120)
                };
                ui.colored_label(
                    color,
                    format!("[{}] {}", if m.enabled { "ON" } else { "OFF" }, m.name),
                );
                if !m.detail.is_empty() {
                    ui.weak(format!("({})", m.detail));
                }
            }
        });
    });

    ui.separator();
    if scene.parts.is_empty() {
        ui.weak("No particle emitter geometry available to render.");
        return;
    }
    render_scene_viewport(
        ui,
        &scene.parts,
        &scene.bone_segments,
        scene.bounds_min,
        scene.bounds_max,
        cam,
    );
}

fn paint_anim_curves(
    painter: &egui::Painter,
    rect: Rect,
    clip: &DecodedAnimationClip,
    track_idx: usize,
) {
    painter.rect_filled(rect, 4.0, Color32::from_rgb(20, 23, 29));
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, Color32::from_gray(60)),
        egui::StrokeKind::Inside,
    );
    let Some(track) = clip.tracks.get(track_idx) else {
        return;
    };
    if track.times.is_empty() || track.values.is_empty() {
        return;
    }

    let stride = if track.property == TrackProperty::Rotation { 4 } else { 3 };
    let count = track.times.len().min(track.values.len() / stride);
    if count == 0 {
        return;
    }

    let t_max = clip
        .duration_secs
        .max(*track.times.last().unwrap_or(&0.0))
        .max(1e-4);
    let mut v_min = f32::INFINITY;
    let mut v_max = f32::NEG_INFINITY;
    for &v in &track.values[..count * stride] {
        v_min = v_min.min(v);
        v_max = v_max.max(v);
    }
    if (v_max - v_min).abs() < 1e-4 {
        v_min -= 1.0;
        v_max += 1.0;
    }

    let colors = [
        Color32::from_rgb(235, 95, 95),
        Color32::from_rgb(95, 220, 120),
        Color32::from_rgb(100, 175, 250),
        Color32::from_rgb(245, 200, 80),
    ];
    let pad = 10.0;
    let inner = rect.shrink(pad);

    // Zero baseline if within range
    if v_min <= 0.0 && v_max >= 0.0 {
        let ny = 1.0 - (0.0 - v_min) / (v_max - v_min);
        let zy = inner.top() + ny * inner.height();
        painter.line_segment(
            [pos2(inner.left(), zy), pos2(inner.right(), zy)],
            Stroke::new(1.0, Color32::from_gray(55)),
        );
    }

    for (comp, &color) in colors.iter().enumerate().take(stride) {
        let mut pts = Vec::with_capacity(count);
        for k in 0..count {
            let nx = (track.times[k] / t_max).clamp(0.0, 1.0);
            let ny = 1.0 - ((track.values[k * stride + comp] - v_min) / (v_max - v_min)).clamp(0.0, 1.0);
            pts.push(pos2(
                inner.left() + nx * inner.width(),
                inner.top() + ny * inner.height(),
            ));
        }
        if pts.len() == 1 {
            painter.circle_filled(pts[0], 3.0, color);
        } else {
            painter.add(egui::Shape::line(pts, Stroke::new(1.6, color)));
        }
    }
}

fn render_scene_viewport(
    ui: &mut egui::Ui,
    parts: &[ViewportMeshPart],
    bone_segments: &[([f32; 3], [f32; 3])],
    bounds_min: [f32; 3],
    bounds_max: [f32; 3],
    cam: &mut MeshViewportState,
) {
    let avail = vec2(ui.available_width().max(180.0), ui.available_height().max(280.0));
    let (rect, resp) = ui.allocate_exact_size(avail, Sense::click_and_drag());

    if resp.double_clicked() {
        cam.reset_camera();
    }
    if resp.dragged_by(egui::PointerButton::Secondary)
        || (resp.dragged() && ui.input(|i| i.modifiers.shift))
    {
        let delta = resp.drag_delta();
        cam.pan += vec2(delta.x / rect.height().max(1.0), delta.y / rect.height().max(1.0)) * cam.distance;
    } else if resp.dragged() {
        let delta = resp.drag_delta();
        cam.yaw -= delta.x * 0.01;
        cam.pitch = (cam.pitch + delta.y * 0.01).clamp(-1.48, 1.48);
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 {
            cam.distance = (cam.distance * (1.0 - scroll * 0.002)).clamp(0.35, 30.0);
        }
    }

    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 4.0, Color32::from_rgb(24, 27, 33));
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, Color32::from_gray(65)),
        egui::StrokeKind::Inside,
    );

    let total_tris_all: usize = parts
        .iter()
        .filter(|p| p.visible)
        .map(|p| p.mesh.indices.len() / 3)
        .sum();
    if parts.is_empty() || total_tris_all == 0 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            if parts.is_empty() {
                "Empty mesh geometry"
            } else {
                "All mesh parts hidden (enable a part in Parts & Visibility)"
            },
            egui::FontId::default(),
            Color32::GRAY,
        );
        return;
    }

    if cam.use_gpu
        && crate::gl_viewport::paint_gpu_scene_viewport(
            ui,
            rect,
            parts,
            bone_segments,
            bounds_min,
            bounds_max,
            cam,
        )
    {
        return;
    }

    let center = [
        (bounds_min[0] + bounds_max[0]) * 0.5,
        (bounds_min[1] + bounds_max[1]) * 0.5,
        (bounds_min[2] + bounds_max[2]) * 0.5,
    ];
    let extent = [
        bounds_max[0] - bounds_min[0],
        bounds_max[1] - bounds_min[1],
        bounds_max[2] - bounds_min[2],
    ];
    let radius = (extent[0] * extent[0] + extent[1] * extent[1] + extent[2] * extent[2])
        .sqrt()
        .max(1e-4)
        * 0.5;

    let (sy, cy) = cam.yaw.sin_cos();
    let (sp, cp) = cam.pitch.sin_cos();
    let forward = normalize3([-cy * cp, -sp, -sy * cp]);
    let right = normalize3(cross3(forward, [0.0, 1.0, 0.0]));
    let up = normalize3(cross3(right, forward));

    let cam_pos = [
        -forward[0] * cam.distance,
        -forward[1] * cam.distance,
        -forward[2] * cam.distance,
    ];
    let scale = rect.height().min(rect.width()) * 0.9;
    let cx = rect.center().x + cam.pan.x * rect.height() * 0.5;
    let cy_scr = rect.center().y + cam.pan.y * rect.height() * 0.5;

    let project = |p: [f32; 3]| -> Option<(Pos2, f32, [f32; 3])> {
        let np = [
            (p[0] - center[0]) / radius,
            (p[1] - center[1]) / radius,
            (p[2] - center[2]) / radius,
        ];
        let rel = [np[0] - cam_pos[0], np[1] - cam_pos[1], np[2] - cam_pos[2]];
        let z = dot3(rel, forward);
        if z <= 0.05 {
            return None;
        }
        let x = dot3(rel, right);
        let y = dot3(rel, up);
        let sx = cx + (x / z) * scale;
        let sy_p = cy_scr - (y / z) * scale;
        Some((pos2(sx, sy_p), z, np))
    };

    struct TriDraw {
        avg_z: f32,
        tex_id: Option<egui::TextureId>,
        pts: [Pos2; 3],
        uvs: [Pos2; 3],
        tint: Color32,
    }

    let tri_step = (total_tris_all / 24_000).max(1);
    let mut visible_tris: Vec<TriDraw> = Vec::with_capacity(total_tris_all.min(24_000));
    let light_dir = normalize3([0.55, 0.78, 0.32]);
    let wire_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(150, 210, 255, 95));

    for part in parts {
        if !part.visible {
            continue;
        }
        let mesh = &part.mesh;
        if mesh.positions.is_empty() || mesh.indices.len() < 3 {
            continue;
        }
        let projected: Vec<Option<(Pos2, f32, [f32; 3])>> =
            mesh.positions.iter().map(|&p| project(p)).collect();
        let has_uv = mesh.uvs.len() == mesh.positions.len();

        let fallback_subs = [crate::scene_preview::ViewportSubMesh {
            index_start: 0,
            index_count: (mesh.indices.len() / 3) * 3,
            material_slot: 0,
        }];
        let submeshes = if part.submeshes.is_empty() {
            &fallback_subs[..]
        } else {
            part.submeshes.as_slice()
        };

        for sub in submeshes {
            let slot = part.material_for_submesh(sub);
            let slot_tex = if cam.textured {
                slot.and_then(|s| s.albedo_texture.as_ref().map(|t| t.id()))
                    .or_else(|| (sub.material_slot == 0).then(|| part.albedo_texture.as_ref().map(|t| t.id())).flatten())
            } else {
                None
            };
            let base_color = slot.map_or(part.base_color, |s| s.base_color);
            let emissive = slot.map_or([0.0; 3], |s| s.emissive_color);
            let double_sided = slot.is_some_and(|s| s.double_sided);

            let tri_start = sub.index_start / 3;
            let tri_end = ((sub.index_start + sub.index_count).min(mesh.indices.len())) / 3;
            for tri_idx in (tri_start..tri_end).step_by(tri_step) {
                let i0 = mesh.indices[tri_idx * 3] as usize;
                let i1 = mesh.indices[tri_idx * 3 + 1] as usize;
                let i2 = mesh.indices[tri_idx * 3 + 2] as usize;
                let (Some(Some((p0, z0, n0))), Some(Some((p1, z1, n1))), Some(Some((p2, z2, n2)))) =
                    (projected.get(i0), projected.get(i1), projected.get(i2))
                else {
                    continue;
                };

                let area2 = (p1.x - p0.x) * (p2.y - p0.y) - (p2.x - p0.x) * (p1.y - p0.y);
                if cam.backface_cull && !double_sided && area2 > 0.0 {
                    continue;
                }

                let e1 = [n1[0] - n0[0], n1[1] - n0[1], n1[2] - n0[2]];
                let e2 = [n2[0] - n0[0], n2[1] - n0[1], n2[2] - n0[2]];
                let fnorm = normalize3(cross3(e1, e2));
                let lambert = dot3(fnorm, light_dir).abs().clamp(0.0, 1.0);
                let light = (92.0 + lambert * 163.0).min(255.0);

                let tint_scale = if slot_tex.is_some() {
                    [base_color[0].clamp(0.2, 1.0), base_color[1].clamp(0.2, 1.0), base_color[2].clamp(0.2, 1.0)]
                } else {
                    [0.78 * base_color[0].clamp(0.2, 1.0), 0.87 * base_color[1].clamp(0.2, 1.0), base_color[2].clamp(0.2, 1.0)]
                };
                let tint = Color32::from_rgba_unmultiplied(
                    (light * tint_scale[0] + emissive[0] * 140.0).min(255.0) as u8,
                    (light * tint_scale[1] + emissive[1] * 140.0).min(255.0) as u8,
                    (light * tint_scale[2] + emissive[2] * 140.0).min(255.0) as u8,
                    (base_color[3].clamp(0.15, 1.0) * 255.0) as u8,
                );

                let uv0 = if has_uv { pos2(mesh.uvs[i0][0], 1.0 - mesh.uvs[i0][1]) } else { pos2(0.0, 0.0) };
                let uv1 = if has_uv { pos2(mesh.uvs[i1][0], 1.0 - mesh.uvs[i1][1]) } else { pos2(0.0, 0.0) };
                let uv2 = if has_uv { pos2(mesh.uvs[i2][0], 1.0 - mesh.uvs[i2][1]) } else { pos2(0.0, 0.0) };

                visible_tris.push(TriDraw {
                    avg_z: (z0 + z1 + z2) / 3.0,
                    tex_id: slot_tex,
                    pts: [*p0, *p1, *p2],
                    uvs: [uv0, uv1, uv2],
                    tint,
                });
            }
        }
    }

    visible_tris.sort_unstable_by(|a, b| b.avg_z.total_cmp(&a.avg_z));

    let mut active_tex_mesh: Option<egui::Mesh> = None;
    let flush_tex = |painter: &egui::Painter, buf: &mut Option<egui::Mesh>| {
        if let Some(m) = buf.take()
            && !m.indices.is_empty()
        {
            painter.add(egui::Shape::mesh(m));
        }
    };

    for tri in &visible_tris {
        let tex_id = tri.tex_id;

        if let Some(tid) = tex_id {
            let matches_current = active_tex_mesh
                .as_ref()
                .is_some_and(|m| m.texture_id == tid && m.vertices.len() < 54_000);
            if !matches_current {
                flush_tex(&painter, &mut active_tex_mesh);
                active_tex_mesh = Some(egui::Mesh::with_texture(tid));
            }
            if let Some(ref mut em) = active_tex_mesh {
                let base = em.vertices.len() as u32;
                for k in 0..3 {
                    em.vertices.push(egui::epaint::Vertex {
                        pos: tri.pts[k],
                        uv: tri.uvs[k],
                        color: tri.tint,
                    });
                }
                em.indices.extend_from_slice(&[base, base + 1, base + 2]);
            }
        } else {
            flush_tex(&painter, &mut active_tex_mesh);
            if cam.shaded {
                painter.add(egui::Shape::convex_polygon(
                    tri.pts.to_vec(),
                    tri.tint,
                    Stroke::NONE,
                ));
            }
        }
    }
    flush_tex(&painter, &mut active_tex_mesh);

    if cam.wireframe || !cam.shaded {
        for tri in &visible_tris {
            painter.line_segment([tri.pts[0], tri.pts[1]], wire_stroke);
            painter.line_segment([tri.pts[1], tri.pts[2]], wire_stroke);
            painter.line_segment([tri.pts[2], tri.pts[0]], wire_stroke);
        }
    }

    if cam.show_bones && !bone_segments.is_empty() {
        let bone_stroke = Stroke::new(1.8, Color32::from_rgb(255, 200, 65));
        for &(a, b) in bone_segments {
            if let (Some((pa, _, _)), Some((pb, _, _))) = (project(a), project(b)) {
                painter.line_segment([pa, pb], bone_stroke);
                painter.circle_filled(pb, 2.2, Color32::from_rgb(105, 240, 255));
            }
        }
    }

    if cam.show_axes {
        crate::gl_viewport::paint_axes_gizmo(&painter, rect, cam);
    }
    painter.text(
        pos2(rect.right() - 8.0, rect.top() + 8.0),
        egui::Align2::RIGHT_TOP,
        "CPU Software",
        egui::FontId::monospace(10.0),
        Color32::from_rgba_unmultiplied(180, 185, 200, 175),
    );
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-6 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 1.0, 0.0]
    }
}

fn render_blendshape_sliders(
    ui: &mut egui::Ui,
    scroll_id: &str,
    shapes: &[DecodedBlendShape],
    weights: &mut [f32],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        if ui.small_button("Zero All").clicked() {
            for w in weights.iter_mut() {
                *w = 0.0;
            }
            changed = true;
        }
        ui.weak(format!("{} channel(s)", shapes.len()));
    });
    ScrollArea::vertical()
        .id_salt(scroll_id)
        .max_height(150.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (idx, bs) in shapes.iter().enumerate() {
                let Some(w) = weights.get_mut(idx) else {
                    break;
                };
                let label = if bs.name.is_empty() {
                    format!("Shape #{idx}")
                } else {
                    bs.name.clone()
                };
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Slider::new(w, 0.0..=1.0)
                                .fixed_decimals(2)
                                .text(label),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                });
            }
        });
    changed
}

fn render_scene_3d_view(
    ui: &mut egui::Ui,
    scene: &mut HierarchyPreviewData,
    cam: &mut MeshViewportState,
    workspace_clips: &[(AssetRef, String)],
    workspace_controllers: &[(AssetRef, String)],
    deferred: &mut Option<PreviewAction>,
) {
    let visible_parts = scene.parts.iter().filter(|p| p.visible).count();
    let total_tris: usize = scene
        .parts
        .iter()
        .filter(|p| p.visible)
        .map(|p| p.mesh.indices.len() / 3)
        .sum();
    let total_verts: usize = scene
        .parts
        .iter()
        .filter(|p| p.visible)
        .map(|p| p.mesh.positions.len())
        .sum();

    ui.horizontal_wrapped(|ui| {
        ui.strong(&scene.title);
        ui.separator();
        ui.label(format!(
            "{visible_parts}/{} part(s) • {total_verts} verts • {total_tris} tris • {} bone segs",
            scene.parts.len(),
            scene.bone_segments.len()
        ));
        ui.separator();
        ui.checkbox(&mut cam.use_gpu, "GPU");
        ui.checkbox(&mut cam.shaded, "Shaded");
        ui.checkbox(&mut cam.textured, "Textured");
        ui.checkbox(&mut cam.wireframe, "Wireframe");
        ui.checkbox(&mut cam.show_bones, "Bones");
        ui.checkbox(&mut cam.backface_cull, "Cull Backfaces");
        ui.checkbox(&mut cam.show_axes, "Axes");
        ui.toggle_value(&mut scene.show_parts_panel, "Parts");
        if scene.has_blend_shapes() {
            ui.toggle_value(&mut scene.show_blendshapes_panel, "Morphs");
        }
        if ui.button("Reset Cam").clicked() {
            cam.reset_camera();
        }
    });

    ui.horizontal_wrapped(|ui| {
        ui.weak("Export 3D:");
        for (fmt, label) in [
            (HierarchyExportFormat::Glb, ".glb"),
            (HierarchyExportFormat::Gltf, ".gltf"),
            (HierarchyExportFormat::Fbx, ".fbx"),
            (HierarchyExportFormat::Dae, ".dae"),
            (HierarchyExportFormat::ObjMtl, ".obj+.mtl"),
        ] {
            if ui.small_button(label).clicked() {
                *deferred = Some(PreviewAction::ExportHierarchyFormat(scene.root_ref, fmt));
            }
        }
        ui.separator();
        ui.weak("Unity:");
        for (is_scene, as_pkg, label) in [
            (false, false, ".prefab"),
            (true, false, ".unity"),
            (false, true, ".unitypackage"),
        ] {
            if ui.small_button(label).clicked() {
                *deferred = Some(PreviewAction::ExportPrefab(
                    scene.root_ref,
                    is_scene,
                    as_pkg,
                ));
            }
        }
    });

    if scene.scene_doc.is_some()
        || !workspace_clips.is_empty()
        || !workspace_controllers.is_empty()
        || scene.attached_controller.is_some()
    {
        render_scene_animation_bar(ui, scene, workspace_clips, workspace_controllers, deferred);
    }
    ui.separator();

    let show_parts = scene.show_parts_panel && !scene.parts.is_empty();
    let show_morphs = scene.show_blendshapes_panel && scene.has_blend_shapes();
    if show_parts || show_morphs {
        let avail = ui.available_size();
        let side_w = (avail.x * 0.27).clamp(185.0, 275.0);
        let view_w = (avail.x - side_w - 8.0).max(180.0);
        let height = avail.y.max(260.0);
        let mut morphs_changed = false;
        ui.horizontal_top(|ui| {
            ui.allocate_ui(vec2(side_w, height), |ui| {
                if show_parts {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.strong(format!("Parts ({})", scene.parts.len()));
                            if ui.small_button("All").clicked() {
                                for p in &mut scene.parts {
                                    p.visible = true;
                                }
                            }
                            if ui.small_button("None").clicked() {
                                for p in &mut scene.parts {
                                    p.visible = false;
                                }
                            }
                        });
                        ui.separator();
                        let mut solo_idx: Option<usize> = None;
                        ScrollArea::vertical()
                            .id_salt("scene3d_parts_scroll")
                            .max_height(if show_morphs { height * 0.42 } else { height })
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (idx, part) in scene.parts.iter_mut().enumerate() {
                                    ui.horizontal(|ui| {
                                        ui.checkbox(&mut part.visible, &part.label);
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui.small_button("Solo").clicked() {
                                                    solo_idx = Some(idx);
                                                }
                                            },
                                        );
                                    });
                                }
                            });
                        if let Some(target) = solo_idx {
                            for (idx, part) in scene.parts.iter_mut().enumerate() {
                                part.visible = idx == target;
                            }
                        }
                    });
                }
                if show_morphs {
                    ui.group(|ui| {
                        ui.strong("BlendShapes / Morphs");
                        ui.separator();
                        for (p_idx, part) in scene.parts.iter_mut().enumerate() {
                            if part.blend_shapes.is_empty() {
                                continue;
                            }
                            ui.small(&part.label);
                            if render_blendshape_sliders(
                                ui,
                                &format!("scene3d_morphs_{p_idx}"),
                                &part.blend_shapes,
                                &mut part.blend_weights,
                            ) {
                                morphs_changed = true;
                            }
                        }
                    });
                }
            });
            ui.allocate_ui(vec2(view_w, height), |ui| {
                render_scene_viewport(
                    ui,
                    &scene.parts,
                    &scene.bone_segments,
                    scene.bounds_min,
                    scene.bounds_max,
                    cam,
                );
            });
        });
        if morphs_changed {
            if scene.animator_mode && scene.attached_controller.is_some() {
                step_and_apply_animator_sim(scene, 0.0);
            } else {
                apply_scene_animation_pose(scene, scene.anim_clip_idx, scene.anim_time_secs);
            }
        }
    } else {
        render_scene_viewport(
            ui,
            &scene.parts,
            &scene.bone_segments,
            scene.bounds_min,
            scene.bounds_max,
            cam,
        );
    }
}

fn render_scene_animation_bar(
    ui: &mut egui::Ui,
    scene: &mut HierarchyPreviewData,
    workspace_clips: &[(AssetRef, String)],
    workspace_controllers: &[(AssetRef, String)],
    deferred: &mut Option<PreviewAction>,
) {
    let Some(doc) = scene.scene_doc.clone() else {
        scene.anim_playing = false;
        scene.last_ui_time = None;
        return;
    };

    let ctrl_name = scene.attached_controller.as_ref().map(|c| c.name.clone());
    ui.horizontal_wrapped(|ui| {
        if let Some(ctrl_name) = ctrl_name {
            ui.weak("Mode:");
            if ui.selectable_label(!scene.animator_mode, "Clip").clicked() && scene.animator_mode {
                scene.animator_mode = false;
                scene.last_ui_time = None;
                apply_scene_animation_pose(scene, scene.anim_clip_idx, scene.anim_time_secs);
            }
            if ui
                .selectable_label(scene.animator_mode, format!("Animator SM ({ctrl_name})"))
                .clicked()
                && !scene.animator_mode
            {
                scene.animator_mode = true;
                scene.last_ui_time = scene.anim_playing.then(|| ui.input(|i| i.time));
                step_and_apply_animator_sim(scene, 0.0);
            }
            ui.separator();
        }

        if !workspace_controllers.is_empty() {
            egui::ComboBox::from_id_salt("scene_attach_ws_ctrl")
                .selected_text("+ Attach Ctrl...")
                .show_ui(ui, |ui| {
                    for (ctrl_ref, label) in workspace_controllers {
                        if ui.selectable_label(false, label).clicked() {
                            *deferred = Some(PreviewAction::AttachControllerToScene(*ctrl_ref));
                        }
                    }
                });
        }
    });

    if scene.animator_mode && scene.attached_controller.is_some() {
        render_scene_animator_sm_bar(ui, scene);
        return;
    }

    let selected_label = match scene.anim_clip_idx {
        Some(idx) => doc
            .animations
            .get(idx)
            .map(|c| format!("{} ({:.2}s)", c.name, c.duration_secs))
            .unwrap_or_else(|| "<Bind Pose>".to_owned()),
        None => "<Bind Pose>".to_owned(),
    };

    let mut next_clip = scene.anim_clip_idx;
    let mut pose_dirty = false;

    ui.horizontal_wrapped(|ui| {
        ui.weak("Anim:");
        egui::ComboBox::from_id_salt("scene_anim_clip_picker")
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(next_clip.is_none(), "<Bind Pose>")
                    .clicked()
                {
                    next_clip = None;
                }
                for (i, clip) in doc.animations.iter().enumerate() {
                    let lbl = format!(
                        "{} ({:.2}s, {} trk)",
                        clip.name,
                        clip.duration_secs,
                        clip.tracks.len()
                    );
                    if ui.selectable_label(next_clip == Some(i), lbl).clicked() {
                        next_clip = Some(i);
                    }
                }
            });

        if !workspace_clips.is_empty() {
            egui::ComboBox::from_id_salt("scene_attach_ws_clip")
                .selected_text("+ Attach Clip...")
                .show_ui(ui, |ui| {
                    for (clip_ref, label) in workspace_clips {
                        if ui.selectable_label(false, label).clicked() {
                            *deferred = Some(PreviewAction::AttachClipToScene(*clip_ref));
                        }
                    }
                });
        }

        if next_clip != scene.anim_clip_idx {
            scene.anim_clip_idx = next_clip;
            scene.anim_time_secs = 0.0;
            scene.anim_playing = next_clip.is_some();
            scene.last_ui_time = scene.anim_playing.then(|| ui.input(|i| i.time));
            pose_dirty = true;
        }

        if let Some(clip_idx) = scene.anim_clip_idx
            && let Some(clip) = doc.animations.get(clip_idx)
        {
            let duration = clip.duration_secs.max(1e-4);
            let play_label = if scene.anim_playing { "⏸ Pause" } else { "▶ Play" };
            if ui.button(play_label).clicked() {
                scene.anim_playing = !scene.anim_playing;
                scene.last_ui_time = scene.anim_playing.then(|| ui.input(|i| i.time));
                if scene.anim_playing && scene.anim_time_secs >= duration && !scene.anim_looping {
                    scene.anim_time_secs = 0.0;
                    pose_dirty = true;
                }
            }
            if ui.button("⏮ Reset").clicked() {
                scene.anim_playing = false;
                scene.anim_time_secs = 0.0;
                scene.last_ui_time = None;
                pose_dirty = true;
            }
            ui.checkbox(&mut scene.anim_looping, "Loop");

            ui.weak("Speed:");
            egui::ComboBox::from_id_salt("scene_anim_speed")
                .width(58.0)
                .selected_text(format!("{:.2}x", scene.anim_speed))
                .show_ui(ui, |ui| {
                    for spd in [0.25_f32, 0.5, 1.0, 1.5, 2.0] {
                        ui.selectable_value(&mut scene.anim_speed, spd, format!("{spd:.2}x"));
                    }
                });

            let slider = egui::Slider::new(&mut scene.anim_time_secs, 0.0..=duration)
                .fixed_decimals(2)
                .suffix("s");
            if ui.add(slider).changed() {
                scene.anim_playing = false;
                scene.last_ui_time = None;
                pose_dirty = true;
            }
            ui.weak(format!("/ {duration:.2}s"));

            if scene.anim_playing {
                let now = ui.input(|i| i.time);
                let dt = scene
                    .last_ui_time
                    .map(|prev| (now - prev).clamp(0.0, 0.1) as f32)
                    .unwrap_or(0.0);
                scene.last_ui_time = Some(now);
                if dt > 0.0 {
                    scene.anim_time_secs += dt * scene.anim_speed;
                    if scene.anim_time_secs > duration {
                        if scene.anim_looping {
                            scene.anim_time_secs %= duration;
                        } else {
                            scene.anim_time_secs = duration;
                            scene.anim_playing = false;
                        }
                    }
                    pose_dirty = true;
                }
                if scene.anim_playing {
                    ui.ctx().request_repaint();
                }
            } else {
                scene.last_ui_time = None;
            }
        } else {
            scene.anim_playing = false;
            scene.last_ui_time = None;
        }
    });

    if pose_dirty {
        apply_scene_animation_pose(scene, scene.anim_clip_idx, scene.anim_time_secs);
    }
}

fn render_scene_animator_sm_bar(ui: &mut egui::Ui, scene: &mut HierarchyPreviewData) {
    let Some(ctrl) = scene.attached_controller.take() else {
        return;
    };
    let mut param_or_state_changed = false;

    if let Some(ref mut sim) = scene.animator_sim {
        ui.horizontal_wrapped(|ui| {
            let play_label = if scene.anim_playing { "⏸ Pause" } else { "▶ Run SM" };
            if ui.button(play_label).clicked() {
                scene.anim_playing = !scene.anim_playing;
                scene.last_ui_time = scene.anim_playing.then(|| ui.input(|i| i.time));
            }
            if ui.button("⏮ Reset SM").clicked() {
                sim.reset(&ctrl);
                scene.last_ui_time = scene.anim_playing.then(|| ui.input(|i| i.time));
                param_or_state_changed = true;
            }

            if ctrl.layers.len() > 1 {
                let cur_layer = sim.layer_idx.min(ctrl.layers.len() - 1);
                let layer_lbl = &ctrl.layers[cur_layer].name;
                egui::ComboBox::from_id_salt("scene_sm_layer_picker")
                    .selected_text(layer_lbl)
                    .show_ui(ui, |ui| {
                        for (i, layer) in ctrl.layers.iter().enumerate() {
                            if ui.selectable_label(sim.layer_idx == i, &layer.name).clicked() {
                                sim.layer_idx = i;
                                sim.reset(&ctrl);
                                param_or_state_changed = true;
                            }
                        }
                    });
            }

            if let Some(sm) = ctrl
                .layers
                .get(sim.layer_idx)
                .and_then(|l| ctrl.state_machines.get(l.state_machine_idx))
            {
                let cur_name = sm
                    .states
                    .iter()
                    .find(|s| s.index == sim.current_state_idx)
                    .map_or("?", |s| s.name.as_str());
                if let Some(tr) = sim.active_transition {
                    let dst_name = sm
                        .states
                        .iter()
                        .find(|s| s.index == tr.target_state_idx)
                        .map_or("?", |s| s.name.as_str());
                    let pct = (tr.elapsed_secs / tr.duration_secs.max(1e-4)).clamp(0.0, 1.0);
                    ui.colored_label(
                        Color32::from_rgb(130, 215, 255),
                        format!("{cur_name} → {dst_name} ({:.0}%)", pct * 100.0),
                    );
                } else {
                    ui.colored_label(
                        Color32::from_rgb(135, 235, 160),
                        format!("State: {cur_name} ({:.2}s)", sim.state_time_secs),
                    );
                }
                if let Some(cur_state) = sm.states.iter().find(|s| s.index == sim.current_state_idx) {
                    let bt_weights = cur_state.evaluate_clip_weights(&sim.params);
                    if bt_weights.len() > 1 {
                        let summary: Vec<String> = bt_weights
                            .iter()
                            .take(4)
                            .map(|&(cid, wt)| {
                                let name = ctrl
                                    .clip_links
                                    .iter()
                                    .find(|l| l.index == cid)
                                    .map_or_else(|| format!("#{cid}"), |l| l.name_hint.clone());
                                format!("{name} {:.0}%", wt * 100.0)
                            })
                            .collect();
                        ui.weak(format!("[{}]", summary.join(" · ")));
                    }
                }
            }
        });

        if !ctrl.parameters.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.weak("Params:");
                for p in &ctrl.parameters {
                    match p.kind_label {
                        "Float" => {
                            let val = match sim.params.get_mut(&p.name) {
                                Some(SimParamValue::Float(v)) => v,
                                _ => {
                                    sim.params.insert(p.name.clone(), SimParamValue::Float(0.0));
                                    match sim.params.get_mut(&p.name) {
                                        Some(SimParamValue::Float(v)) => v,
                                        _ => continue,
                                    }
                                }
                            };
                            ui.label(&p.name);
                            if ui.add(egui::DragValue::new(val).speed(0.05)).changed() {
                                param_or_state_changed = true;
                            }
                        }
                        "Int" => {
                            let val = match sim.params.get_mut(&p.name) {
                                Some(SimParamValue::Int(v)) => v,
                                _ => {
                                    sim.params.insert(p.name.clone(), SimParamValue::Int(0));
                                    match sim.params.get_mut(&p.name) {
                                        Some(SimParamValue::Int(v)) => v,
                                        _ => continue,
                                    }
                                }
                            };
                            ui.label(&p.name);
                            if ui.add(egui::DragValue::new(val)).changed() {
                                param_or_state_changed = true;
                            }
                        }
                        "Bool" => {
                            let val = match sim.params.get_mut(&p.name) {
                                Some(SimParamValue::Bool(b)) => b,
                                _ => {
                                    sim.params.insert(p.name.clone(), SimParamValue::Bool(false));
                                    match sim.params.get_mut(&p.name) {
                                        Some(SimParamValue::Bool(b)) => b,
                                        _ => continue,
                                    }
                                }
                            };
                            if ui.checkbox(val, &p.name).changed() {
                                param_or_state_changed = true;
                            }
                        }
                        "Trigger" if ui.button(format!("⚡ {}", p.name)).clicked() => {
                            sim.fire_trigger(&p.name);
                            scene.anim_playing = true;
                            scene.last_ui_time = Some(ui.input(|i| i.time));
                            param_or_state_changed = true;
                        }
                        _ => {}
                    }
                }
            });
        }
    }

    scene.attached_controller = Some(ctrl);

    let mut dt = 0.0f32;
    if scene.anim_playing {
        let now = ui.input(|i| i.time);
        dt = scene
            .last_ui_time
            .map(|prev| (now - prev).clamp(0.0, 0.1) as f32)
            .unwrap_or(0.0)
            * scene.anim_speed;
        scene.last_ui_time = Some(now);
        ui.ctx().request_repaint();
    } else {
        scene.last_ui_time = None;
    }

    if param_or_state_changed || dt > 0.0 {
        step_and_apply_animator_sim(scene, dt);
    }
}

fn render_animator_controller_view(
    ui: &mut egui::Ui,
    controller: &DecodedAnimatorController,
    selected_layer: &mut usize,
    deferred: &mut Option<PreviewAction>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.heading(&controller.name);
        ui.separator();
        ui.label(format!(
            "{} param(s) • {} layer(s) • {} state machine(s) • {} clip link(s)",
            controller.parameters.len(),
            controller.layers.len(),
            controller.state_machines.len(),
            controller.clip_links.len()
        ));
    });
    ui.separator();

    ScrollArea::vertical()
        .id_salt("animator_controller_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::CollapsingHeader::new(format!("Parameters ({})", controller.parameters.len()))
                .default_open(true)
                .show(ui, |ui| {
                    if controller.parameters.is_empty() {
                        ui.weak("No parameters defined.");
                        return;
                    }
                    egui::Grid::new("animator_params_grid")
                        .striped(true)
                        .min_col_width(110.0)
                        .show(ui, |ui| {
                            ui.strong("Name");
                            ui.strong("Type");
                            ui.strong("Default");
                            ui.end_row();
                            for p in &controller.parameters {
                                ui.monospace(&p.name);
                                ui.label(p.kind_label);
                                ui.monospace(&p.default_display);
                                ui.end_row();
                            }
                        });
                });

            egui::CollapsingHeader::new(format!(
                "Linked AnimationClips ({})",
                controller.clip_links.len()
            ))
            .default_open(false)
            .show(ui, |ui| {
                if controller.clip_links.is_empty() {
                    ui.weak("No referenced AnimationClips.");
                    return;
                }
                for link in &controller.clip_links {
                    if let Some(r) = link.asset_ref {
                        if ui
                            .link(format!(
                                "[{}] {} (#{}:{})",
                                link.index, link.name_hint, r.file_index, r.path_id
                            ))
                            .clicked()
                        {
                            *deferred = Some(PreviewAction::SelectAsset(r));
                        }
                    } else {
                        ui.weak(format!("[{}] {} (external)", link.index, link.name_hint));
                    }
                }
            });

            ui.separator();
            if controller.layers.is_empty() {
                ui.weak("Controller has no layers.");
                return;
            }
            if *selected_layer >= controller.layers.len() {
                *selected_layer = 0;
            }
            ui.horizontal_wrapped(|ui| {
                ui.strong("Layer:");
                for (idx, layer) in controller.layers.iter().enumerate() {
                    let lbl = format!(
                        "{} (w={:.2}, blend={})",
                        layer.name, layer.default_weight, layer.blending_mode
                    );
                    ui.selectable_value(selected_layer, idx, lbl);
                }
            });
            ui.separator();

            let layer = &controller.layers[*selected_layer];
            let Some(sm) = controller.state_machines.get(layer.state_machine_idx) else {
                ui.weak("Selected layer references missing state machine index.");
                return;
            };

            if !sm.any_state_transitions.is_empty() {
                egui::CollapsingHeader::new(format!(
                    "AnyState Transitions ({})",
                    sm.any_state_transitions.len()
                ))
                .default_open(true)
                .show(ui, |ui| {
                    for tr in &sm.any_state_transitions {
                        render_animator_transition(ui, tr, sm);
                    }
                });
            }

            ui.strong(format!("States ({})", sm.states.len()));
            ui.add_space(4.0);
            for st in &sm.states {
                let is_def = st.is_default || st.index == sm.default_state;
                ui.group(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        let title = if is_def {
                            format!("[DEFAULT] {} (#{})", st.name, st.index)
                        } else {
                            format!("{} (#{})", st.name, st.index)
                        };
                        let rich = if is_def {
                            RichText::new(title).strong().color(Color32::from_rgb(130, 225, 145))
                        } else {
                            RichText::new(title).strong()
                        };
                        ui.label(rich);
                        ui.weak(format!(
                            "speed: {:.2} | loop: {}{}",
                            st.speed,
                            st.is_loop,
                            if st.tag.is_empty() {
                                String::new()
                            } else {
                                format!(" | tag: {}", st.tag)
                            }
                        ));
                    });

                    if let Some(root_bt) = st.blend_tree.first()
                        && root_bt.kind != BlendTreeKind::SimpleClip
                    {
                        let p_lbl = match (&root_bt.param_x, &root_bt.param_y) {
                            (Some(x), Some(y)) => format!("{x}, {y}"),
                            (Some(x), None) => x.clone(),
                            _ => "Direct/Default".to_owned(),
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(
                                Color32::from_rgb(150, 210, 255),
                                format!("BlendTree: {} ({})", root_bt.kind.label(), p_lbl),
                            );
                            for (ci, &child_node_idx) in root_bt.child_indices.iter().enumerate() {
                                let cname = st
                                    .blend_tree
                                    .get(child_node_idx)
                                    .and_then(|n| n.clip_id)
                                    .and_then(|cid| {
                                        controller.clip_links.iter().find(|l| l.index == cid)
                                    })
                                    .map_or_else(
                                        || format!("node[{child_node_idx}]"),
                                        |l| l.name_hint.clone(),
                                    );
                                let coord = if let Some(&th) = root_bt.thresholds_1d.get(ci) {
                                    format!("@{th:.2}")
                                } else if let Some(&[x, y]) = root_bt.positions_2d.get(ci) {
                                    format!("@({x:.2},{y:.2})")
                                } else if let Some(Some(dp)) = root_bt.direct_params.get(ci) {
                                    format!("<- {dp}")
                                } else {
                                    String::new()
                                };
                                ui.monospace(format!("{cname}{coord}"));
                            }
                        });
                    }

                    if !st.leaf_clip_indices.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.weak("Clips:");
                            for &clip_idx in &st.leaf_clip_indices {
                                if let Some(link) =
                                    controller.clip_links.iter().find(|l| l.index == clip_idx)
                                {
                                    if let Some(r) = link.asset_ref {
                                        if ui.link(&link.name_hint).clicked() {
                                            *deferred = Some(PreviewAction::SelectAsset(r));
                                        }
                                    } else {
                                        ui.weak(format!("{} (external)", link.name_hint));
                                    }
                                } else {
                                    ui.monospace(format!("clip[{clip_idx}]"));
                                }
                            }
                        });
                    }

                    for tr in &st.transitions {
                        render_animator_transition(ui, tr, sm);
                    }
                });
                ui.add_space(4.0);
            }
        });
}

fn render_animator_transition(
    ui: &mut egui::Ui,
    tr: &AnimatorTransitionDef,
    sm: &AnimatorStateMachineDef,
) {
    let dest = if tr.is_exit {
        "[EXIT]".to_owned()
    } else if let Some(idx) = tr.destination_state_idx {
        sm.states
            .iter()
            .find(|s| s.index == idx)
            .map(|s| format!("{} (#{idx})", s.name))
            .unwrap_or_else(|| format!("state#{idx}"))
    } else {
        "<unknown>".to_owned()
    };

    let mut summary = format!(
        "-> {dest}  (dur={:.2}s{})",
        tr.duration,
        if tr.has_exit_time {
            format!(", exit={:.2}", tr.exit_time)
        } else {
            String::new()
        }
    );
    if !tr.conditions.is_empty() {
        let conds: Vec<String> = tr
            .conditions
            .iter()
            .map(|c| format!("{} {} {:.2}", c.param_name, c.mode_label, c.threshold))
            .collect();
        summary.push_str("  if ");
        summary.push_str(&conds.join(" && "));
    }
    ui.monospace(summary);
}

#[cfg(test)]
mod ui_affine_preview_tests {
    use super::*;
    use devx_codecs::animation::DecodedUiAnimTrack;

    fn clip(channels: &[(UiAnimChannel, f32)]) -> DecodedUiAnimationClip {
        DecodedUiAnimationClip {
            name: "native-property-values".to_owned(),
            duration_secs: 1.0,
            sample_rate: 60.0,
            wrap_mode: 0,
            tracks: channels.iter().map(|&(channel, value)| DecodedUiAnimTrack {
                path_hash: 0,
                path_name: Some(String::new()),
                channel,
                times: vec![0.0],
                values: vec![value],
            }).collect(),
        }
    }

    fn assert_rect_near(actual: Rect, expected: [f32; 4]) {
        for (actual, expected) in [actual.min.x, actual.min.y, actual.width(), actual.height()]
            .into_iter().zip(expected)
        {
            assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
        }
    }

    #[test]
    fn non_square_artwork_keeps_axis_lengths_and_cumulative_rotation() {
        let node = UiCanvasNode {
            canvas_rect: [530.0, 360.0, 20.0, 40.0],
            canvas_render_rect: Some([520.0, 370.0, 40.0, 20.0]),
            rotation_z_deg: 0.0,
            canvas_rotation_z_deg: Some(-90.0),
            ..Default::default()
        };
        let geometry = ui_node_paint_geometry(&node, sample_ui_anim_for_path(&[], "", 0.0, &node));
        assert_rect_near(geometry.bounds, [530.0, 360.0, 20.0, 40.0]);
        assert_rect_near(geometry.artwork, [520.0, 370.0, 40.0, 20.0]);
        assert_eq!(geometry.rotation_deg, -90.0);
        assert!(!geometry.flip_x && !geometry.flip_y);
    }

    #[test]
    fn counter_rotated_footer_graphic_with_children_stays_upright() {
        let node = UiCanvasNode {
            canvas_rect: [477.0, 950.0, 887.0, 45.0],
            canvas_render_rect: Some([477.0, 950.0, 887.0, 45.0]),
            rotation_z_deg: -180.0,
            canvas_rotation_z_deg: Some(0.0),
            children: vec![UiCanvasNode::default()],
            ..Default::default()
        };
        let geometry = ui_node_paint_geometry(&node, sample_ui_anim_for_path(&[], "", 0.0, &node));
        assert_eq!(geometry.rotation_deg, 0.0);
        assert_rect_near(geometry.bounds, [477.0, 950.0, 887.0, 45.0]);
    }

    #[test]
    fn anchored_animation_uses_absolute_values_and_inverted_parent_basis() {
        let node = UiCanvasNode {
            canvas_rect: [100.0, 200.0, 80.0, 20.0],
            anchored_position: [10.0, -217.0],
            scale: [2.0, 1.0, 1.0],
            canvas_rotation_z_deg: Some(0.0),
            canvas_parent_basis: Some([[-1.0, 0.0], [0.0, 1.0]]),
            ..Default::default()
        };
        let native = clip(&[
            (UiAnimChannel::AnchoredPosX, 10.0),
            (UiAnimChannel::AnchoredPosY, -153.0),
            (UiAnimChannel::LocalScaleX, 2.0),
        ]);
        let geometry = ui_node_paint_geometry(&node, sample_ui_anim_for_path(&[native], "", 0.0, &node));
        assert_rect_near(geometry.bounds, [100.0, 264.0, 80.0, 20.0]);
        assert_rect_near(geometry.artwork, [100.0, 264.0, 80.0, 20.0]);
    }

    #[test]
    fn animated_rotation_keeps_native_pivot_and_parent_handedness() {
        let node = UiCanvasNode {
            canvas_rect: [100.0, 100.0, 40.0, 20.0],
            pivot: [0.0, 1.0],
            canvas_rotation_z_deg: Some(0.0),
            ..Default::default()
        };
        let native = clip(&[(UiAnimChannel::LocalEulerZDeg, 90.0)]);
        let anim = sample_ui_anim_for_path(&[native], "", 0.0, &node);
        let geometry = ui_node_paint_geometry(&node, anim);
        assert_rect_near(geometry.bounds, [100.0, 60.0, 20.0, 40.0]);
        assert_eq!(geometry.rotation_deg, -90.0);
        let reflected = UiCanvasNode {
            canvas_parent_basis: Some([[-1.0, 0.0], [0.0, -1.0]]),
            ..node
        };
        assert_eq!(ui_node_paint_geometry(&reflected, anim).rotation_deg, 90.0);
    }
}
