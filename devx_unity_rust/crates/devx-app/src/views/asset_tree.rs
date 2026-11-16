//! Left dockable Asset Explorer panel supporting multi-mode grouping and virtualized scrolling.

use crate::state::DevXState;
use devx_core::tree::{TreeGroupingMode, TreeNodeKind};
use devx_exporter::hierarchy::HierarchyExportFormat;
use devx_plugin_api::AssetRef;
use egui::{Color32, RichText, ScrollArea};

/// Renders the multi-mode Asset Tree panel.
pub fn show(ui: &mut egui::Ui, state: &mut DevXState) {
    let Some(catalog) = state.catalog.as_ref().cloned() else {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.label(RichText::new("No workspace loaded").strong());
            ui.label("Open a Unity file or game directory to browse assets.");
        });
        return;
    };

    // Controls: grouping mode + instant text filter
    ui.horizontal(|ui| {
        ui.label("Mode:");
        let prev_mode = state.grouping_mode;
        egui::ComboBox::from_id_salt("tree_grouping_mode")
            .selected_text(state.grouping_mode.label())
            .show_ui(ui, |ui| {
                for mode in [
                    TreeGroupingMode::ByClass,
                    TreeGroupingMode::ByContainer,
                    TreeGroupingMode::BySceneHierarchy,
                    TreeGroupingMode::Flat,
                ] {
                    ui.selectable_value(&mut state.grouping_mode, mode, mode.label());
                }
            });
        if state.grouping_mode != prev_mode {
            state.expanded_nodes.clear();
            state.tree_dirty = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("Filter:");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.filter.search_text)
                .hint_text("name, class, container, PathID...")
                .desired_width(ui.available_width() - 32.0),
        );
        if resp.changed() {
            state.tree_dirty = true;
        }
        if !state.filter.search_text.is_empty() && ui.small_button("×").clicked() {
            state.filter.search_text.clear();
            state.tree_dirty = true;
        }
    });

    ui.horizontal(|ui| {
        // Class filter
        let prev_cid = state.filter.class_id;
        let selected_cls_label = state
            .filter
            .class_id
            .and_then(|cid| {
                catalog
                    .entries
                    .iter()
                    .find(|e| e.summary.class_id == cid)
                    .map(|e| e.summary.class_name.as_str())
            })
            .unwrap_or("All Types");
        egui::ComboBox::from_id_salt("tree_class_filter")
            .width(125.0)
            .selected_text(selected_cls_label)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut state.filter.class_id, None, "All Types");
                for ((cid, cls_name), count) in &catalog.class_counts {
                    ui.selectable_value(
                        &mut state.filter.class_id,
                        Some(*cid),
                        format!("{cls_name} ({count})"),
                    );
                }
            });
        if state.filter.class_id != prev_cid {
            state.tree_dirty = true;
        }

        // File filter
        let prev_fi = state.filter.file_index;
        let selected_file_label = state
            .filter
            .file_index
            .and_then(|fi| catalog.workspace.get_file(fi))
            .map_or("All Files", |f| f.virtual_path.as_str());
        egui::ComboBox::from_id_salt("tree_file_filter")
            .width(120.0)
            .selected_text(short_tail(selected_file_label, 18))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut state.filter.file_index, None, "All Files");
                for f in &catalog.workspace.files {
                    ui.selectable_value(
                        &mut state.filter.file_index,
                        Some(f.file_index),
                        format!("{} ({})", short_tail(&f.virtual_path, 34), f.file.objects.len()),
                    );
                }
            });
        if state.filter.file_index != prev_fi {
            state.tree_dirty = true;
        }
    });

    if state.tree_dirty {
        state.rebuild_tree_now();
    }

    let mut deferred_load_indices: Option<Vec<usize>> = None;
    let mut deferred_extract_filter: Option<String> = None;

    if !catalog.workspace.deferred_bundles.is_empty() {
        let total_def = catalog.workspace.deferred_bundles.len();
        let unloaded_def = catalog
            .workspace
            .deferred_bundles
            .iter()
            .filter(|d| !d.loaded)
            .count();
        egui::CollapsingHeader::new(format!(
            "Archive .ab Bundles ({unloaded_def} unloaded / {total_def})"
        ))
        .default_open(catalog.entries.is_empty() || unloaded_def > 0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut state.deferred_bundle_filter)
                        .hint_text("filter .ab (e.g. shader, scene, ui)...")
                        .desired_width(ui.available_width() - 140.0),
                );
                let needle = state.deferred_bundle_filter.trim().to_ascii_lowercase();
                if ui.button("Load Matching").clicked() {
                    let indices: Vec<usize> = catalog
                        .workspace
                        .deferred_bundles
                        .iter()
                        .enumerate()
                        .filter(|(_, d)| {
                            !d.loaded
                                && (needle.is_empty()
                                    || d.virtual_path.to_ascii_lowercase().contains(&needle))
                        })
                        .map(|(i, _)| i)
                        .collect();
                    deferred_load_indices = Some(indices);
                }
                if ui.button("Extract .ab...").clicked() {
                    deferred_extract_filter = Some(needle);
                }
                ui.checkbox(&mut state.load_deps_on_bundle_open, "+Deps")
                    .on_hover_text("Automatically resolve and load transitive AssetBundle dependencies (m_Dependencies / CAB-*)");
            });

            let needle = state.deferred_bundle_filter.trim().to_ascii_lowercase();
            let matches: Vec<(usize, &devx_unity_io::DeferredBundleEntry)> = catalog
                .workspace
                .deferred_bundles
                .iter()
                .enumerate()
                .filter(|(_, d)| {
                    needle.is_empty() || d.virtual_path.to_ascii_lowercase().contains(&needle)
                })
                .take(80)
                .collect();

            ScrollArea::vertical()
                .id_salt("deferred_ab_scroll")
                .max_height(140.0)
                .show(ui, |ui| {
                    for (idx, d) in matches {
                        ui.horizontal(|ui| {
                            let status = if d.loaded { "[loaded]" } else { "[lazy]" };
                            ui.small(status);
                            ui.label(short_tail(&d.entry_name, 34));
                            ui.weak(format!("{} KB", d.uncompressed_size / 1024));
                            if !d.loaded && ui.small_button("Load").clicked() {
                                deferred_load_indices = Some(vec![idx]);
                            }
                        });
                    }
                });
        });
        ui.separator();
    }

    ui.horizontal(|ui| {
        ui.small(format!(
            "{} rows | {} total assets",
            state.flat_rows.len(),
            catalog.entries.len()
        ));
        if ui.small_button("Collapse").clicked() {
            state.expanded_nodes.clear();
            state.tree_dirty = true;
        }
        if ui.small_button("Expand").clicked() {
            let keys: Vec<String> = state
                .cached_tree
                .iter()
                .enumerate()
                .map(|(i, n)| format!("0/{i}:{}", n.label))
                .collect();
            for k in keys {
                state.expanded_nodes.insert(k);
            }
            state.tree_dirty = true;
        }
    });

    ui.separator();

    let row_height = 20.0;
    let total_rows = state.flat_rows.len();
    let mut toggle_key: Option<String> = None;
    let mut select_req: Option<AssetRef> = None;
    let mut export_req: Option<AssetRef> = None;
    let mut export_hier_req: Option<(AssetRef, HierarchyExportFormat)> = None;
    let mut export_prefab_req: Option<(AssetRef, bool, bool)> = None;

    ScrollArea::vertical()
        .id_salt("asset_tree_scroll")
        .auto_shrink([false, false])
        .show_rows(ui, row_height, total_rows, |ui, range| {
            for row_idx in range {
                let row = &state.flat_rows[row_idx];
                ui.horizontal(|ui| {
                    ui.add_space(row.depth as f32 * 14.0);
                    if row.has_children {
                        let arrow = if row.expanded { "▼" } else { "▶" };
                        if ui.small_button(arrow).clicked() {
                            toggle_key = Some(row.key.clone());
                        }
                    } else {
                        ui.add_space(16.0);
                    }

                    match row.kind {
                        TreeNodeKind::Group { .. } => {
                            let resp = ui.add(
                                egui::Button::new(RichText::new(&row.label).strong())
                                    .frame(false),
                            );
                            if resp.clicked() && row.has_children {
                                toggle_key = Some(row.key.clone());
                            }
                        }
                        TreeNodeKind::Asset(asset_ref) => {
                            let is_sel = state.selected == Some(asset_ref);
                            let class_id = catalog
                                .entry(asset_ref)
                                .map_or(0, |e| e.summary.class_id);
                            let (badge, color) = class_badge(class_id);
                            ui.label(RichText::new(badge).monospace().small().color(color));

                            let resp = ui.selectable_label(is_sel, &row.label);
                            if resp.clicked() {
                                select_req = Some(asset_ref);
                            }
                            resp.context_menu(|ui| {
                                if ui.button("Export Asset...").clicked() {
                                    export_req = Some(asset_ref);
                                    ui.close();
                                }
                                if matches!(class_id, 1 | 4 | 23 | 33 | 43 | 137 | 142 | 224) {
                                    ui.separator();
                                    for (fmt, label) in [
                                        (HierarchyExportFormat::Fbx, "Export Hierarchy (.fbx)..."),
                                        (HierarchyExportFormat::Glb, "Export Hierarchy (.glb)..."),
                                        (HierarchyExportFormat::Gltf, "Export Hierarchy (.gltf)..."),
                                        (HierarchyExportFormat::Dae, "Export Hierarchy (.dae)..."),
                                        (HierarchyExportFormat::ObjMtl, "Export Hierarchy (.obj + .mtl)..."),
                                    ] {
                                        if ui.button(label).clicked() {
                                            export_hier_req = Some((asset_ref, fmt));
                                            ui.close();
                                        }
                                    }
                                    ui.separator();
                                    for (is_scene, as_pkg, label) in [
                                        (false, false, "Export Prefab (.prefab)..."),
                                        (true, false, "Export Scene (.unity)..."),
                                        (false, true, "Export .unitypackage..."),
                                    ] {
                                        if ui.button(label).clicked() {
                                            export_prefab_req = Some((asset_ref, is_scene, as_pkg));
                                            ui.close();
                                        }
                                    }
                                    ui.separator();
                                }
                                if ui.button("Copy PathID").clicked() {
                                    ui.ctx().copy_text(asset_ref.path_id.to_string());
                                    ui.close();
                                }
                            });
                            if let Some(ref sub) = row.subtitle {
                                ui.weak(sub);
                            }
                        }
                    }
                });
            }
        });

    if let Some(key) = toggle_key {
        state.toggle_tree_node(&key);
    }
    if let Some(asset_ref) = select_req {
        let ctx = ui.ctx().clone();
        state.select_asset(asset_ref, true, Some(&ctx));
    }
    if let Some(asset_ref) = export_req {
        state.export_selected_interactive(asset_ref, None);
    }
    if let Some((asset_ref, fmt)) = export_hier_req {
        state.export_hierarchy_interactive(asset_ref, fmt);
    }
    if let Some((asset_ref, is_scene, as_pkg)) = export_prefab_req {
        state.export_prefab_interactive(asset_ref, is_scene, as_pkg);
    }
    let workspace_path = catalog.workspace.root_path.clone();
    drop(catalog);

    if let Some(indices) = deferred_load_indices {
        state.load_deferred_bundles_async(indices);
    }
    if let Some(filter_str) = deferred_extract_filter
        && let Some(dir) = rfd::FileDialog::new().pick_folder()
    {
        let filter = (!filter_str.is_empty()).then_some(filter_str);
        state.spawn_extract_raw_bundles(workspace_path, dir, filter);
    }
}

fn class_badge(class_id: i32) -> (&'static str, Color32) {
    match class_id {
        28 => ("[TEX]", Color32::from_rgb(90, 200, 250)),
        213 => ("[SPR]", Color32::from_rgb(120, 220, 255)),
        83 => ("[AUD]", Color32::from_rgb(110, 230, 140)),
        43 => ("[MSH]", Color32::from_rgb(255, 190, 95)),
        48 => ("[SHD]", Color32::from_rgb(195, 145, 255)),
        49 => ("[TXT]", Color32::from_rgb(190, 205, 255)),
        128 => ("[FNT]", Color32::from_rgb(255, 155, 200)),
        114 | 115 => ("[SCR]", Color32::from_rgb(255, 225, 110)),
        21 => ("[MAT]", Color32::from_rgb(255, 130, 130)),
        74 => ("[ANM]", Color32::from_rgb(130, 240, 210)),
        89 => ("[CUB]", Color32::from_rgb(100, 185, 255)),
        137 => ("[SMR]", Color32::from_rgb(255, 175, 110)),
        142 => ("[AB ]", Color32::from_rgb(215, 185, 255)),
        687078895 => ("[ATL]", Color32::from_rgb(155, 215, 255)),
        1 => ("[GO ]", Color32::from_rgb(185, 185, 185)),
        4 | 224 => ("[TRN]", Color32::from_rgb(155, 165, 175)),
        _ => ("[OBJ]", Color32::from_rgb(160, 160, 160)),
    }
}

fn short_tail(text: &str, max_chars: usize) -> &str {
    let count = text.chars().count();
    if count <= max_chars {
        return text;
    }
    let skip = count.saturating_sub(max_chars);
    text.char_indices()
        .nth(skip)
        .map_or(text, |(idx, _)| &text[idx..])
}
