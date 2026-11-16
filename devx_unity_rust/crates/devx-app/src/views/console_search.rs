//! Bottom workbench dock panel: Deep Search (metadata + HEX), Batch Export & Pack,
//! Scripts & IL2CPP browser, and Plugins & Console logs.

use crate::state::{
    BatchScope, BottomTab, DevXState, Il2CppSourceCache, Il2CppSourceCacheKey, Il2CppViewTab,
};
use devx_plugin_api::{AssetRef, LogLevel};
use devx_scripts::il2cpp::{Il2CppDumpReport, parse_rva_hex};
use egui::{Color32, RichText, ScrollArea};

/// Renders the bottom workbench panel.
pub fn show(ui: &mut egui::Ui, state: &mut DevXState) {
    ui.horizontal(|ui| {
        for tab in BottomTab::ALL {
            ui.selectable_value(&mut state.bottom_tab, tab, tab.label());
        }
    });
    ui.separator();

    match state.bottom_tab {
        BottomTab::DeepSearch => show_deep_search(ui, state),
        BottomTab::BatchExport => show_batch_export(ui, state),
        BottomTab::Scripts => show_scripts_tab(ui, state),
        BottomTab::PluginsConsole => show_plugins_console(ui, state),
    }
}

fn show_deep_search(ui: &mut egui::Ui, state: &mut DevXState) {
    let mut trigger_search = false;
    ui.horizontal(|ui| {
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.search_query)
                .hint_text("Search name, PathID, UTF-8 text, or hex pattern (e.g. 4F 67 67 53)...")
                .desired_width(340.0),
        );
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            trigger_search = true;
        }
        ui.checkbox(&mut state.search_raw_bytes, "Scan Raw Bytes / HEX");
        if ui.button("Search").clicked() {
            trigger_search = true;
        }
        if !state.search_hits.is_empty() && ui.button("Clear").clicked() {
            state.search_hits.clear();
        }
        ui.weak(format!("{} hit(s)", state.search_hits.len()));
    });

    if trigger_search {
        state.spawn_deep_search();
    }

    ui.separator();

    let mut select_req: Option<AssetRef> = None;
    let total = state.search_hits.len();
    ScrollArea::vertical()
        .id_salt("deep_search_results")
        .auto_shrink([false, false])
        .show_rows(ui, 20.0, total, |ui, range| {
            for idx in range {
                let hit = &state.search_hits[idx];
                ui.horizontal(|ui| {
                    if ui.link(&hit.name).clicked() {
                        select_req = Some(hit.asset_ref);
                    }
                    ui.weak(format!(
                        "[{}] #{}:{} — {}",
                        hit.class_name, hit.asset_ref.file_index, hit.asset_ref.path_id, hit.reason
                    ));
                });
            }
        });

    if let Some(asset_ref) = select_req {
        let ctx = ui.ctx().clone();
        state.select_asset(asset_ref, true, Some(&ctx));
    }
}

fn show_batch_export(ui: &mut egui::Ui, state: &mut DevXState) {
    ui.horizontal_wrapped(|ui| {
        ui.strong("Scope:");
        ui.selectable_value(&mut state.batch_scope, BatchScope::AllAssets, "All Assets");
        ui.selectable_value(
            &mut state.batch_scope,
            BatchScope::FilteredTree,
            "Filtered Tree",
        );
        ui.selectable_value(
            &mut state.batch_scope,
            BatchScope::SelectedClass,
            "Selected Class",
        );
    });

    ui.horizontal_wrapped(|ui| {
        ui.label("Textures:");
        egui::ComboBox::from_id_salt("batch_tex_fmt")
            .selected_text(state.export_opts.texture_exporter_id.clone())
            .show_ui(ui, |ui| {
                for (id, label) in [
                    ("builtin.png", "PNG (.png)"),
                    ("builtin.jpg", "JPEG (.jpg)"),
                    ("builtin.webp", "WebP (.webp)"),
                    ("builtin.bmp", "BMP (.bmp)"),
                    ("builtin.tga", "TGA (.tga)"),
                    ("builtin.dds", "DDS (.dds)"),
                ] {
                    ui.selectable_value(&mut state.export_opts.texture_exporter_id, id.to_owned(), label);
                }
            });

        ui.label("Audio:");
        egui::ComboBox::from_id_salt("batch_aud_fmt")
            .selected_text(state.export_opts.audio_exporter_id.clone())
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut state.export_opts.audio_exporter_id,
                    "builtin.wav".to_owned(),
                    "WAV PCM (.wav)",
                );
                ui.selectable_value(
                    &mut state.export_opts.audio_exporter_id,
                    "builtin.audio_native".to_owned(),
                    "Original Stream",
                );
            });

        ui.label("Meshes:");
        egui::ComboBox::from_id_salt("batch_msh_fmt")
            .selected_text(state.export_opts.mesh_exporter_id.clone())
            .show_ui(ui, |ui| {
                for (id, label) in [
                    ("builtin.obj", "Wavefront OBJ (.obj)"),
                    ("builtin.fbx", "Binary FBX (.fbx)"),
                    ("builtin.glb", "glTF 2.0 (.glb)"),
                    ("builtin.dae", "COLLADA (.dae)"),
                    ("builtin.ply", "Stanford PLY (.ply)"),
                    ("builtin.stl", "Binary STL (.stl)"),
                ] {
                    ui.selectable_value(&mut state.export_opts.mesh_exporter_id, id.to_owned(), label);
                }
            });

        ui.checkbox(
            &mut state.export_opts.export_yaml_for_objects,
            "Export Unity YAML",
        );
        ui.checkbox(
            &mut state.export_opts.preserve_container_paths,
            "Preserve Container Paths",
        );
    });

    ui.separator();

    ui.horizontal_wrapped(|ui| {
        let enabled = state.catalog.is_some();
        ui.add_enabled_ui(enabled, |ui| {
            if ui.button("Export to Directory...").clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                state.spawn_batch_export(dir);
            }
            if ui.button("Export All Hierarchy GLBs...").clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                state.spawn_export_all_hierarchies(dir, None);
            }
            if ui.button("Export .unitypackage...").clicked()
                && let Some(file) = rfd::FileDialog::new()
                    .set_file_name("exported_assets.unitypackage")
                    .save_file()
            {
                state.spawn_unitypackage_export(file);
            }
            if ui.button("Export Unity Project...").clicked()
                && let Some(dir) = rfd::FileDialog::new()
                    .set_title("Choose Output Directory for Unity Editor Project")
                    .pick_folder()
            {
                state.spawn_export_unity_project(dir);
            }
            if ui.button("Repack UnityFS Bundle (.unity3d)...").clicked()
                && let Some(file) = rfd::FileDialog::new()
                    .set_file_name("repacked.unity3d")
                    .save_file()
            {
                state.spawn_unityfs_repack(file);
            }
        });
    });

    ui.separator();

    egui::CollapsingHeader::new(RichText::new("Bevy 0.20 Zero-Logic Game Exporter").strong())
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Project:");
                ui.add(
                    egui::TextEdit::singleline(&mut state.bevy_export_ui.project_name)
                        .desired_width(130.0),
                );
                ui.label("Scenes (csv):");
                ui.add(
                    egui::TextEdit::singleline(&mut state.bevy_export_ui.scene_filters)
                        .hint_text("auto or building_mimafang,a-34")
                        .desired_width(150.0),
                );
                ui.label("Chars (csv):");
                ui.add(
                    egui::TextEdit::singleline(&mut state.bevy_export_ui.char_filters)
                        .hint_text("auto or monster_xlt_01")
                        .desired_width(150.0),
                );
                ui.label("Props (csv):");
                ui.add(
                    egui::TextEdit::singleline(&mut state.bevy_export_ui.prop_filters)
                        .hint_text("auto or ak47,qbu97")
                        .desired_width(140.0),
                );
                ui.label("UI Panels (csv):");
                ui.add(
                    egui::TextEdit::singleline(&mut state.bevy_export_ui.ui_panel_filters)
                        .hint_text("auto or loadingpanel,settingpanel")
                        .desired_width(160.0),
                );
            });
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::DragValue::new(&mut state.bevy_export_ui.max_scenes).range(1..=64).prefix("Max Scenes: "));
                ui.add(egui::DragValue::new(&mut state.bevy_export_ui.max_characters).range(1..=64).prefix("Max Chars: "));
                ui.add(egui::DragValue::new(&mut state.bevy_export_ui.max_props).range(1..=64).prefix("Max Props: "));
                ui.add(egui::DragValue::new(&mut state.bevy_export_ui.max_audio).range(0..=256).prefix("Max Audio: "));
                ui.add(egui::DragValue::new(&mut state.bevy_export_ui.max_ui_sprites).range(0..=256).prefix("Max UI Spr: "));
                ui.add(egui::DragValue::new(&mut state.bevy_export_ui.max_ui_panels).range(0..=64).prefix("Max UI Panels: "));
                ui.separator();
                let enabled = state.catalog.is_some();
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::new(
                            RichText::new("🚀 Generate Bevy 0.20 Project...").strong(),
                        ),
                    )
                    .clicked()
                    && let Some(dir) = rfd::FileDialog::new()
                        .set_title("Choose Output Directory for Bevy 0.20 Project")
                        .pick_folder()
                {
                    state.spawn_export_bevy_project(dir);
                }
            });
        });

    if let Some(ref rep) = state.last_report {
        ui.add_space(6.0);
        ui.colored_label(Color32::from_rgb(120, 220, 150), rep);
    }
}

fn disasm_toggle(ui: &mut egui::Ui, enabled: bool, selected: &mut bool, label: &str) -> bool {
    let resp = ui.add_enabled(enabled, egui::Button::selectable(*selected, label));
    if resp.clicked() {
        *selected = !*selected;
        return true;
    }
    false
}

fn weighted_columns(ui: &mut egui::Ui, weights: &[f32], add_contents: impl FnOnce(&mut [egui::Ui])) {
    if weights.is_empty() {
        return;
    }
    let spacing = ui.spacing().item_spacing.x;
    let total_spacing = spacing * (weights.len().saturating_sub(1) as f32);
    let avail_w = (ui.available_width() - total_spacing).max(weights.len() as f32 * 40.0);
    let weight_sum: f32 = weights.iter().copied().sum::<f32>().max(0.001);
    let top_left = ui.cursor().min;

    let mut x = top_left.x;
    let mut columns: Vec<egui::Ui> = weights
        .iter()
        .map(|&w| {
            let col_w = avail_w * (w / weight_sum);
            let child_rect = egui::Rect::from_min_max(
                egui::pos2(x, top_left.y),
                egui::pos2(x + col_w, ui.max_rect().right_bottom().y),
            );
            x += col_w + spacing;
            let mut col_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(child_rect)
                    .layout(egui::Layout::top_down_justified(egui::Align::LEFT)),
            );
            col_ui.set_width(col_w);
            col_ui
        })
        .collect();

    add_contents(&mut columns);

    let max_h = columns
        .iter()
        .map(|c| c.min_size().y)
        .fold(0.0_f32, f32::max);
    let total_w = total_spacing + avail_w;
    ui.advance_cursor_after_rect(egui::Rect::from_min_size(
        top_left,
        egui::vec2(ui.available_width().max(total_w), max_h),
    ));
}

fn show_scripts_images_column(
    ui: &mut egui::Ui,
    state: &mut DevXState,
    scripts: &devx_scripts::ScriptsIndex,
) {
    ui.label(RichText::new("Assemblies & Script Binaries").strong());
    ScrollArea::vertical()
        .id_salt("scripts_left_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (idx, asm) in scripts.assemblies.iter().enumerate() {
                let is_sel = state.selected_assembly_idx == Some(idx);
                if ui
                    .selectable_label(
                        is_sel,
                        format!(
                            "{} v{} ({} types, {})",
                            asm.assembly_name,
                            asm.assembly_version,
                            asm.type_defs.len(),
                            asm.runtime_version
                        ),
                    )
                    .clicked()
                {
                    state.selected_assembly_idx = Some(idx);
                    state.selected_il2cpp_image_idx = None;
                    state.selected_il2cpp_type_idx = None;
                    state.il2cpp_selected_method_idx = None;
                }
            }
            if let Some(ref rec) = scripts.il2cpp_recovered {
                if !scripts.assemblies.is_empty() {
                    ui.separator();
                }
                ui.weak(format!("Recovered IL2CPP Images ({})", rec.images.len()));
                let all_sel =
                    state.selected_il2cpp_image_idx.is_none() && state.selected_assembly_idx.is_none();
                if ui
                    .selectable_label(all_sel, format!("All IL2CPP Types ({})", rec.types.len()))
                    .clicked()
                {
                    state.selected_il2cpp_image_idx = None;
                    state.selected_assembly_idx = None;
                }
                for img in &rec.images {
                    let is_sel = state.selected_il2cpp_image_idx == Some(img.image_index);
                    if ui
                        .selectable_label(
                            is_sel,
                            format!("{} ({} types)", img.assembly_name, img.type_count),
                        )
                        .clicked()
                    {
                        state.selected_il2cpp_image_idx = Some(img.image_index);
                        state.selected_assembly_idx = None;
                        state.selected_il2cpp_type_idx = None;
                        state.il2cpp_selected_method_idx = None;
                    }
                }
            }
            ui.separator();
            for nat in &scripts.native_binaries {
                ui.weak(format!(
                    "[{}] {} ({} bytes)",
                    nat.kind, nat.virtual_path, nat.byte_len
                ));
            }
        });
}

fn show_scripts_tab(ui: &mut egui::Ui, state: &mut DevXState) {
    let Some(scripts) = state.scripts.as_ref().cloned() else {
        ui.weak("No scripts index loaded yet.");
        return;
    };

    ui.horizontal_wrapped(|ui| {
        ui.label(format!(
            "MonoScripts: {} | Managed DLLs: {} | Native/WASM: {}",
            scripts.mono_scripts.len(),
            scripts.assemblies.len(),
            scripts.native_binaries.len()
        ));
        if let Some(ref rec) = scripts.il2cpp_recovered {
            ui.colored_label(
                Color32::from_rgb(130, 225, 165),
                format!(
                    "| IL2CPP v{} ({}, {}): {} assemblies, {} types, {} methods",
                    rec.metadata_version,
                    rec.binary_format,
                    rec.arch,
                    rec.images.len(),
                    rec.types.len(),
                    rec.total_method_count
                ),
            );
            if ui.button("Export IL2CPP Dump...").clicked() {
                state.export_il2cpp_dump_interactive();
            }
        } else if let Some(ref il2) = scripts.il2cpp {
            ui.weak(format!(
                "| IL2CPP v{} ({} literals)",
                il2.format_version, il2.string_literal_count
            ));
        }
        if let Some(ref err) = scripts.il2cpp_error {
            ui.colored_label(Color32::YELLOW, format!("IL2CPP init warning: {err}"));
        }
    });

    if let Some(ref rec) = scripts.il2cpp_recovered {
        let mut disasm_opts_changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled(
                rec.arch_has_native_code,
                egui::Checkbox::new(&mut state.il2cpp_include_disasm, "Disasm"),
            )
            .on_disabled_hover_text(format!(
                "{} does not expose native machine instructions",
                rec.arch
            ));
            ui.checkbox(&mut state.il2cpp_include_pseudo_c, "Pseudo-C");
            ui.checkbox(&mut state.il2cpp_include_dummy_dlls, "DummyDlls");
            ui.separator();
            let can_disasm = rec.arch_has_native_code;
            disasm_opts_changed |= disasm_toggle(
                ui,
                can_disasm,
                &mut state.il2cpp_disasm_opts.hex_bytes,
                "Hex",
            );
            disasm_opts_changed |= disasm_toggle(
                ui,
                can_disasm,
                &mut state.il2cpp_disasm_opts.cfg,
                "CFG",
            );
            disasm_opts_changed |= disasm_toggle(
                ui,
                can_disasm,
                &mut state.il2cpp_disasm_opts.annotations,
                "Ann",
            );
            disasm_opts_changed |= disasm_toggle(
                ui,
                can_disasm,
                &mut state.il2cpp_disasm_opts.field_names,
                "Fields",
            );
            disasm_opts_changed |= ui
                .add_enabled(
                    can_disasm,
                    egui::DragValue::new(&mut state.il2cpp_disasm_opts.max_instructions)
                        .range(16..=4096)
                        .speed(8)
                        .prefix("Max "),
                )
                .changed();
        });
        if disasm_opts_changed
            && let Some(Ok(prev)) = state.il2cpp_disasm_result.as_ref()
        {
            let prev_rva = prev.rva;
            let type_hint = state.selected_il2cpp_type_idx;
            let method_hint = state.il2cpp_selected_method_idx;
            state.disassemble_il2cpp_rva(rec, prev_rva, type_hint, method_hint);
        }
    }

    ui.separator();

    if let Some(ref rec) = scripts.il2cpp_recovered
        && state.selected_assembly_idx.is_none()
    {
        weighted_columns(ui, &[0.24, 0.28, 0.48], |cols| {
            show_scripts_images_column(&mut cols[0], state, &scripts);

            cols[1].horizontal(|ui| {
                ui.label(RichText::new("Types").strong());
                let filter_w = (ui.available_width() - 28.0).clamp(80.0, 220.0);
                ui.add(
                    egui::TextEdit::singleline(&mut state.script_filter)
                        .hint_text("filter class (use | for OR)...")
                        .desired_width(filter_w),
                );
                if !state.script_filter.is_empty() && ui.small_button("×").clicked() {
                    state.script_filter.clear();
                }
            });
            let q = state.script_filter.trim().to_ascii_lowercase();
            let q_alts: Vec<&str> = q
                .split('|')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            let mut matched_types: Vec<_> = rec
                .types
                .iter()
                .filter(|td| {
                    state
                        .selected_il2cpp_image_idx
                        .is_none_or(|img_i| td.image_index == img_i)
                })
                .filter(|td| {
                    if q_alts.is_empty() {
                        return true;
                    }
                    let full_low = td.full_name.to_ascii_lowercase();
                    let asm_low = td.assembly_name.to_ascii_lowercase();
                    q_alts
                        .iter()
                        .any(|alt| full_low.contains(alt) || asm_low.contains(alt))
                })
                .collect();
            let total_matched = matched_types.len();
            let cap_id = cols[1].make_persistent_id("il2cpp_type_list_cap");
            let mut cap: usize = cols[1].data(|d| d.get_temp(cap_id)).unwrap_or(500);
            if cap < 500 {
                cap = 500;
            }
            let shown_count = total_matched.min(cap);
            cols[1].horizontal(|ui| {
                ui.weak(format!("Showing {shown_count} of {total_matched}"));
                if total_matched > shown_count && ui.small_button("+500").clicked() {
                    cap = (cap + 500).min(total_matched);
                    ui.data_mut(|d| d.insert_temp(cap_id, cap));
                }
                if total_matched > shown_count && ui.small_button("Show all").clicked() {
                    ui.data_mut(|d| d.insert_temp(cap_id, total_matched.max(500)));
                }
            });
            matched_types.truncate(cap);
            ScrollArea::vertical()
                .id_salt("il2cpp_type_list_scroll")
                .auto_shrink([false, false])
                .show(&mut cols[1], |ui| {
                    for td in matched_types {
                        let is_sel = state.selected_il2cpp_type_idx == Some(td.type_def_index);
                        let lbl = format!(
                            "{} {} ({}f, {}m)",
                            td.kind,
                            td.full_name,
                            td.fields.len(),
                            td.method_count
                        );
                        if ui.selectable_label(is_sel, lbl).clicked() {
                            state.selected_il2cpp_type_idx = Some(td.type_def_index);
                            state.il2cpp_selected_method_idx = None;
                        }
                    }
                });

            cols[2].horizontal_wrapped(|ui| {
                ui.selectable_value(
                    &mut state.il2cpp_view_tab,
                    Il2CppViewTab::TypeSource,
                    "Type Source",
                );
                ui.selectable_value(
                    &mut state.il2cpp_view_tab,
                    Il2CppViewTab::Disassembler,
                    "Disassembler",
                );
                if let Some(td_idx) = state.selected_il2cpp_type_idx {
                    ui.weak(format!("| TypeDef #{td_idx}"));
                }
            });
            cols[2].separator();

            match state.il2cpp_view_tab {
                Il2CppViewTab::TypeSource => {
                    show_il2cpp_type_source_pane(&mut cols[2], state, rec);
                }
                Il2CppViewTab::Disassembler => {
                    show_il2cpp_disassembler_pane(&mut cols[2], state, rec);
                }
            }
        });
        return;
    }

    weighted_columns(ui, &[0.32, 0.68], |cols| {
        show_scripts_images_column(&mut cols[0], state, &scripts);

        cols[1].horizontal(|ui| {
            ui.label(RichText::new("Types / IL2CPP Symbols").strong());
            let filter_w = (ui.available_width() - 28.0).clamp(80.0, 220.0);
            ui.add(
                egui::TextEdit::singleline(&mut state.script_filter)
                    .hint_text("filter class or string...")
                    .desired_width(filter_w),
            );
            if !state.script_filter.is_empty() && ui.small_button("×").clicked() {
                state.script_filter.clear();
            }
        });
        let q = state.script_filter.trim().to_ascii_lowercase();

        if let Some(idx) = state.selected_assembly_idx
            && let Some(asm) = scripts.assemblies.get(idx)
        {
            ScrollArea::vertical()
                .id_salt("scripts_right_scroll")
                .auto_shrink([false, false])
                .show(&mut cols[1], |ui| {
                    ui.weak(format!("{} TypeDefs ({})", asm.file_name, asm.virtual_path));
                    for td in asm
                        .type_defs
                        .iter()
                        .filter(|td| {
                            q.is_empty()
                                || td.name.to_ascii_lowercase().contains(&q)
                                || td.namespace.to_ascii_lowercase().contains(&q)
                        })
                        .take(500)
                    {
                        let vis = if td.is_public { "public" } else { "internal" };
                        ui.monospace(format!("{vis} {} {}.{}", td.kind, td.namespace, td.name));
                    }
                });
        } else {
            ScrollArea::vertical()
                .id_salt("scripts_right_scroll")
                .auto_shrink([false, false])
                .show(&mut cols[1], |ui| {
                    if let Some(ref il2) = scripts.il2cpp {
                        ui.weak("IL2CPP String Literals & Symbols Preview:");
                        for s in il2
                            .string_literals_preview
                            .iter()
                            .chain(il2.symbol_strings_preview.iter())
                            .filter(|s| q.is_empty() || s.to_ascii_lowercase().contains(&q))
                            .take(500)
                        {
                            ui.monospace(s);
                        }
                    } else {
                        for ms in scripts
                            .mono_scripts
                            .iter()
                            .filter(|m| {
                                q.is_empty()
                                    || m.full_name().to_ascii_lowercase().contains(&q)
                                    || m.assembly_name.to_ascii_lowercase().contains(&q)
                            })
                            .take(400)
                        {
                            ui.monospace(format!("{} ({})", ms.full_name(), ms.assembly_name));
                        }
                    }
                });
        }
    });
}

fn show_plugins_console(ui: &mut egui::Ui, state: &mut DevXState) {
    ui.columns(2, |cols| {
        cols[0].label(RichText::new("Registered Plugins & Exporters").strong());
        ScrollArea::vertical()
            .id_salt("plugins_list_scroll")
            .auto_shrink([false, false])
            .show(&mut cols[0], |ui| {
                for p in state.plugins.plugins() {
                    let m = p.metadata();
                    ui.label(format!("Plugin: {} v{} ({})", m.name, m.version, m.id));
                }
                ui.separator();
                for exp in state.plugins.exporters() {
                    ui.monospace(format!("Exporter: {} — {} (.{})", exp.id(), exp.label(), exp.extension()));
                }
            });

        cols[1].horizontal(|ui| {
            ui.label(RichText::new("Host Console Log").strong());
            if ui.small_button("Clear Log").clicked() {
                state.host.logs.clear();
            }
        });
        ScrollArea::vertical()
            .id_salt("host_console_scroll")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(&mut cols[1], |ui| {
                for entry in &state.host.logs {
                    let (tag, color) = match entry.level {
                        LogLevel::Info => ("INFO", Color32::LIGHT_GRAY),
                        LogLevel::Warn => ("WARN", Color32::YELLOW),
                        LogLevel::Error => ("ERR ", Color32::from_rgb(255, 110, 110)),
                    };
                    ui.colored_label(color, format!("[{tag}] {}", entry.message));
                }
            });
    });
}

fn save_text_with_dialog(
    state: &mut DevXState,
    default_name: &str,
    filter_name: &str,
    ext: &str,
    contents: &str,
    description: &str,
) {
    if let Some(path) = rfd::FileDialog::new()
        .set_file_name(default_name)
        .add_filter(filter_name, &[ext])
        .save_file()
        && std::fs::write(&path, contents).is_ok()
    {
        state.status_banner = format!("Saved {description} to {}", path.display());
    }
}

fn show_il2cpp_type_source_pane(
    ui: &mut egui::Ui,
    state: &mut DevXState,
    rec: &Il2CppDumpReport,
) {
    let Some(td_idx) = state.selected_il2cpp_type_idx else {
        ui.weak("Select a recovered IL2CPP type to inspect fields, offsets, and method RVAs.");
        return;
    };

    let key = Il2CppSourceCacheKey {
        type_def_index: td_idx,
        include_disassembly: state.il2cpp_include_disasm && rec.arch_has_native_code,
        include_pseudo_c: state.il2cpp_include_pseudo_c,
        disasm: state.il2cpp_disasm_opts,
    };
    let cache_hit = state
        .il2cpp_type_source_cache
        .as_ref()
        .is_some_and(|c| c.key == key);
    if !cache_hit {
        let rendered = rec
            .format_type_source(
                key.type_def_index,
                key.include_disassembly,
                key.include_pseudo_c,
                &key.disasm,
            )
            .unwrap_or_else(|e| format!("// Failed to format TypeDef #{td_idx}: {e}"));
        state.il2cpp_type_source_cache = Some(Il2CppSourceCache { key, text: rendered });
    }

    let Some(preview_src) = state.il2cpp_type_source_cache.as_ref().map(|c| c.text.clone()) else {
        return;
    };
    let mut display_src = preview_src.clone();

    ui.horizontal_wrapped(|ui| {
        if ui.small_button("Copy").clicked() {
            ui.ctx().copy_text(preview_src.clone());
        }
        if ui.small_button("Save .cs").clicked() {
            save_text_with_dialog(
                state,
                &format!("TypeDef_{td_idx}.cs"),
                "C# Source",
                "cs",
                &preview_src,
                "recovered IL2CPP source",
            );
        }
        if rec.arch_has_native_code && ui.small_button("Methods & Disasm ->").clicked() {
            state.il2cpp_view_tab = Il2CppViewTab::Disassembler;
        }
    });

    ScrollArea::vertical()
        .id_salt("il2cpp_type_source_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut display_src)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
        });
}

fn show_il2cpp_disassembler_pane(
    ui: &mut egui::Ui,
    state: &mut DevXState,
    rec: &Il2CppDumpReport,
) {
    if !rec.arch_has_native_code {
        ui.colored_label(
            Color32::YELLOW,
            format!(
                "{} ({}) contains bytecode rather than native machine instructions.",
                rec.binary_format, rec.arch
            ),
        );
        return;
    }

    let mut trigger_rva = false;
    ui.horizontal_wrapped(|ui| {
        ui.label("RVA:");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.il2cpp_rva_input)
                .hint_text("0x1A2B3C")
                .desired_width(130.0),
        );
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            trigger_rva = true;
        }
        if ui.small_button("Disassemble").clicked() {
            trigger_rva = true;
        }
    });

    if trigger_rva {
        match parse_rva_hex(&state.il2cpp_rva_input) {
            Ok(rva) => state.disassemble_il2cpp_rva(rec, rva, state.selected_il2cpp_type_idx, None),
            Err(e) => state.il2cpp_disasm_result = Some(Err(e.to_string())),
        }
    }

    if let Some(td_idx) = state.selected_il2cpp_type_idx {
        let methods_hit = state
            .il2cpp_type_methods_cache
            .as_ref()
            .is_some_and(|(cached_td, _)| *cached_td == td_idx);
        if !methods_hit {
            let list = rec.list_type_methods(td_idx).unwrap_or_default();
            state.il2cpp_type_methods_cache = Some((td_idx, list));
        }

        let mut clicked_method: Option<(u64, usize)> = None;
        if let Some((_, methods)) = state.il2cpp_type_methods_cache.as_ref() {
            ui.weak(format!("Methods on TypeDef #{td_idx} ({}):", methods.len()));
            let list_h = (ui.available_height() * 0.36).clamp(56.0, 140.0);
            ScrollArea::vertical()
                .id_salt("il2cpp_methods_list_scroll")
                .max_height(list_h)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for m in methods {
                        let thumb_tag = if m.thumb { " [T]" } else { "" };
                        let static_tag = if m.is_static { " [S]" } else { "" };
                        let label = format!(
                            "0x{:08X} ({:>4}B){}{} | {}",
                            m.rva, m.body_size, thumb_tag, static_tag, m.signature
                        );
                        if m.rva == 0 || m.is_abstract {
                            ui.add_enabled(false, egui::Button::new(&label).frame(false));
                        } else {
                            let is_sel =
                                state.il2cpp_selected_method_idx == Some(m.method_def_index);
                            if ui.selectable_label(is_sel, label).clicked() {
                                clicked_method = Some((m.rva, m.method_def_index));
                            }
                        }
                    }
                });
        }

        if let Some((rva, method_idx)) = clicked_method {
            state.disassemble_il2cpp_rva(rec, rva, Some(td_idx), Some(method_idx));
        }
        ui.separator();
    }

    let Some(result_clone) = state.il2cpp_disasm_result.clone() else {
        ui.weak(
            "Select a method above or enter an RVA (e.g. 0x1A2B3C) to view CFG & annotated disassembly.",
        );
        return;
    };

    match result_clone {
        Err(err) => {
            ui.colored_label(Color32::from_rgb(255, 110, 110), err);
        }
        Ok(rep) => {
            let title = rep.method_name.as_deref().unwrap_or("<subroutine>");
            let thumb_tag = if rep.thumb { " [Thumb]" } else { "" };
            let mut body = rep.text.clone();
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    Color32::from_rgb(130, 225, 165),
                    format!(
                        "{title} | {}{} | RVA 0x{:X} | File 0x{:X} | {} insns ({}B)",
                        rep.arch,
                        thumb_tag,
                        rep.rva,
                        rep.file_offset,
                        rep.instruction_count,
                        rep.byte_size
                    ),
                );
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(rep.text.clone());
                }
                if ui.small_button("Save .asm").clicked() {
                    save_text_with_dialog(
                        state,
                        &format!("sub_{:X}.asm", rep.rva),
                        "Assembly",
                        "asm",
                        &rep.text,
                        "native disassembly",
                    );
                }
            });
            ScrollArea::vertical()
                .id_salt("il2cpp_disasm_output_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut body)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .interactive(false),
                    );
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_text_positions(
        shape: &egui::epaint::Shape,
        out: &mut Vec<(egui::Pos2, String)>,
    ) {
        match shape {
            egui::epaint::Shape::Text(t) => {
                out.push((t.pos, t.galley.job.text.clone()));
            }
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    collect_text_positions(s, out);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn scripts_toolbar_and_weighted_columns_fit_within_narrow_dock_panel() {
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(994.0, 549.0),
            )),
            ..Default::default()
        };

        let mut hex = true;
        let mut cfg = true;
        let mut ann = true;
        let mut fields = true;
        let mut max_insns = 512usize;

        let mut texts = Vec::new();
        for frame in 0..3 {
            let mut out = ctx.run_ui(raw.clone(), |ui| {
                egui::Panel::bottom("test_dock")
                    .exact_size(400.0)
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("MonoScripts: 13660 | Managed DLLs: 0 | Native/WASM: 1");
                            ui.colored_label(
                                Color32::from_rgb(130, 225, 165),
                                "| IL2CPP v29 (elf, ARM64): 66 assemblies, 18256 types, 130543 methods",
                            );
                            let _ = ui.button("Export IL2CPP Dump...");
                        });
                        ui.horizontal_wrapped(|ui| {
                            disasm_toggle(ui, true, &mut hex, "Hex");
                            disasm_toggle(ui, true, &mut cfg, "CFG");
                            disasm_toggle(ui, true, &mut ann, "Ann");
                            disasm_toggle(ui, true, &mut fields, "Fields");
                            ui.add_enabled(
                                true,
                                egui::DragValue::new(&mut max_insns)
                                    .range(16..=4096)
                                    .prefix("Max "),
                            );
                        });
                        ui.separator();
                        weighted_columns(ui, &[0.24, 0.28, 0.48], |cols| {
                            cols[0].label("AssembliesCol");
                            cols[1].label("TypesCol");
                            cols[2].label("DisasmCol");
                        });
                    });
            });
            out.textures_delta.clear();
            if frame == 2 {
                for cs in &out.shapes {
                    collect_text_positions(&cs.shape, &mut texts);
                }
            }
        }

        for marker in [
            "Export IL2CPP Dump...",
            "Hex",
            "CFG",
            "Ann",
            "Fields",
            "AssembliesCol",
            "TypesCol",
            "DisasmCol",
        ] {
            let found = texts.iter().find(|(_, s)| s == marker);
            let (pos, _) = found.unwrap_or_else(|| panic!("missing marker `{marker}`"));
            assert!(
                pos.x < 994.0 && pos.y < 549.0,
                "`{marker}` rendered outside visible 994x549 viewport at {pos:?}"
            );
        }
    }
}
