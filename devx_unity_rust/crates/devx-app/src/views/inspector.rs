//! Right dockable Object Inspector showing TypeTree fields, clickable `PPtr`s,
//! reverse backlinks, and MonoBehaviour/MonoScript script bindings.

use crate::state::DevXState;
use devx_core::AssetCatalog;
use devx_exporter::hierarchy::HierarchyExportFormat;
use devx_plugin_api::AssetRef;
use devx_unity_io::{UnityObject, UnityValue, extract_i32_field, extract_i64_field, read_object};
use egui::{Color32, RichText, ScrollArea};

enum InspectorAction {
    Navigate(AssetRef),
    ExportHierarchyFormat(AssetRef, HierarchyExportFormat),
    ExportPrefab(AssetRef, bool, bool),
    SaveRawBin(String, Vec<u8>),
    ReplaceRepack(AssetRef),
}

/// Renders the right Object Inspector pane.
pub fn show(ui: &mut egui::Ui, state: &mut DevXState) {
    let (Some(catalog), Some(asset_ref)) = (state.catalog.as_ref().cloned(), state.selected) else {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.weak("Select an asset in the Explorer to inspect Hierarchy, PPtr Links, and TypeTree fields.");
        });
        return;
    };

    let Some(entry) = catalog.entry(asset_ref).cloned() else {
        ui.weak("Selected asset is no longer present in catalog.");
        return;
    };

    ui.horizontal(|ui| {
        ui.label(RichText::new(&entry.summary.name).strong());
        ui.weak(format!(
            "[{} / {}]",
            entry.summary.class_name, entry.summary.class_id
        ));
    });
    ui.small(format!(
        "File #{} | PathID: {} | {} bytes",
        asset_ref.file_index, asset_ref.path_id, entry.summary.byte_size
    ));
    if let Some(ref c) = entry.summary.container_path {
        ui.small(format!("Container: {c}"));
    }

    let mut action: Option<InspectorAction> = None;

    ui.horizontal_wrapped(|ui| {
        if matches!(
            entry.summary.class_id,
            1 | 4 | 23 | 33 | 43 | 137 | 142 | 224
        ) {
            for (fmt, label) in [
                (HierarchyExportFormat::Fbx, "Hierarchy .fbx"),
                (HierarchyExportFormat::Glb, "Hierarchy .glb"),
                (HierarchyExportFormat::Gltf, ".gltf"),
                (HierarchyExportFormat::Dae, ".dae"),
                (HierarchyExportFormat::ObjMtl, ".obj+.mtl"),
            ] {
                if ui.small_button(label).clicked() {
                    action = Some(InspectorAction::ExportHierarchyFormat(asset_ref, fmt));
                }
            }
            for (is_scene, as_pkg, label) in [
                (false, false, ".prefab"),
                (true, false, ".unity"),
                (false, true, ".unitypackage"),
            ] {
                if ui.small_button(label).clicked() {
                    action = Some(InspectorAction::ExportPrefab(asset_ref, is_scene, as_pkg));
                }
            }
        }
        if let Some(ref raw) = state.selected_raw_bytes
            && ui.small_button("Dump Raw .bin...").clicked()
        {
            action = Some(InspectorAction::SaveRawBin(
                format!("{}_{}.bin", entry.summary.class_name, asset_ref.path_id),
                raw.clone(),
            ));
        }
        if ui
            .small_button("Replace & Repack .ab...")
            .on_hover_text("Replace this object's raw SerializedFile payload and repack as a UnityFS bundle")
            .clicked()
        {
            action = Some(InspectorAction::ReplaceRepack(asset_ref));
        }
    });

    ui.separator();

    let mut navigate_target: Option<AssetRef> = None;

    ScrollArea::vertical()
        .id_salt("inspector_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // 0. Scene / GameObject Hierarchy & Components quick navigation
            if let Some(obj) = state.selected_object.as_ref()
                && matches!(
                    entry.summary.class_id,
                    1 | 4 | 23 | 33 | 137 | 224 | 114 | 95 | 111
                )
            {
                render_hierarchy_links(
                    ui,
                    &catalog,
                    asset_ref,
                    entry.summary.class_id,
                    obj,
                    &mut navigate_target,
                );
            }

            // 1. Script binding info for MonoBehaviour / MonoScript
            if let (Some(scripts), Some(obj)) = (state.scripts.as_ref(), state.selected_object.as_ref())
            {
                if entry.summary.class_id == 114
                    && let Some(script_info) =
                        scripts.resolve_behaviour_script(&catalog.workspace, asset_ref.file_index, obj)
                {
                    egui::CollapsingHeader::new(RichText::new("MonoBehaviour Script").strong())
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.label(format!("Class: {}", script_info.full_name()));
                            ui.label(format!("Assembly: {}", script_info.assembly_name));
                            ui.label(format!("Execution Order: {}", script_info.execution_order));
                            let script_ref = AssetRef {
                                file_index: script_info.file_index,
                                path_id: script_info.path_id,
                            };
                            if ui
                                .link(format!(
                                    "Jump to MonoScript #{}:{}",
                                    script_ref.file_index, script_ref.path_id
                                ))
                                .clicked()
                            {
                                navigate_target = Some(script_ref);
                            }
                            if let Some(td) = scripts.resolve_il2cpp_type(script_info) {
                                ui.weak(format!(
                                    "IL2CPP: {} : {} ({} fields, {} methods)",
                                    td.kind,
                                    if td.parent.is_empty() { "object" } else { &td.parent },
                                    td.fields.len(),
                                    td.method_count
                                ));
                                egui::CollapsingHeader::new("Recovered Fields & C# Stub")
                                    .default_open(false)
                                    .show(ui, |ui| {
                                        for f in td.fields.iter().take(48) {
                                            ui.monospace(format!(
                                                "0x{:02X} {} {}",
                                                f.offset.max(0),
                                                f.type_name,
                                                f.name
                                            ));
                                        }
                                        let mut stub = scripts.generate_csharp_stub_for_script(script_info);
                                        if ui.small_button("Open in Scripts Tab").clicked() {
                                            state.selected_il2cpp_image_idx = Some(td.image_index);
                                            state.selected_il2cpp_type_idx = Some(td.type_def_index);
                                            state.selected_assembly_idx = None;
                                            state.bottom_tab = crate::state::BottomTab::Scripts;
                                        }
                                        ui.add(
                                            egui::TextEdit::multiline(&mut stub)
                                                .code_editor()
                                                .desired_width(f32::INFINITY)
                                                .interactive(false),
                                        );
                                    });
                            }
                        });
                } else if entry.summary.class_id == 115
                    && let Some(idx) = scripts
                        .mono_script_lookup
                        .get(&(asset_ref.file_index, asset_ref.path_id))
                    && let Some(script_info) = scripts.mono_scripts.get(*idx)
                {
                    egui::CollapsingHeader::new(RichText::new("MonoScript Metadata").strong())
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.label(format!("Full Type: {}", script_info.full_name()));
                            ui.label(format!("Assembly: {}", script_info.assembly_name));
                            ui.label(format!("Execution Order: {}", script_info.execution_order));
                            if let Some(td) = scripts.resolve_il2cpp_type(script_info) {
                                let mut stub = scripts.generate_csharp_stub_for_script(script_info);
                                ui.weak(format!(
                                    "IL2CPP TypeDef #{} ({} fields, {} methods)",
                                    td.type_def_index,
                                    td.fields.len(),
                                    td.method_count
                                ));
                                ui.add(
                                    egui::TextEdit::multiline(&mut stub)
                                        .code_editor()
                                        .desired_width(f32::INFINITY)
                                        .interactive(false),
                                );
                            }
                        });
                }
            }

            // 2. Forward & Reverse PPtr References
            if let Some(graph) = state.graph.as_ref() {
                let fwd = graph
                    .forward
                    .get(&asset_ref)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let rev = graph
                    .reverse
                    .get(&asset_ref)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);

                egui::CollapsingHeader::new(format!("Uses — Outgoing ({})", fwd.len()))
                    .default_open(!fwd.is_empty())
                    .show(ui, |ui| {
                        if fwd.is_empty() {
                            ui.weak("No outgoing PPtr references.");
                        }
                        for edge in fwd.iter().take(200) {
                            let target_label = describe_target(&catalog, edge.to);
                            if ui
                                .link(format!("{} -> {target_label}", edge.field_path))
                                .clicked()
                            {
                                navigate_target = Some(edge.to);
                            }
                        }
                    });

                egui::CollapsingHeader::new(format!("Used By — Backlinks ({})", rev.len()))
                    .default_open(!rev.is_empty())
                    .show(ui, |ui| {
                        if rev.is_empty() {
                            ui.weak("No reverse backlinks indexed.");
                        }
                        for edge in rev.iter().take(200) {
                            let from_label = describe_target(&catalog, edge.from);
                            if ui
                                .link(format!("{from_label} ({})", edge.field_path))
                                .clicked()
                            {
                                navigate_target = Some(edge.from);
                            }
                        }
                    });
            } else {
                ui.weak("Building cross-asset PPtr reference graph...");
            }

            ui.separator();

            // 3. TypeTree property tree
            if let Some(obj) = state.selected_object.as_ref() {
                egui::CollapsingHeader::new(RichText::new("TypeTree Properties").strong())
                    .default_open(true)
                    .show(ui, |ui| {
                        let mut depth_budget = 64usize;
                        for (k, v) in obj.class.properties() {
                            render_unity_value(
                                ui,
                                k,
                                v,
                                asset_ref.file_index,
                                &catalog,
                                &mut navigate_target,
                                &mut depth_budget,
                            );
                        }
                    });
            }
        });

    if let Some(target) = navigate_target {
        action = Some(InspectorAction::Navigate(target));
    }
    match action {
        Some(InspectorAction::Navigate(target)) => {
            let ctx = ui.ctx().clone();
            state.select_asset(target, true, Some(&ctx));
        }
        Some(InspectorAction::ExportHierarchyFormat(root, fmt)) => {
            state.export_hierarchy_interactive(root, fmt);
        }
        Some(InspectorAction::ExportPrefab(root, is_scene, as_pkg)) => {
            state.export_prefab_interactive(root, is_scene, as_pkg);
        }
        Some(InspectorAction::SaveRawBin(name, bytes)) => {
            state.save_bytes_interactive(&name, &bytes);
        }
        Some(InspectorAction::ReplaceRepack(target)) => {
            state.replace_object_payload_interactive(target);
        }
        None => {}
    }
}

fn resolve_pptr_ref(
    catalog: &AssetCatalog,
    src_file: usize,
    val: Option<&UnityValue>,
) -> Option<AssetRef> {
    let map = val?.as_object()?;
    let fid = extract_i32_field(map.get("m_FileID")?)?;
    let pid = extract_i64_field(map.get("m_PathID")?)?;
    if pid == 0 {
        return None;
    }
    let tf = catalog.workspace.resolve_external_file(src_file, fid)?;
    Some(AssetRef {
        file_index: tf,
        path_id: pid,
    })
}

fn render_hierarchy_links(
    ui: &mut egui::Ui,
    catalog: &AssetCatalog,
    asset_ref: AssetRef,
    class_id: i32,
    obj: &UnityObject,
    navigate_target: &mut Option<AssetRef>,
) {
    let props = obj.class.properties();
    egui::CollapsingHeader::new(RichText::new("Hierarchy & Components").strong())
        .default_open(true)
        .show(ui, |ui| {
            // Component -> owning GameObject
            let mut go_ref = if class_id == 1 {
                Some(asset_ref)
            } else {
                resolve_pptr_ref(catalog, asset_ref.file_index, props.get("m_GameObject"))
            };
            if class_id != 1
                && let Some(go) = go_ref
                && ui
                    .link(format!("Owner GameObject: {}", describe_target(catalog, go)))
                    .clicked()
            {
                *navigate_target = Some(go);
            }

            // Locate Transform to show Parent & Children
            let mut tr_ref = (class_id == 4 || class_id == 224).then_some(asset_ref);
            let mut comp_refs: Vec<AssetRef> = Vec::new();
            if let Some(go) = go_ref
                && let Some((gl, gi)) = catalog.workspace.object_info(go.file_index, go.path_id)
                && let Ok(go_obj) = read_object(gl, gi)
                && let Some(comps) = go_obj
                    .class
                    .properties()
                    .get("m_Component")
                    .and_then(|v| v.as_array())
            {
                for item in comps {
                    let pptr = item.as_object().and_then(|m| m.get("component")).or(Some(item));
                    if let Some(cr) = resolve_pptr_ref(catalog, go.file_index, pptr) {
                        comp_refs.push(cr);
                        if tr_ref.is_none()
                            && let Some(e) = catalog.entry(cr)
                            && (e.summary.class_id == 4 || e.summary.class_id == 224)
                        {
                            tr_ref = Some(cr);
                        }
                    }
                }
            }
            let _ = &mut go_ref;

            if let Some(tr) = tr_ref
                && let Some((tl, ti)) = catalog.workspace.object_info(tr.file_index, tr.path_id)
                && let Ok(tr_obj) = read_object(tl, ti)
            {
                let tprops = tr_obj.class.properties();
                if let Some(parent_tr) =
                    resolve_pptr_ref(catalog, tr.file_index, tprops.get("m_Father"))
                    && ui
                        .link(format!("^ Parent: {}", describe_target(catalog, parent_tr)))
                        .clicked()
                {
                    *navigate_target = Some(parent_tr);
                }
                if let Some(children) = tprops.get("m_Children").and_then(|v| v.as_array())
                    && !children.is_empty()
                {
                    ui.small(format!("Children ({}):", children.len()));
                    for child in children.iter().take(48) {
                        if let Some(cr) = resolve_pptr_ref(catalog, tr.file_index, Some(child))
                            && ui
                                .link(format!("  -> {}", describe_target(catalog, cr)))
                                .clicked()
                        {
                            *navigate_target = Some(cr);
                        }
                    }
                }
            }

            if !comp_refs.is_empty() {
                ui.small(format!("Components ({}):", comp_refs.len()));
                for cr in comp_refs {
                    let label = describe_target(catalog, cr);
                    if cr == asset_ref {
                        ui.weak(format!("  • {label} (current)"));
                    } else if ui.link(format!("  • {label}")).clicked() {
                        *navigate_target = Some(cr);
                    }
                }
            }
        });
}

fn describe_target(catalog: &AssetCatalog, target: AssetRef) -> String {
    if target.path_id == 0 {
        return "Null (0)".to_owned();
    }
    if let Some(e) = catalog.entry(target) {
        format!(
            "{} [{}] ({}:{})",
            e.summary.name, e.summary.class_name, target.file_index, target.path_id
        )
    } else {
        format!("External ({}:{})", target.file_index, target.path_id)
    }
}

fn render_unity_value(
    ui: &mut egui::Ui,
    key: &str,
    value: &UnityValue,
    source_file_index: usize,
    catalog: &AssetCatalog,
    navigate_target: &mut Option<AssetRef>,
    depth_budget: &mut usize,
) {
    if *depth_budget == 0 {
        ui.weak(format!("{key}: <max depth>"));
        return;
    }

    match value {
        UnityValue::Object(map) => {
            if let (Some(fid_v), Some(pid_v)) = (map.get("m_FileID"), map.get("m_PathID"))
                && let (Some(fid), Some(pid)) =
                    (extract_i32_field(fid_v), extract_i64_field(pid_v))
            {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(key).strong());
                    if pid == 0 {
                        ui.weak("-> Null PPtr");
                    } else if let Some(target_file) =
                        catalog.workspace.resolve_external_file(source_file_index, fid)
                    {
                        let target = AssetRef {
                            file_index: target_file,
                            path_id: pid,
                        };
                        let label = describe_target(catalog, target);
                        if ui.link(label).clicked() {
                            *navigate_target = Some(target);
                        }
                    } else {
                        ui.colored_label(
                            Color32::YELLOW,
                            format!("External fileId={fid}, pathId={pid}"),
                        );
                    }
                });
                return;
            }

            let open_by_default = map.len() <= 10 && *depth_budget > 58;
            egui::CollapsingHeader::new(format!("{key} ({{{} fields}})", map.len()))
                .id_salt(format!("prop_{key}_{}", map.len()))
                .default_open(open_by_default)
                .show(ui, |ui| {
                    *depth_budget -= 1;
                    for (sub_k, sub_v) in map.iter().take(256) {
                        render_unity_value(
                            ui,
                            sub_k,
                            sub_v,
                            source_file_index,
                            catalog,
                            navigate_target,
                            depth_budget,
                        );
                    }
                    if map.len() > 256 {
                        ui.weak(format!("... {} more fields truncated", map.len() - 256));
                    }
                    *depth_budget += 1;
                });
        }
        UnityValue::Array(items) => {
            if items.len() > 64
                && items
                    .first()
                    .is_some_and(|v| matches!(v, UnityValue::Integer(_) | UnityValue::Float(_)))
            {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(key).strong());
                    ui.weak(format!("<numeric array: {} elements>", items.len()));
                });
                return;
            }
            egui::CollapsingHeader::new(format!("{key} ([{}])", items.len()))
                .id_salt(format!("arr_{key}_{}", items.len()))
                .default_open(items.len() <= 6)
                .show(ui, |ui| {
                    *depth_budget -= 1;
                    for (idx, item) in items.iter().take(128).enumerate() {
                        render_unity_value(
                            ui,
                            &format!("[{idx}]"),
                            item,
                            source_file_index,
                            catalog,
                            navigate_target,
                            depth_budget,
                        );
                    }
                    if items.len() > 128 {
                        ui.weak(format!("... {} more items", items.len() - 128));
                    }
                    *depth_budget += 1;
                });
        }
        UnityValue::Bytes(bytes) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(key).strong());
                ui.weak(format!("<{} raw bytes>", bytes.len()));
            });
        }
        UnityValue::String(s) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(key).strong());
                if s.len() > 160 {
                    ui.monospace(format!("{}... ({} chars)", &s[..160], s.len()));
                } else {
                    ui.monospace(format!("\"{s}\""));
                }
            });
        }
        other => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(key).strong());
                ui.monospace(format!("{other:?}"));
            });
        }
    }
}
