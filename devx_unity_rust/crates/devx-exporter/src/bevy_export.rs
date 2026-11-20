//! Standalone Bevy `0.20.1` zero-logic showcase & sandbox project generator.
//!
//! Exports real 3D scenes, rigged/textured characters, props, animations, audio clips,
//! TrueType/OpenType fonts, and UI sprites from loaded/deferred Unity bundles into a
//! ready-to-run Bevy `0.20.1` crate with `manifest.json` and multi-mode stage viewer.

use crate::hierarchy::{
    derive_bundle_stem, discover_hierarchy_groups, export_hierarchy_glb, sanitize_stem,
};
use crate::{ExportError, ExportResult};
use devx_codecs::audio::decode_audio_clip;
use devx_codecs::gltf_scene::GltfSceneOptions;
use devx_codecs::text_font::decode_font;
use devx_codecs::texture::{TextureDecodeOptions, decode_sprite_cached, decode_texture2d, encode_png};
use devx_plugin_api::{AssetRef, DecodedImage};
use devx_unity_io::{UnityWorkspaceData, read_object, shared_tpk_registry};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Configuration for generating a Bevy `0.20.1` zero-logic showcase project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BevyExportOptions {
    pub project_name: String,
    pub scene_filters: Vec<String>,
    pub character_filters: Vec<String>,
    pub prop_filters: Vec<String>,
    pub anim_filters: Vec<String>,
    pub audio_filters: Vec<String>,
    pub max_scenes: usize,
    pub max_characters: usize,
    pub max_props: usize,
    pub max_audio: usize,
    pub max_fonts: usize,
    pub max_ui_sprites: usize,
    #[serde(default)]
    pub ui_sprite_filters: Vec<String>,
    pub ui_panel_filters: Vec<String>,
    pub max_ui_panels: usize,
    pub max_texture_dim: Option<u32>,
    /// When true, updates `<out_dir>/assets` and merges `<out_dir>/assets/manifest.json`
    /// without overwriting custom `Cargo.toml` / `src/*.rs` game files.
    pub assets_only: bool,
}

impl Default for BevyExportOptions {
    fn default() -> Self {
        Self {
            project_name: "bevy_zero_game".to_owned(),
            scene_filters: Vec::new(),
            character_filters: Vec::new(),
            prop_filters: Vec::new(),
            anim_filters: Vec::new(),
            audio_filters: Vec::new(),
            max_scenes: 6,
            max_characters: 8,
            max_props: 10,
            max_audio: 24,
            max_fonts: 4,
            max_ui_sprites: 32,
            ui_sprite_filters: Vec::new(),
            ui_panel_filters: Vec::new(),
            max_ui_panels: 6,
            max_texture_dim: Some(1024),
            assets_only: false,
        }
    }
}

/// Exported `.glb` entry recorded in `assets/manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BevyGlbEntry {
    pub name: String,
    pub asset_path: String,
    pub source_bundle: String,
    pub node_count: usize,
    pub mesh_count: usize,
    pub material_count: usize,
    pub texture_count: usize,
    pub skin_count: usize,
    pub animation_count: usize,
    pub triangle_count: usize,
    pub animation_names: Vec<String>,
}

/// Exported audio/font/UI media entry recorded in `assets/manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BevyMediaEntry {
    pub name: String,
    pub asset_path: String,
    pub kind: String,
}

/// Exported Unity uGUI panel layout entry recorded in `assets/manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BevyUiPanelEntry {
    pub name: String,
    pub asset_path: String,
    pub source_bundle: String,
    pub reference_width: f32,
    pub reference_height: f32,
    pub node_count: usize,
    pub graphic_count: usize,
    pub text_count: usize,
    pub resolved_texture_count: usize,
    pub missing_texture_count: usize,
}

/// Complete manifest written to `<out_dir>/assets/manifest.json` and read by Bevy at startup.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BevyExportManifest {
    pub project_name: String,
    pub unity_version: String,
    pub scenes: Vec<BevyGlbEntry>,
    pub characters: Vec<BevyGlbEntry>,
    pub props: Vec<BevyGlbEntry>,
    pub audio: Vec<BevyMediaEntry>,
    pub fonts: Vec<BevyMediaEntry>,
    pub ui_sprites: Vec<BevyMediaEntry>,
    #[serde(default)]
    pub ui_panels: Vec<BevyUiPanelEntry>,
}

/// Summary report returned to the CLI or GUI caller after exporting a Bevy project.
#[derive(Debug, Clone)]
pub struct BevyExportReport {
    pub output_dir: PathBuf,
    pub manifest: BevyExportManifest,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlbCategory {
    Scene,
    Character,
    Prop,
}

/// Generates a complete standalone Bevy `0.20.1` showcase project in `out_dir` from `workspace`.
///
/// When `workspace.deferred_bundles` is populated (e.g. an `.apk`), curated/filtered bundles are
/// automatically loaded alongside their transitive `.ab` dependencies before extraction.
pub fn export_bevy_project(
    workspace: &mut UnityWorkspaceData,
    out_dir: &Path,
    opts: &BevyExportOptions,
) -> ExportResult<BevyExportReport> {
    let assets_dir = out_dir.join("assets");
    let scenes_dir = assets_dir.join("scenes");
    let models_dir = assets_dir.join("models");
    let audio_dir = assets_dir.join("audio");
    let fonts_dir = assets_dir.join("fonts");
    let ui_dir = assets_dir.join("ui");
    let ui_panels_dir = assets_dir.join("ui_panels");

    for d in [
        &scenes_dir,
        &models_dir,
        &audio_dir,
        &fonts_dir,
        &ui_dir,
        &ui_panels_dir,
    ] {
        std::fs::create_dir_all(d)?;
    }

    let mut warnings = Vec::new();
    let mut loaded_bundle_paths = HashSet::new();

    // 1. Select & load deferred bundles with transitive dependencies if any remain deferred.
    if !workspace.deferred_bundles.is_empty() {
        let scene_vpaths = if opts.max_scenes == 0 {
            Vec::new()
        } else {
            select_deferred_candidates(
                workspace,
                &opts.scene_filters,
                &[
                    "sceneprefab/building_mimafang",
                    "sceneprefab/a-34",
                    "sceneprefab/",
                    "scene/",
                    "map/",
                    "env/",
                ],
                opts.max_scenes.saturating_mul(2).max(4),
            )
        };
        let char_vpaths = if opts.max_characters == 0 {
            Vec::new()
        } else {
            select_deferred_candidates(
                workspace,
                &opts.character_filters,
                &[
                    "role/man_clothes_chushijuese",
                    "role/man_trousers_chushijuese",
                    "role/man_glove_chushijuese",
                    "role/man_shoes_chushijuese",
                    "role/man_hair_chushijuese",
                    "role/woman_clothes_chushijuese",
                    "role/woman_trousers_chushijuese",
                    "role/woman_glove_chushijuese",
                    "role/woman_shoes_chushijuese",
                    "role/woman_hair_chushijuese",
                    "role/man_liantifu",
                    "role/monster_xlt",
                    "role/boss_gangkoushouling",
                    "role/woman_head",
                    "role/man_head",
                    "role/",
                    "partskinmesh/",
                ],
                opts.max_characters.saturating_mul(2).max(4),
            )
        };
        let prop_vpaths = if opts.max_props == 0 {
            Vec::new()
        } else {
            select_deferred_candidates(
                workspace,
                &opts.prop_filters,
                &[
                    "model/wq_qbu97",
                    "model/ak47",
                    "model/baseballbat",
                    "model/wq_bishou",
                    "model/wq_",
                    "part/",
                    "model/",
                    "weapon/",
                    "prop/",
                ],
                opts.max_props.saturating_mul(2).max(4),
            )
        };
        let anim_vpaths = select_deferred_candidates(
            workspace,
            &opts.anim_filters,
            &["anim/", "role/animationbundle"],
            6,
        );
        let audio_vpaths = if opts.max_audio == 0 {
            Vec::new()
        } else {
            select_deferred_candidates(
                workspace,
                &opts.audio_filters,
                &[
                    "wq_bq_1_gunshot",
                    "wq_ump9_1_gunshot",
                    "wq_m92_1_gunshot",
                    "wq_mg4_1_gunshot",
                    "wq_jiqiangpaotai_gunshot",
                    "gunattack_mutou",
                    "sheshi_cuihui_shitou",
                    "sheshi_tizi_fangxia",
                    "sheshi_chuwuxiang_kai",
                    "combat_banzhuan_body2",
                    "common_mensui",
                    "bag_yaoping",
                    "footsteps_move_caodi_02",
                    "monster_jiangshi1_gj",
                    "monster_jiangshi3_die",
                    "animal_yezhu_daiji",
                    "combat_dilei_baozha",
                    "bgm_wenmingchongqi",
                    "sound/",
                    "audio/",
                ],
                opts.max_audio.max(4),
            )
        };
        let font_vpaths = if opts.max_fonts == 0 {
            Vec::new()
        } else {
            select_deferred_candidates(workspace, &[], &["fonts/", "font/"], opts.max_fonts.min(4))
        };
        let ui_vpaths = if opts.max_ui_sprites == 0 {
            Vec::new()
        } else if !opts.ui_sprite_filters.is_empty() {
            select_deferred_candidates(
                workspace,
                &opts.ui_sprite_filters,
                &[],
                opts.max_ui_sprites.max(64),
            )
        } else {
            select_deferred_candidates(
                workspace,
                &[],
                &["icon/", "uiatlas/", "ui/"],
                3,
            )
        };
        let ui_panel_vpaths = select_deferred_ui_panel_candidates(
            workspace,
            &opts.ui_panel_filters,
            opts.max_ui_panels,
        );

        let all_picks = scene_vpaths
            .into_iter()
            .chain(char_vpaths)
            .chain(prop_vpaths)
            .chain(anim_vpaths)
            .chain(audio_vpaths)
            .chain(font_vpaths)
            .chain(ui_vpaths)
            .chain(ui_panel_vpaths);

        for vpath in all_picks {
            if !loaded_bundle_paths.insert(vpath.clone()) {
                continue;
            }
            if let Err(err) = workspace.load_deferred_bundles_with_deps(Some(&vpath)) {
                warnings.push(format!("dependency load `{vpath}`: {err}"));
            }
        }
    }

    let unity_version = workspace
        .files
        .iter()
        .map(|f| f.file.unity_version.clone())
        .find(|v| !v.is_empty() && !v.starts_with("0."))
        .unwrap_or_else(|| "2021.3.0f1".to_owned());

    let base_glb_opts = GltfSceneOptions {
        include_textures: true,
        include_materials: true,
        include_skinning: true,
        include_animations: true,
        fallback_unity_version: unity_version.clone(),
        max_texture_dim: opts.max_texture_dim,
        zero_root_transform: false,
        skip_secondary_lods: true,
    };

    let showcase_clips = collect_showcase_animation_clips(workspace, &opts.anim_filters);
    let mut groups = discover_hierarchy_groups(workspace, None);
    groups.sort_by_key(|g| group_priority_key(workspace, &g.source_bundle, &g.stem, &g.roots, opts));
    let has_non_internal_groups = groups
        .iter()
        .any(|g| !g.source_bundle.to_ascii_lowercase().contains("bin/data/"));

    let mut manifest = BevyExportManifest {
        project_name: sanitize_package_name(&opts.project_name),
        unity_version,
        ..Default::default()
    };

    let mut used_glb_names = HashSet::new();
    for group in groups {
        if has_non_internal_groups
            && group.source_bundle.to_ascii_lowercase().contains("bin/data/")
        {
            continue;
        }
        if is_excluded_ui_or_meta_group(workspace, &group.source_bundle, &group.stem, &group.roots, opts) {
            continue;
        }
        let cat = classify_group(workspace, &group.source_bundle, &group.roots, opts);
        let (target_dir, subdir, cur_len, cap) = match cat {
            GlbCategory::Scene => (&scenes_dir, "scenes", manifest.scenes.len(), opts.max_scenes),
            GlbCategory::Character => (
                &models_dir,
                "models",
                manifest.characters.len(),
                opts.max_characters,
            ),
            GlbCategory::Prop => (&models_dir, "models", manifest.props.len(), opts.max_props),
        };
        if cur_len >= cap {
            continue;
        }

        let mut stem = sanitize_stem(&group.stem);
        if !used_glb_names.insert((subdir, stem.clone())) {
            let mut n = 2usize;
            while !used_glb_names.insert((subdir, format!("{stem}_{n}"))) {
                n += 1;
            }
            stem = format!("{stem}_{n}");
        }

        // Only attach workspace-wide character AnimationClips to rigged Characters (not static buildings/props).
        let extra_for_group: &[AssetRef] =
            if cat == GlbCategory::Character || group_has_skinned_mesh(workspace, &group.roots) {
                &showcase_clips
            } else {
                &[]
            };
        let glb_opts = GltfSceneOptions {
            zero_root_transform: cat != GlbCategory::Scene,
            ..base_glb_opts.clone()
        };
        let out_glb = target_dir.join(format!("{stem}.glb"));
        match export_hierarchy_glb(workspace, &group.roots, extra_for_group, &out_glb, &glb_opts) {
            Ok(rep) => {
                if rep.summary.mesh_count == 0 {
                    let _ = std::fs::remove_file(&out_glb);
                    continue;
                }
                let entry = BevyGlbEntry {
                    name: stem.clone(),
                    asset_path: format!("{subdir}/{stem}.glb"),
                    source_bundle: group.source_bundle,
                    node_count: rep.summary.node_count,
                    mesh_count: rep.summary.mesh_count,
                    material_count: rep.summary.material_count,
                    texture_count: rep.summary.texture_count,
                    skin_count: rep.summary.skin_count,
                    animation_count: rep.summary.animation_count,
                    triangle_count: rep.summary.triangle_count,
                    animation_names: rep.summary.animation_names,
                };
                match cat {
                    GlbCategory::Scene => manifest.scenes.push(entry),
                    GlbCategory::Character => manifest.characters.push(entry),
                    GlbCategory::Prop => manifest.props.push(entry),
                }
            }
            Err(err) => {
                let _ = std::fs::remove_file(&out_glb);
                warnings.push(format!("glb `{stem}`: {err}"));
            }
        }
    }

    // 3. Extract audio clips (`class_id == 83`).
    export_audio_assets(
        workspace,
        &audio_dir,
        opts.max_audio,
        &opts.audio_filters,
        &mut manifest,
        &mut warnings,
    );

    // 4. Extract fonts (`class_id == 128`).
    if opts.max_fonts > 0 {
        export_font_assets(workspace, &fonts_dir, opts.max_fonts, &mut manifest, &mut warnings);
    }

    // 5. Extract UI sprites (`class_id == 213` -> PNG, fallback `Texture2D` from UI bundles).
    if opts.max_ui_sprites > 0 {
        export_ui_sprite_assets(
            workspace,
            &ui_dir,
            opts.max_ui_sprites,
            &opts.ui_sprite_filters,
            &mut manifest,
            &mut warnings,
        );
    }

    // 6. Extract Unity uGUI panel layouts (`assets/ui_panels/*.json` + referenced sprites).
    if opts.max_ui_panels > 0 {
        export_ui_panel_assets(
            workspace,
            &ui_dir,
            &ui_panels_dir,
            opts,
            &mut manifest,
            &mut warnings,
        );
    }

    // Write (or merge in `--assets-only` mode) `assets/manifest.json` + optional Bevy crate files.
    let manifest_path = assets_dir.join("manifest.json");
    if opts.assets_only && manifest_path.is_file() {
        merge_and_write_manifest(&manifest_path, &manifest)?;
    } else {
        let manifest_text = serde_json::to_string_pretty(&manifest)
            .map_err(|e| ExportError::Yaml(format!("manifest serialize: {e}")))?;
        std::fs::write(&manifest_path, &manifest_text)?;
    }
    if !opts.assets_only {
        write_bevy_project_files(out_dir, &manifest)?;
    }

    Ok(BevyExportReport {
        output_dir: out_dir.to_path_buf(),
        manifest,
        warnings,
    })
}

fn is_excluded_deferred_bundle(virtual_path_low: &str) -> bool {
    virtual_path_low.contains("scenetexture/")
        || virtual_path_low.contains("animationbundle")
        || virtual_path_low.contains("panel")
        || virtual_path_low.contains("localization")
        || virtual_path_low.contains("/lua")
        || virtual_path_low.contains("/shader")
}

fn select_deferred_ui_panel_candidates(
    workspace: &UnityWorkspaceData,
    user_filters: &[String],
    limit: usize,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let mut picked = Vec::new();
    let mut seen = HashSet::new();

    if !user_filters.is_empty() {
        // Group sorted matches per filter so an earlier broad filter never starves later ones,
        // excluding atlas/texture helper bundles (auto-pulled via `load_deferred_bundles_with_deps`).
        let mut per_filter_queues: Vec<Vec<String>> = Vec::with_capacity(user_filters.len());
        for pat in user_filters {
            let p = pat.trim().to_ascii_lowercase();
            if p.is_empty() {
                continue;
            }
            let mut matches: Vec<_> = workspace
                .deferred_bundles
                .iter()
                .filter(|b| {
                    let low = b.virtual_path.to_ascii_lowercase();
                    let is_helper_asset = low.contains("uiatlas")
                        || low.contains("/texture/")
                        || low.contains("/icon/");
                    matches_filter_pattern(&low, &p) && !is_helper_asset && b.compressed_size > 1024
                })
                .collect();
            matches.sort_by(|a, b| {
                let a_low = a.virtual_path.to_ascii_lowercase();
                let b_low = b.virtual_path.to_ascii_lowercase();
                let a_stem = Path::new(&a_low)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                let b_stem = Path::new(&b_low)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                let a_exact = a_stem == p;
                let b_exact = b_stem == p;
                let a_ui = a_low.contains("panel") || a_low.contains("ui/");
                let b_ui = b_low.contains("panel") || b_low.contains("ui/");
                b_exact
                    .cmp(&a_exact)
                    .then_with(|| b_ui.cmp(&a_ui))
                    .then_with(|| a.virtual_path.len().cmp(&b.virtual_path.len()))
            });
            per_filter_queues.push(
                matches
                    .into_iter()
                    .map(|m| m.virtual_path.clone())
                    .collect(),
            );
        }

        let max_len = per_filter_queues.iter().map(Vec::len).max().unwrap_or(0);
        for round in 0..max_len {
            for queue in &per_filter_queues {
                if let Some(vp) = queue.get(round)
                    && seen.insert(vp.clone())
                {
                    picked.push(vp.clone());
                    if picked.len() >= limit {
                        return picked;
                    }
                }
            }
        }
        return picked;
    }

    let mut candidates: Vec<_> = workspace
        .deferred_bundles
        .iter()
        .filter(|b| {
            let low = b.virtual_path.to_ascii_lowercase();
            let is_ui_panel = low.starts_with("ui/")
                || low.contains("/ui/")
                || (low.contains("panel")
                    && !low.contains("uiatlas")
                    && !low.contains("icon")
                    && !low.contains("texture"));
            is_ui_panel && b.compressed_size > 4096 && !low.contains("localization")
        })
        .collect();
    candidates.sort_by(|a, b| {
        let alow = a.virtual_path.to_ascii_lowercase();
        let blow = b.virtual_path.to_ascii_lowercase();
        let a_pri = alow.contains("loading")
            || alow.contains("setting")
            || alow.contains("login")
            || alow.contains("main");
        let b_pri = blow.contains("loading")
            || blow.contains("setting")
            || blow.contains("login")
            || blow.contains("main");
        b_pri
            .cmp(&a_pri)
            .then_with(|| (a.compressed_size > 20_000).cmp(&(b.compressed_size > 20_000)).reverse())
            .then_with(|| a.virtual_path.len().cmp(&b.virtual_path.len()))
    });
    for c in candidates {
        if seen.insert(c.virtual_path.clone()) {
            picked.push(c.virtual_path.clone());
            if picked.len() >= limit {
                break;
            }
        }
    }
    picked
}

fn select_deferred_candidates(
    workspace: &UnityWorkspaceData,
    user_filters: &[String],
    default_patterns: &[&str],
    limit: usize,
) -> Vec<String> {
    let mut picked = Vec::new();
    let mut seen = HashSet::new();

    if !user_filters.is_empty() {
        // Round-robin sorted matches per filter so an earlier broad filter never starves later ones.
        let mut queues: Vec<Vec<String>> = Vec::with_capacity(user_filters.len());
        for pat in user_filters {
            let trimmed = pat.trim();
            if trimmed.is_empty() {
                continue;
            }
            let mut matches: Vec<_> = workspace
                .deferred_bundles
                .iter()
                .filter(|b| matches_filter_pattern(&b.virtual_path.to_ascii_lowercase(), trimmed))
                .collect();
            matches.sort_by_key(|a| {
                let is_tex_shard = a.virtual_path.to_ascii_lowercase().contains("scenetexture/");
                (is_tex_shard, std::cmp::Reverse(a.compressed_size))
            });
            queues.push(matches.into_iter().map(|m| m.virtual_path.clone()).collect());
        }
        let max_len = queues.iter().map(Vec::len).max().unwrap_or(0);
        for round in 0..max_len {
            for q in &queues {
                if let Some(vp) = q.get(round)
                    && seen.insert(vp.clone())
                {
                    picked.push(vp.clone());
                    if picked.len() >= limit {
                        return picked;
                    }
                }
            }
        }
        return picked;
    }

    for pat in default_patterns {
        let mut matches: Vec<_> = workspace
            .deferred_bundles
            .iter()
            .filter(|b| {
                let low = b.virtual_path.to_ascii_lowercase();
                low.contains(pat) && b.compressed_size > 2048 && !is_excluded_deferred_bundle(&low)
            })
            .collect();
        matches.sort_by(|a, b| {
            // Prefer moderately sized realistic bundles without stalling on giant archives first,
            // while preferring non-trivial (>10 KB) bundles over stubs.
            let a_big = a.compressed_size > 10_000;
            let b_big = b.compressed_size > 10_000;
            b_big
                .cmp(&a_big)
                .then_with(|| a.virtual_path.len().cmp(&b.virtual_path.len()))
        });
        for m in matches {
            if seen.insert(m.virtual_path.clone()) {
                picked.push(m.virtual_path.clone());
                if picked.len() >= limit {
                    return picked;
                }
            }
        }
    }

    picked
}

fn matches_filter_pattern(haystack_low: &str, pattern: &str) -> bool {
    pattern
        .split('|')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .any(|alt| haystack_low.contains(&alt))
}

fn matches_any_filter(haystack: &str, filters: &[String]) -> bool {
    let low = haystack.to_ascii_lowercase();
    filters
        .iter()
        .any(|f| !f.trim().is_empty() && matches_filter_pattern(&low, f))
}

fn is_ui_button_clip_name(name_low: &str) -> bool {
    matches!(
        name_low,
        "normal" | "highlighted" | "pressed" | "selected" | "disabled" | "show" | "hide" | "open" | "close"
    ) || name_low.contains("highlighted")
        || name_low.contains("pressed")
        || name_low.contains("disabled")
        || name_low.contains("selected")
        || name_low.contains("laddertask")
        || name_low.contains("panel")
        || name_low.contains("button")
        || name_low.contains("btn_")
        || name_low.contains("_btn")
        || name_low.contains("ui_")
        || name_low.contains("_ui")
        || name_low.contains("popup")
        || name_low.contains("dialog")
        || name_low.contains("hud")
        || name_low.contains("reddot")
        || name_low.contains("tooltip")
        || name_low.contains("leaderboard")
}

fn is_core_role_showcase_clip(name_low: &str) -> bool {
    name_low == "kongshou_zhan_idle01"
        || name_low == "womankongshou_zhan_idle01"
        || name_low.contains("kongshou_zhan_idle")
        || name_low.contains("womankongshou_zhan_idle")
        || name_low == "idle"
        || name_low == "idle01"
        || name_low == "walk"
        || name_low == "run"
        || name_low == "jump"
}

fn collect_showcase_animation_clips(
    workspace: &UnityWorkspaceData,
    anim_filters: &[String],
) -> Vec<AssetRef> {
    let mut file_order: Vec<usize> = (0..workspace.files.len()).collect();
    file_order.sort_by_key(|&idx| {
        let low = workspace.files[idx].virtual_path.to_ascii_lowercase();
        let tier = if low.contains("anim/") || low.contains("partanim/") {
            0u8
        } else if low.contains("role/") || low.contains("partskinmesh/") {
            1u8
        } else if low.contains("panel")
            || low.contains("/ui")
            || low.contains("uiatlas")
            || low == "resources.assets"
        {
            3u8
        } else {
            2u8
        };
        (tier, idx)
    });

    let mut clips = Vec::new();
    let mut seen_names = HashSet::new();
    const MAX_SHOWCASE_CLIPS: usize = 64;
    const MAX_ANIMATIONBUNDLE_CLIPS: usize = 16;

    for file_index in file_order {
        let loaded = &workspace.files[file_index];
        let path_low = loaded.virtual_path.to_ascii_lowercase();
        let is_role_anim_bundle = path_low.contains("role/animationbundle")
            || (path_low.contains("animationbundle") && !anim_filters.is_empty());
        let is_ui_bundle = (!is_role_anim_bundle && path_low.contains("animationbundle"))
            || path_low.contains("panel")
            || path_low.contains("/ui/")
            || path_low.starts_with("ui/")
            || path_low.contains("uiatlas")
            || path_low == "resources.assets";
        if is_ui_bundle {
            continue;
        }
        let mut bundle_added = 0usize;
        for info in &loaded.file.objects {
            if info.type_id != 74 {
                continue;
            }
            let Ok(obj) = read_object(loaded, info) else {
                continue;
            };
            let Some(name) = obj.name() else {
                continue;
            };
            let name_low = name.trim().to_ascii_lowercase();
            if name_low.is_empty() || is_ui_button_clip_name(&name_low) {
                continue;
            }
            if is_role_anim_bundle {
                let explicit_hit = anim_filters.iter().any(|filter| {
                    if let Some(exact) = filter.trim().strip_prefix('=') {
                        name_low.eq_ignore_ascii_case(exact)
                    } else {
                        matches_filter_pattern(&name_low, filter)
                    }
                });
                if !explicit_hit && !is_core_role_showcase_clip(&name_low) {
                    continue;
                }
            }
            if !seen_names.insert(name_low) {
                continue;
            }
            clips.push(AssetRef {
                file_index,
                path_id: info.path_id,
            });
            bundle_added += 1;
            if clips.len() >= MAX_SHOWCASE_CLIPS {
                return clips;
            }
            if is_role_anim_bundle && bundle_added >= MAX_ANIMATIONBUNDLE_CLIPS {
                break;
            }
        }
    }
    clips
}

fn group_has_skinned_mesh(workspace: &UnityWorkspaceData, roots: &[AssetRef]) -> bool {
    let mut checked_files = HashSet::new();
    for root in roots {
        if !checked_files.insert(root.file_index) {
            continue;
        }
        if let Some(loaded) = workspace.files.get(root.file_index)
            && loaded.file.objects.iter().any(|o| o.type_id == 137)
        {
            return true;
        }
    }
    false
}

fn is_ui_panel_name(low: &str) -> bool {
    low.contains("panel")
        || low.contains("canvas")
        || low.contains("uiview")
        || low.contains("uipop")
        || low.contains("uiwindow")
        || low.contains("uipanel")
        || low.contains("dialog")
        || low.contains("popup")
        || low.contains("leaderboard")
        || low.contains("animationbundle")
        || low.starts_with("ui_")
        || low.ends_with("_ui")
}

fn is_excluded_ui_or_meta_group(
    workspace: &UnityWorkspaceData,
    source_bundle: &str,
    stem: &str,
    roots: &[AssetRef],
    opts: &BevyExportOptions,
) -> bool {
    let bundle_low = source_bundle.to_ascii_lowercase();
    let stem_low = stem.to_ascii_lowercase();
    let wants_scenetex = opts
        .scene_filters
        .iter()
        .chain(opts.character_filters.iter())
        .chain(opts.prop_filters.iter())
        .any(|f| f.to_ascii_lowercase().contains("scenetexture"));
    if bundle_low.contains("scenetexture/") && !wants_scenetex {
        return true;
    }

    let user_requested = matches_any_filter(source_bundle, &opts.scene_filters)
        || matches_any_filter(source_bundle, &opts.character_filters)
        || matches_any_filter(source_bundle, &opts.prop_filters)
        || matches_any_filter(stem, &opts.scene_filters)
        || matches_any_filter(stem, &opts.character_filters)
        || matches_any_filter(stem, &opts.prop_filters);
    if user_requested {
        return false;
    }

    if is_ui_panel_name(&bundle_low)
        || is_ui_panel_name(&stem_low)
        || bundle_low.contains("/ui/")
        || bundle_low.starts_with("ui/")
        || bundle_low.contains("uiatlas")
        || bundle_low.ends_with(".sharedassets")
    {
        return true;
    }

    if let Some(first) = roots.first()
        && let Some(loaded) = workspace.files.get(first.file_index)
    {
        let mut ui_count = 0usize;
        let mut mesh_count = 0usize;
        let mut has_skin = false;
        for o in &loaded.file.objects {
            match o.type_id {
                222 | 224 => ui_count += 1,
                23 => mesh_count += 1,
                137 => has_skin = true,
                _ => {}
            }
        }
        if !has_skin && ui_count > 0 && ui_count >= mesh_count {
            return true;
        }
    }

    false
}

fn group_priority_key(
    workspace: &UnityWorkspaceData,
    source_bundle: &str,
    stem: &str,
    roots: &[AssetRef],
    opts: &BevyExportOptions,
) -> (u8, u8, String) {
    let low = source_bundle.to_ascii_lowercase();
    let user_hit = matches_any_filter(&low, &opts.scene_filters)
        || matches_any_filter(&low, &opts.character_filters)
        || matches_any_filter(&low, &opts.prop_filters)
        || matches_any_filter(stem, &opts.scene_filters)
        || matches_any_filter(stem, &opts.character_filters)
        || matches_any_filter(stem, &opts.prop_filters);
    let has_skin = group_has_skinned_mesh(workspace, roots);
    let skin_rank = u8::from(!has_skin);

    if user_hit && !low.contains("scenetexture/") {
        return (0, skin_rank, stem.to_owned());
    }
    let is_curated = low.contains("sceneprefab/")
        || low.contains("role/")
        || low.contains("partskinmesh/")
        || low.contains("model/");
    if is_curated {
        return (1, skin_rank, stem.to_owned());
    }
    let is_internal_stub = low.contains("bin/data/")
        || low.contains("scenetexture/")
        || low.ends_with(".sharedassets")
        || low == "resources.assets";
    if !is_internal_stub {
        return (2, skin_rank, stem.to_owned());
    }
    if low.contains("bin/data/") {
        return (4, skin_rank, stem.to_owned());
    }
    (3, skin_rank, stem.to_owned())
}

fn classify_group(
    workspace: &UnityWorkspaceData,
    source_bundle: &str,
    roots: &[AssetRef],
    opts: &BevyExportOptions,
) -> GlbCategory {
    let low = source_bundle.to_ascii_lowercase();
    if matches_any_filter(&low, &opts.scene_filters) && !low.contains("scenetexture/") {
        return GlbCategory::Scene;
    }
    if matches_any_filter(&low, &opts.character_filters) {
        return GlbCategory::Character;
    }
    if matches_any_filter(&low, &opts.prop_filters) {
        return GlbCategory::Prop;
    }
    if low.contains("scene/")
        || low.contains("sceneprefab/")
        || low.contains("building")
        || low.contains("terrain")
        || low.contains("a-34")
    {
        return GlbCategory::Scene;
    }
    if low.contains("role/")
        || low.contains("partskinmesh/")
        || low.contains("monster")
        || low.contains("woman_")
        || low.contains("man_")
        || low.contains("character")
        || low.contains("hero")
    {
        return GlbCategory::Character;
    }
    if low.contains("model/")
        || low.contains("weapon/")
        || low.contains("prop/")
        || low.contains("part/")
    {
        return GlbCategory::Prop;
    }

    // Fallback inspection: require real SkinnedMeshRenderer (`137`), not Animator (`95`),
    // so UI prefabs or animated props do not get classified as Characters.
    if group_has_skinned_mesh(workspace, roots) {
        return GlbCategory::Character;
    }
    if let Some(first) = roots.first()
        && let Some(loaded) = workspace.files.get(first.file_index)
    {
        let transform_count = loaded
            .file
            .objects
            .iter()
            .filter(|o| o.type_id == 4)
            .count();
        if transform_count > 40 {
            return GlbCategory::Scene;
        }
    }

    GlbCategory::Prop
}

fn ordered_file_indices(workspace: &UnityWorkspaceData, preferred_tags: &[&str]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..workspace.files.len()).collect();
    order.sort_by_key(|&idx| {
        let low = workspace.files[idx].virtual_path.to_ascii_lowercase();
        let is_internal = low.contains("bin/data/");
        let is_pref = preferred_tags.iter().any(|t| low.contains(t));
        (is_internal, !is_pref, idx)
    });
    order
}

fn merge_and_write_manifest(manifest_path: &Path, incoming: &BevyExportManifest) -> ExportResult<()> {
    let existing_raw = std::fs::read_to_string(manifest_path)?;
    let mut root_val: serde_json::Value = serde_json::from_str(&existing_raw)
        .map_err(|e| ExportError::Yaml(format!("manifest parse: {e}")))?;
    let incoming_val = serde_json::to_value(incoming)
        .map_err(|e| ExportError::Yaml(format!("manifest serialize: {e}")))?;

    if let (Some(dst_obj), Some(src_obj)) = (root_val.as_object_mut(), incoming_val.as_object()) {
        for key in [
            "scenes",
            "characters",
            "props",
            "audio",
            "fonts",
            "ui_sprites",
            "ui_panels",
        ] {
            let Some(src_arr) = src_obj.get(key).and_then(|v| v.as_array()) else {
                continue;
            };
            let dst_entry = dst_obj
                .entry(key)
                .or_insert_with(|| serde_json::Value::Array(Vec::new()));
            let Some(dst_arr) = dst_entry.as_array_mut() else {
                continue;
            };
            let mut seen_paths: HashSet<String> = dst_arr
                .iter()
                .filter_map(|item| {
                    item.get("asset_path")
                        .and_then(|p| p.as_str())
                        .map(ToOwned::to_owned)
                })
                .collect();
            for item in src_arr {
                let path = item
                    .get("asset_path")
                    .and_then(|p| p.as_str())
                    .unwrap_or_default();
                if path.is_empty() {
                    continue;
                }
                if seen_paths.insert(path.to_owned()) {
                    dst_arr.push(item.clone());
                } else if let Some(slot) = dst_arr.iter_mut().find(|d| {
                    d.get("asset_path").and_then(|p| p.as_str()) == Some(path)
                }) {
                    *slot = item.clone();
                }
            }
        }
    }

    let text = serde_json::to_string_pretty(&root_val)
        .map_err(|e| ExportError::Yaml(format!("manifest serialize: {e}")))?;
    std::fs::write(manifest_path, text)?;
    Ok(())
}

fn export_audio_assets(
    workspace: &UnityWorkspaceData,
    audio_dir: &Path,
    limit: usize,
    audio_filters: &[String],
    manifest: &mut BevyExportManifest,
    warnings: &mut Vec<String>,
) {
    if limit == 0 {
        return;
    }
    let mut used = HashSet::new();
    let mut order = ordered_file_indices(workspace, &["sound/", "audio/", "bgm/", "sfx/"]);
    if !audio_filters.is_empty() {
        order.sort_by_key(|&idx| {
            let vp = &workspace.files[idx].virtual_path;
            let stem = Path::new(vp)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let matched = matches_any_filter(vp, audio_filters)
                || matches_any_filter(stem, audio_filters);
            !matched
        });
    }
    for file_index in order {
        let loaded = &workspace.files[file_index];
        if !audio_filters.is_empty() && !matches_any_filter(&loaded.virtual_path, audio_filters) {
            let has_match = loaded.file.objects.iter().any(|o| {
                o.type_id == 83
                    && read_object(loaded, o)
                        .ok()
                        .and_then(|obj| obj.name())
                        .is_some_and(|n| matches_any_filter(&n, audio_filters))
            });
            if !has_match {
                continue;
            }
        }
        for info in &loaded.file.objects {
            if manifest.audio.len() >= limit {
                return;
            }
            if info.type_id != 83 {
                continue;
            }
            let Ok(obj) = read_object(loaded, info) else {
                continue;
            };
            let ver = if !loaded.file.unity_version.is_empty() && !loaded.file.unity_version.starts_with("0.") {
                loaded.file.unity_version.as_str()
            } else {
                manifest.unity_version.as_str()
            };
            match decode_audio_clip(&obj, ver, &workspace.streams) {
                Ok(decoded) => {
                    let clip = &decoded.primary;
                    let (ext, bytes) = if !clip.samples_i16.is_empty() && clip.sample_rate > 0 {
                        match devx_codecs::audio::encode_wav(clip) {
                            Ok(wav) => ("wav", wav),
                            Err(e) => {
                                warnings.push(format!("audio wav encode {}: {e}", info.path_id));
                                continue;
                            }
                        }
                    } else if let Some((raw_ext, raw_bytes)) = &clip.raw_container {
                        let ext_low = raw_ext.to_ascii_lowercase();
                        let fmt_low = clip.source_format.to_ascii_lowercase();
                        if raw_bytes.starts_with(b"OggS")
                            || ext_low == "ogg"
                            || fmt_low.contains("ogg")
                        {
                            ("ogg", raw_bytes.clone())
                        } else if raw_bytes.starts_with(b"RIFF")
                            || ext_low == "wav"
                            || fmt_low.contains("wav")
                        {
                            ("wav", raw_bytes.clone())
                        } else if ext_low == "mp3" || fmt_low.contains("mp3") {
                            ("mp3", raw_bytes.clone())
                        } else {
                            continue;
                        }
                    } else {
                        continue;
                    };
                    let mut stem = sanitize_stem(&decoded.name);
                    if stem == "asset" {
                        stem = derive_bundle_stem(&loaded.virtual_path, file_index);
                    }
                    let unique = make_unique_stem(&mut used, &stem);
                    let fname = format!("{unique}.{ext}");
                    if std::fs::write(audio_dir.join(&fname), &bytes).is_ok() {
                        manifest.audio.push(BevyMediaEntry {
                            name: decoded.name.clone(),
                            asset_path: format!("audio/{fname}"),
                            kind: ext.to_owned(),
                        });
                    }
                }
                Err(e) => warnings.push(format!("audio {}: {e}", info.path_id)),
            }
        }
    }
}

fn export_font_assets(
    workspace: &UnityWorkspaceData,
    fonts_dir: &Path,
    limit: usize,
    manifest: &mut BevyExportManifest,
    warnings: &mut Vec<String>,
) {
    let mut used = HashSet::new();
    for loaded in &workspace.files {
        for info in &loaded.file.objects {
            if manifest.fonts.len() >= limit {
                return;
            }
            if info.type_id != 128 {
                continue;
            }
            let Ok(obj) = read_object(loaded, info) else {
                continue;
            };
            match decode_font(&obj) {
                Ok(font) => {
                    let ext = font.format.extension();
                    if ext != "ttf" && ext != "otf" {
                        continue;
                    }
                    let stem = make_unique_stem(&mut used, &sanitize_stem(&font.name));
                    let fname = format!("{stem}.{ext}");
                    if std::fs::write(fonts_dir.join(&fname), &font.data).is_ok() {
                        manifest.fonts.push(BevyMediaEntry {
                            name: font.name,
                            asset_path: format!("fonts/{fname}"),
                            kind: ext.to_owned(),
                        });
                    }
                }
                Err(e) => warnings.push(format!("font {}: {e}", info.path_id)),
            }
        }
    }
}

fn export_ui_sprite_assets(
    workspace: &UnityWorkspaceData,
    ui_dir: &Path,
    limit: usize,
    ui_sprite_filters: &[String],
    manifest: &mut BevyExportManifest,
    warnings: &mut Vec<String>,
) {
    let tpk = shared_tpk_registry();
    let mut tex_cache: HashMap<(usize, i64), DecodedImage> = HashMap::new();
    let mut used = HashSet::new();

    let file_order: Vec<usize> = if ui_sprite_filters.is_empty() {
        ordered_file_indices(workspace, &["icon", "uiatlas", "/ui"])
    } else {
        let mut order = Vec::with_capacity(workspace.files.len());
        let mut seen_fi = HashSet::new();
        for (i, loaded) in workspace.files.iter().enumerate() {
            if matches_any_filter(&loaded.virtual_path, ui_sprite_filters) && seen_fi.insert(i) {
                order.push(i);
            }
        }
        for i in ordered_file_indices(workspace, &["icon", "uiatlas", "/ui", "texture"]) {
            if seen_fi.insert(i) {
                order.push(i);
            }
        }
        for i in 0..workspace.files.len() {
            if seen_fi.insert(i) {
                order.push(i);
            }
        }
        order
    };

    for file_index in file_order.iter().copied() {
        let loaded = &workspace.files[file_index];
        let bundle_match = !ui_sprite_filters.is_empty()
            && matches_any_filter(&loaded.virtual_path, ui_sprite_filters);
        for info in &loaded.file.objects {
            if manifest.ui_sprites.len() >= limit {
                return;
            }
            if info.type_id != 213 {
                continue;
            }
            let cpath_opt = loaded.bundle_container_paths.get(&info.path_id);
            let cpath_match = !ui_sprite_filters.is_empty()
                && cpath_opt.is_some_and(|cp| matches_any_filter(cp, ui_sprite_filters));
            let Ok(obj) = read_object(loaded, info) else {
                continue;
            };
            if !ui_sprite_filters.is_empty() && !bundle_match && !cpath_match {
                let name_match = obj
                    .name()
                    .as_deref()
                    .is_some_and(|n| matches_any_filter(n, ui_sprite_filters));
                if !name_match {
                    continue;
                }
            }
            let ver = if !loaded.file.unity_version.is_empty() && !loaded.file.unity_version.starts_with("0.") {
                loaded.file.unity_version.as_str()
            } else {
                manifest.unity_version.as_str()
            };
            match decode_sprite_cached(&obj, file_index, ver, workspace, &*tpk, &mut tex_cache) {
                Ok(sp) if sp.image.width > 4 && sp.image.height > 4 => {
                    if !ui_sprite_filters.is_empty()
                        && !bundle_match
                        && !cpath_match
                        && !matches_any_filter(&sp.name, ui_sprite_filters)
                    {
                        continue;
                    }
                    let stem = make_unique_stem(&mut used, &sanitize_stem(&sp.name));
                    let fname = format!("{stem}.png");
                    if let Ok(png) = encode_png(&sp.image)
                        && std::fs::write(ui_dir.join(&fname), &png).is_ok()
                    {
                        manifest.ui_sprites.push(BevyMediaEntry {
                            name: sp.name,
                            asset_path: format!("ui/{fname}"),
                            kind: "sprite".to_owned(),
                        });
                    }
                }
                Ok(_) => {}
                Err(e) => warnings.push(format!("sprite {}: {e}", info.path_id)),
            }
        }
    }

    // Fallback to UI bundle Texture2Ds if no `Sprite` objects matched or explicit filters look for raw Texture2Ds.
    if manifest.ui_sprites.is_empty() || !ui_sprite_filters.is_empty() {
        for file_index in file_order {
            let loaded = &workspace.files[file_index];
            let low = loaded.virtual_path.to_ascii_lowercase();
            let bundle_match = !ui_sprite_filters.is_empty()
                && matches_any_filter(&loaded.virtual_path, ui_sprite_filters);
            if ui_sprite_filters.is_empty()
                && !low.contains("icon")
                && !low.contains("ui")
                && !low.contains("atlas")
            {
                continue;
            }
            for info in &loaded.file.objects {
                if manifest.ui_sprites.len() >= limit {
                    return;
                }
                if info.type_id != 28 {
                    continue;
                }
                let cpath_opt = loaded.bundle_container_paths.get(&info.path_id);
                let cpath_match = !ui_sprite_filters.is_empty()
                    && cpath_opt.is_some_and(|cp| matches_any_filter(cp, ui_sprite_filters));
                let Ok(obj) = read_object(loaded, info) else {
                    continue;
                };
                let name = obj.name().unwrap_or_else(|| format!("ui_{}", info.path_id));
                if !ui_sprite_filters.is_empty()
                    && !bundle_match
                    && !cpath_match
                    && !matches_any_filter(&name, ui_sprite_filters)
                {
                    continue;
                }
                let sanitized = sanitize_stem(&name);
                if used.contains(&sanitized) {
                    continue;
                }
                if let Ok(img) = decode_texture2d(
                    &obj,
                    manifest.unity_version.as_str(),
                    &workspace.streams,
                    TextureDecodeOptions {
                        flip_vertical: true,
                        ..Default::default()
                    },
                ) && let Ok(png) = encode_png(&img)
                {
                    let stem = make_unique_stem(&mut used, &sanitized);
                    let fname = format!("{stem}.png");
                    if std::fs::write(ui_dir.join(&fname), &png).is_ok() {
                        manifest.ui_sprites.push(BevyMediaEntry {
                            name,
                            asset_path: format!("ui/{fname}"),
                            kind: "texture2d".to_owned(),
                        });
                    }
                }
            }
        }
    }
}

fn export_ui_panel_assets(
    workspace: &UnityWorkspaceData,
    ui_dir: &Path,
    ui_panels_dir: &Path,
    opts: &BevyExportOptions,
    manifest: &mut BevyExportManifest,
    warnings: &mut Vec<String>,
) {
    use crate::ui_canvas::{
        DEFAULT_MAX_UI_NODES, UiExtractionCache, extract_ui_canvas_cached,
    };

    let mut ui_cache = UiExtractionCache::default();
    let mut seen_roots: HashSet<AssetRef> = HashSet::new();
    let mut extracted_panels = Vec::new();
    let extract_cap = (opts.max_ui_panels * 3).max(12);

    // Collect candidate root GameObjects from loaded UI/panel bundles or Canvases.
    let mut candidate_roots: Vec<(usize, AssetRef, bool)> = Vec::new();
    for (file_index, loaded) in workspace.files.iter().enumerate() {
        let low = loaded.virtual_path.to_ascii_lowercase();
        let is_ui_bundle = low.starts_with("ui/")
            || low.contains("/ui/")
            || low.contains("panel");
        let filter_match = opts.ui_panel_filters.is_empty()
            || matches_any_filter(&loaded.virtual_path, &opts.ui_panel_filters);

        if is_ui_bundle || filter_match {
            for (&path_id, cpath) in &loaded.bundle_container_paths {
                let cpath_low = cpath.to_ascii_lowercase();
                if !cpath_low.ends_with(".prefab") {
                    continue;
                }
                if !opts.ui_panel_filters.is_empty()
                    && !matches_any_filter(&loaded.virtual_path, &opts.ui_panel_filters)
                    && !matches_any_filter(cpath, &opts.ui_panel_filters)
                {
                    continue;
                }
                if loaded
                    .file
                    .objects
                    .iter()
                    .any(|o| o.path_id == path_id && o.type_id == 1)
                {
                    candidate_roots.push((file_index, AssetRef { file_index, path_id }, true));
                }
            }
        }

        // Also inspect explicit Canvas (`class_id == 223`) or root RectTransform (`224`) when
        // container paths are absent (e.g. standalone `.prefab` / unpacked `.assets`).
        if filter_match {
            for info in &loaded.file.objects {
                if info.type_id == 223 {
                    candidate_roots.push((
                        file_index,
                        AssetRef {
                            file_index,
                            path_id: info.path_id,
                        },
                        is_ui_bundle,
                    ));
                }
            }
        }
    }

    for (_fi, cand_ref, _is_ui_bundle) in candidate_roots {
        if extracted_panels.len() >= extract_cap {
            break;
        }
        let Some(extracted) =
            extract_ui_canvas_cached(workspace, cand_ref, DEFAULT_MAX_UI_NODES, &mut ui_cache)
        else {
            continue;
        };
        if !extracted.document.has_ui_content()
            || (extracted.document.stats.graphic_count == 0
                && extracted.document.stats.interactive_count == 0)
        {
            continue;
        }
        if !opts.ui_panel_filters.is_empty()
            && !matches_any_filter(&extracted.document.source_bundle, &opts.ui_panel_filters)
            && !matches_any_filter(&extracted.document.name, &opts.ui_panel_filters)
        {
            continue;
        }
        if !seen_roots.insert(extracted.document.root_ref) {
            continue;
        }
        extracted_panels.push(extracted);
    }

    extracted_panels.sort_by(|a, b| {
        let sa = &a.document.stats;
        let sb = &b.document.stats;
        sb.resolved_texture_count
            .cmp(&sa.resolved_texture_count)
            .then_with(|| sb.active_graphic_count.cmp(&sa.active_graphic_count))
            .then_with(|| sb.graphic_count.cmp(&sa.graphic_count))
    });
    extracted_panels.truncate(opts.max_ui_panels);

    let mut used_ui_stems: HashSet<String> = manifest
        .ui_sprites
        .iter()
        .filter_map(|e| {
            Path::new(&e.asset_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(ToOwned::to_owned)
        })
        .collect();
    if opts.assets_only
        && manifest.ui_sprites.is_empty()
        && let Some(manifest_path) = ui_dir.parent().map(|p| p.join("manifest.json"))
        && let Ok(raw) = std::fs::read_to_string(manifest_path)
        && let Ok(existing) = serde_json::from_str::<BevyExportManifest>(&raw)
    {
        for e in existing.ui_sprites {
            if let Some(stem) = Path::new(&e.asset_path).file_stem().and_then(|s| s.to_str()) {
                used_ui_stems.insert(stem.to_owned());
            }
        }
    }
    let mut exported_tex_paths: HashMap<String, String> = HashMap::new();
    let mut used_panel_stems: HashSet<String> = HashSet::new();

    for mut extracted in extracted_panels {
        let doc = &mut extracted.document;
        for meta in &mut doc.textures {
            if let Some(existing_path) = exported_tex_paths.get(&meta.key) {
                meta.asset_path = Some(existing_path.clone());
                continue;
            }
            let Some(img) = extracted.images.get(&meta.key) else {
                continue;
            };
            let Ok(png) = encode_png(img) else {
                continue;
            };
            let stem = make_unique_stem(&mut used_ui_stems, &sanitize_stem(&meta.name));
            let fname = format!("{stem}.png");
            let out_png = ui_dir.join(&fname);
            let already_identical = std::fs::read(&out_png)
                .map(|existing| existing == png)
                .unwrap_or(false);
            if already_identical || std::fs::write(&out_png, &png).is_ok() {
                let rel = format!("ui/{fname}");
                meta.asset_path = Some(rel.clone());
                exported_tex_paths.insert(meta.key.clone(), rel);
            }
        }

        if doc.stats.missing_texture_count > 0 {
            warnings.push(format!(
                "UI panel `{}` ({}): {} resolved sprites, {} missing external sprite/atlas refs",
                doc.name,
                doc.source_bundle,
                doc.stats.resolved_texture_count,
                doc.stats.missing_texture_count,
            ));
        }

        let panel_stem = make_unique_stem(&mut used_panel_stems, &sanitize_stem(&doc.name));
        let panel_fname = format!("{panel_stem}.json");
        let panel_rel = format!("ui_panels/{panel_fname}");
        match serde_json::to_vec_pretty(doc) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(ui_panels_dir.join(&panel_fname), bytes) {
                    warnings.push(format!("ui_panel `{}` write error: {e}", doc.name));
                    continue;
                }
            }
            Err(e) => {
                warnings.push(format!("ui_panel `{}` serialize error: {e}", doc.name));
                continue;
            }
        }

        manifest.ui_panels.push(BevyUiPanelEntry {
            name: doc.name.clone(),
            asset_path: panel_rel,
            source_bundle: doc.source_bundle.clone(),
            reference_width: doc.reference_width,
            reference_height: doc.reference_height,
            node_count: doc.stats.node_count,
            graphic_count: doc.stats.graphic_count,
            text_count: doc.stats.text_count,
            resolved_texture_count: doc.stats.resolved_texture_count,
            missing_texture_count: doc.stats.missing_texture_count,
        });
    }
}

fn make_unique_stem(used: &mut HashSet<String>, base: &str) -> String {
    if used.insert(base.to_owned()) {
        return base.to_owned();
    }
    let mut n = 2usize;
    loop {
        let candidate = format!("{base}_{n}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

fn sanitize_package_name(raw: &str) -> String {
    let mut s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() || s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        s = format!("devx_{s}");
    }
    s
}

fn write_bevy_project_files(out_dir: &Path, manifest: &BevyExportManifest) -> ExportResult<()> {
    let src_dir = out_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let cargo_toml = format!(
        r#"[package]
name = "{}"
version = "0.1.0"
edition = "2024"

[dependencies]
bevy = {{ version = "0.20.1", features = ["wav", "symphonia-wav", "mp3", "vorbis"] }}
serde = {{ version = "1.0", features = ["derive"] }}
serde_json = "1.0"

[profile.dev.package."*"]
opt-level = 2
"#,
        manifest.project_name
    );
    std::fs::write(out_dir.join("Cargo.toml"), cargo_toml)?;

    let readme = format!(
        r#"# {} (Bevy 0.20.1 Zero-Logic Playable Sandbox)

Generated by **DevX Unity Rust** (`devx-exporter`) from Unity `{}`.
Strictly zero proprietary game logic — uses standard Bevy `0.20.1` third-person locomotion, animation state crossfading, socket attachment, and audio playback.

## Controls
- `F1` — Scene Explorer ({} scenes)
- `F2` — Battle Stage Arena Sandbox ({} characters + {} props)
- `F3` — Model & Animation Studio
- `Tab` / `C` — Cycle Control Mode (`PlayThirdPerson` -> `Orbit` -> `Free-Fly`)
- `WASD` (+ `Shift` Sprint) — Move Player (in `PlayThirdPerson`), or Pan / Fly Camera
- `Space` — Jump (in `PlayThirdPerson`)
- `F` — Action / Attack clip + SFX trigger (in `PlayThirdPerson`)
- `V` — Attach / Detach Weapon or Prop to Character Hand Socket
- `B` — Cycle Attached Weapon / Prop
- `[` / `]` — Previous / Next Scene
- `,` / `.` — Previous / Next Character or Model
- `N` — Next Animation Clip (`P` Pause/Resume)
- `M` — Play / Cycle Audio Track
- `G` — Toggle Axis & Floor Grid Gizmos
- `H` — Toggle HUD Overlay

## Run & Verify
```bash
cargo run --release
cargo test
```
"#,
        manifest.project_name,
        manifest.unity_version,
        manifest.scenes.len(),
        manifest.characters.len(),
        manifest.props.len()
    );
    std::fs::write(out_dir.join("README.md"), readme)?;
    std::fs::write(src_dir.join("main.rs"), BEVY_SHOWCASE_MAIN_RS)?;
    Ok(())
}

const BEVY_SHOWCASE_MAIN_RS: &str = r##"//! Bevy 0.20.1 Zero-Logic Playable Asset Sandbox & Battle Stage.
//!
//! Spawns extracted Unity scenes, rigged animated characters, weapon/prop models,
//! extracted fonts, UI sprites, and audio clips directly from `assets/manifest.json`
//! without any proprietary gameplay, damage, AI, or networking logic.

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::gltf::Gltf;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::light::GlobalAmbientLight;
use bevy::picking::hover::Hovered;
use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::text::{FontSize, FontSource, Justify};
use bevy::ui::widget::NodeImageMode;
use bevy::ui::{Pressed, UiTransform};
use bevy::ui_widgets::{observe, Activate, Button};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
struct GlbAssetEntry {
    name: String,
    asset_path: String,
    source_bundle: String,
    node_count: usize,
    mesh_count: usize,
    material_count: usize,
    texture_count: usize,
    skin_count: usize,
    animation_count: usize,
    triangle_count: usize,
    animation_names: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
struct MediaEntry {
    name: String,
    asset_path: String,
    kind: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
struct UiPanelEntry {
    name: String,
    asset_path: String,
    source_bundle: String,
    reference_width: f32,
    reference_height: f32,
    node_count: usize,
    graphic_count: usize,
    text_count: usize,
    #[serde(default)]
    resolved_texture_count: usize,
    #[serde(default)]
    missing_texture_count: usize,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default, Resource)]
struct AssetManifest {
    project_name: String,
    unity_version: String,
    scenes: Vec<GlbAssetEntry>,
    characters: Vec<GlbAssetEntry>,
    props: Vec<GlbAssetEntry>,
    audio: Vec<MediaEntry>,
    fonts: Vec<MediaEntry>,
    ui_sprites: Vec<MediaEntry>,
    #[serde(default)]
    ui_panels: Vec<UiPanelEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    SceneExplorer,
    BattleStage,
    ModelStudio,
    UiPanels,
}

impl ViewMode {
    fn label(self) -> &'static str {
        match self {
            Self::SceneExplorer => "1: Scene Explorer",
            Self::BattleStage => "2: Battle Stage",
            Self::ModelStudio => "3: Model & Anim Studio",
            Self::UiPanels => "4: UI Panels",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlMode {
    PlayThirdPerson,
    Orbit,
    FreeFly,
}

impl ControlMode {
    fn label(self) -> &'static str {
        match self {
            Self::PlayThirdPerson => "Play (WASD+Jump+F)",
            Self::Orbit => "Orbit",
            Self::FreeFly => "Free-Fly",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::PlayThirdPerson => Self::Orbit,
            Self::Orbit => Self::FreeFly,
            Self::FreeFly => Self::PlayThirdPerson,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimState {
    Idle,
    Walk,
    Run,
    Jump,
    Attack,
}

impl AnimState {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Walk => "Walk",
            Self::Run => "Run",
            Self::Jump => "Jump",
            Self::Attack => "Attack",
        }
    }
}

#[derive(Resource)]
struct ShowcaseState {
    mode: ViewMode,
    control_mode: ControlMode,
    scene_index: usize,
    model_index: usize,
    prop_index: usize,
    prop_attached: bool,
    clip_index: usize,
    audio_index: usize,
    anim_paused: bool,
    show_gizmos: bool,
    show_hud: bool,
    dirty_rebuild: bool,
    dirty_clip_switch: bool,
    dirty_prop_attach: bool,
    stage_gen: u64,
    ui_panel_index: usize,
    active_ui_panel_name: String,
    last_ui_click_label: String,
    font: Option<Handle<Font>>,
    emblem: Option<Handle<Image>>,
}

impl Default for ShowcaseState {
    fn default() -> Self {
        Self {
            mode: ViewMode::BattleStage,
            control_mode: ControlMode::PlayThirdPerson,
            scene_index: 0,
            model_index: 0,
            prop_index: 0,
            prop_attached: true,
            clip_index: 0,
            audio_index: 0,
            anim_paused: false,
            show_gizmos: true,
            show_hud: true,
            dirty_rebuild: true,
            dirty_clip_switch: false,
            dirty_prop_attach: false,
            stage_gen: 0,
            ui_panel_index: 0,
            active_ui_panel_name: String::new(),
            last_ui_click_label: String::new(),
            font: None,
            emblem: None,
        }
    }
}

#[derive(Component)]
struct StageContent;

#[derive(Component)]
struct StageActorGltf {
    label: String,
    gltf: Handle<Gltf>,
    phase_offset_secs: f32,
}

#[derive(Component)]
struct PlayerCharacter {
    stage_gen: u64,
    velocity: Vec2,
    vertical_vel: f32,
    grounded: bool,
    attack_timer: f32,
    anim_state: AnimState,
    last_applied_state: Option<AnimState>,
    manual_override: bool,
}

impl Default for PlayerCharacter {
    fn default() -> Self {
        Self {
            stage_gen: 0,
            velocity: Vec2::ZERO,
            vertical_vel: 0.0,
            grounded: true,
            attack_timer: 0.0,
            anim_state: AnimState::Idle,
            last_applied_state: None,
            manual_override: false,
        }
    }
}

#[derive(Component)]
struct PartnerCharacter;

#[derive(Component)]
struct AttachedProp;

#[derive(Component)]
struct PropSocketSynced;

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum BoundActorRole {
    Player,
    Other,
}

#[derive(Component)]
struct BoundAnimationSet {
    role: BoundActorRole,
    nodes: Vec<(String, AnimationNodeIndex)>,
}

#[derive(Component)]
struct OrbitRig {
    focus: Vec3,
    yaw: f32,
    pitch: f32,
    radius: f32,
}

#[derive(Component)]
struct ShowcaseAudio;

#[derive(Component)]
struct HudRoot;

#[derive(Component)]
struct HudButton;

#[derive(Component)]
struct HudStatusText;

#[derive(Component)]
struct UiPanelRoot;

#[derive(Component)]
struct UiPanelFrame {
    ref_w: f32,
    ref_h: f32,
}

#[derive(Component)]
struct UiPanelScaledText {
    base_px: f32,
}

#[derive(Component)]
struct UiPanelInteractiveLabel(String);

#[derive(Component, Clone, Copy)]
enum HudAction {
    SetMode(ViewMode),
    PrevScene,
    NextScene,
    PrevModel,
    NextModel,
    PrevUiPanel,
    NextUiPanel,
    ToggleAttachProp,
    NextAttachProp,
    TriggerAction,
    NextClip,
    TogglePause,
    ToggleControlMode,
    PlayNextAudio,
    ToggleGizmos,
}

fn main() {
    let manifest = load_manifest_from_disk().unwrap_or_else(|e| {
        warn!("Failed to read assets/manifest.json: {e}");
        AssetManifest::default()
    });

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: format!("{} — Bevy 0.20 Zero-Logic Sandbox", manifest.project_name),
                resolution: (1440, 900).into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(manifest)
        .insert_resource(GlobalAmbientLight {
            color: Color::srgb(0.88, 0.92, 1.0),
            brightness: 550.0,
            ..default()
        })
        .init_resource::<ShowcaseState>()
        .add_systems(Startup, setup_stage_and_hud)
        .add_systems(
            Update,
            (
                handle_keyboard_shortcuts,
                ApplyDeferred,
                rebuild_stage_if_dirty,
                ApplyDeferred,
                bind_loaded_gltf_animations,
                third_person_player_movement_system,
                sync_attached_prop_socket_system,
                update_animation_controls,
                camera_controller_system,
                draw_stage_gizmos,
                resize_ui_panel_frames,
                style_hud_buttons,
                update_hud_status_text,
            )
                .chain(),
        )
        .run();
}

fn load_manifest_from_disk() -> Result<AssetManifest, String> {
    let path = std::path::Path::new("assets/manifest.json");
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| e.to_string())
}

fn combined_models(manifest: &AssetManifest) -> Vec<&GlbAssetEntry> {
    manifest
        .characters
        .iter()
        .chain(manifest.props.iter())
        .chain(manifest.scenes.iter())
        .collect()
}

fn setup_stage_and_hud(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    manifest: Res<AssetManifest>,
    mut state: ResMut<ShowcaseState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if let Some(first_font) = manifest.fonts.first() {
        state.font = Some(asset_server.load(first_font.asset_path.clone()));
    }
    if let Some(first_ui) = manifest.ui_sprites.first() {
        state.emblem = Some(asset_server.load(first_ui.asset_path.clone()));
    }
    if manifest.characters.is_empty() && !manifest.scenes.is_empty() {
        state.mode = ViewMode::SceneExplorer;
        state.control_mode = ControlMode::Orbit;
    } else if manifest.scenes.is_empty() && manifest.characters.is_empty() && !manifest.props.is_empty()
    {
        state.mode = ViewMode::ModelStudio;
        state.control_mode = ControlMode::Orbit;
    } else if manifest.scenes.is_empty()
        && manifest.characters.is_empty()
        && manifest.props.is_empty()
        && !manifest.ui_panels.is_empty()
    {
        state.mode = ViewMode::UiPanels;
        state.control_mode = ControlMode::Orbit;
    }

    let initial_rig = OrbitRig {
        focus: Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.45,
        pitch: 0.35,
        radius: 9.0,
    };
    commands.spawn((
        Camera3d::default(),
        Tonemapping::TonyMcMapface,
        Transform::from_xyz(4.0, 4.5, 8.5).looking_at(initial_rig.focus, Vec3::Y),
        initial_rig,
    ));

    // Key directional sunlight with soft shadows + rim fill light.
    commands.spawn((
        DirectionalLight {
            illuminance: 14_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(12.0, 18.0, 10.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        PointLight {
            color: Color::srgb(0.45, 0.70, 1.0),
            intensity: 350_000.0,
            range: 45.0,
            ..default()
        },
        Transform::from_xyz(-7.0, 5.0, -7.0),
    ));

    // Clean arena plinth floor.
    let floor_mesh = meshes.add(Cylinder::new(14.0, 0.25));
    let floor_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.14, 0.16, 0.20),
        perceptual_roughness: 0.78,
        metallic: 0.12,
        ..default()
    });
    commands.spawn((
        Name::new("StageFloor"),
        Mesh3d(floor_mesh),
        MeshMaterial3d(floor_mat),
        Transform::from_xyz(0.0, -0.13, 0.0),
    ));

    spawn_hud(&mut commands, &manifest, &state);
}

fn spawn_hud(commands: &mut Commands, manifest: &AssetManifest, state: &ShowcaseState) {
    let font_source = state
        .font
        .clone()
        .map(FontSource::from)
        .unwrap_or_else(FontSource::sans_serif);

    commands
        .spawn((
            HudRoot,
            ZIndex(20),
            Pickable::IGNORE,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::all(Val::Px(14.0)),
                ..default()
            },
        ))
        .with_children(|root| {
            // Top bar with emblem + mode switcher + sandbox controls.
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    flex_wrap: FlexWrap::Wrap,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    row_gap: Val::Px(6.0),
                    padding: UiRect::all(Val::Px(10.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.07, 0.09, 0.13, 0.88)),
                BorderColor::all(Color::srgba(0.32, 0.45, 0.65, 0.7)),
            ))
            .with_children(|bar| {
                if let Some(emblem) = &state.emblem {
                    bar.spawn((
                        ImageNode::new(emblem.clone()),
                        Node {
                            width: Val::Px(34.0),
                            height: Val::Px(34.0),
                            ..default()
                        },
                    ));
                }
                bar.spawn((
                    Text::new(format!("{} | Bevy 0.20 Sandbox", manifest.project_name)),
                    TextFont {
                        font: font_source.clone(),
                        font_size: FontSize::Px(16.0),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                ));

                spawn_hud_button(bar, "Scene [F1]", HudAction::SetMode(ViewMode::SceneExplorer), &font_source);
                spawn_hud_button(bar, "Battle Stage [F2]", HudAction::SetMode(ViewMode::BattleStage), &font_source);
                spawn_hud_button(bar, "Model Studio [F3]", HudAction::SetMode(ViewMode::ModelStudio), &font_source);
                spawn_hud_button(bar, "UI Panels [F4]", HudAction::SetMode(ViewMode::UiPanels), &font_source);
                spawn_hud_button(bar, "Mode [Tab/C]", HudAction::ToggleControlMode, &font_source);
                spawn_hud_button(bar, "< Scene", HudAction::PrevScene, &font_source);
                spawn_hud_button(bar, "Scene >", HudAction::NextScene, &font_source);
                spawn_hud_button(bar, "< Hero", HudAction::PrevModel, &font_source);
                spawn_hud_button(bar, "Hero >", HudAction::NextModel, &font_source);
                spawn_hud_button(bar, "< UI [U]", HudAction::PrevUiPanel, &font_source);
                spawn_hud_button(bar, "UI > [I]", HudAction::NextUiPanel, &font_source);
                spawn_hud_button(bar, "Socket Prop [V]", HudAction::ToggleAttachProp, &font_source);
                spawn_hud_button(bar, "Prop > [B]", HudAction::NextAttachProp, &font_source);
                spawn_hud_button(bar, "Action [F]", HudAction::TriggerAction, &font_source);
                spawn_hud_button(bar, "Anim [N]", HudAction::NextClip, &font_source);
                spawn_hud_button(bar, "Pause [P]", HudAction::TogglePause, &font_source);
                spawn_hud_button(bar, "SFX [M]", HudAction::PlayNextAudio, &font_source);
                spawn_hud_button(bar, "Grid [G]", HudAction::ToggleGizmos, &font_source);
            });

            // Bottom telemetry card.
            root.spawn((
                Node {
                    padding: UiRect::all(Val::Px(10.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    align_self: AlignSelf::FlexStart,
                    ..default()
                },
                BackgroundColor(Color::srgba(0.07, 0.09, 0.13, 0.85)),
                BorderColor::all(Color::srgba(0.32, 0.45, 0.65, 0.65)),
            ))
            .with_children(|panel| {
                panel.spawn((
                    HudStatusText,
                    Text::new("Loading assets..."),
                    TextFont {
                        font: font_source,
                        font_size: FontSize::Px(14.0),
                        ..default()
                    },
                    TextColor(Color::srgb(0.88, 0.93, 0.98)),
                ));
            });
        });
}

fn spawn_hud_button(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    action: HudAction,
    font: &FontSource,
) {
    parent
        .spawn((
            Button,
            HudButton,
            Hovered::default(),
            action,
            Node {
                padding: UiRect::axes(Val::Px(9.0), Val::Px(5.0)),
                border: UiRect::all(Val::Px(1.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgb(0.16, 0.21, 0.30)),
            BorderColor::all(Color::srgb(0.35, 0.48, 0.68)),
            observe(
                |activate: On<Activate>,
                 actions: Query<&HudAction>,
                 mut state: ResMut<ShowcaseState>,
                 manifest: Res<AssetManifest>,
                 asset_server: Res<AssetServer>,
                 existing_audio: Query<Entity, With<ShowcaseAudio>>,
                 mut player_q: Query<&mut PlayerCharacter>,
                 mut commands: Commands| {
                    let Ok(&act) = actions.get(activate.entity) else {
                        return;
                    };
                    apply_hud_action(
                        act,
                        &mut state,
                        &manifest,
                        &asset_server,
                        &existing_audio,
                        &mut player_q,
                        &mut commands,
                    );
                },
            ),
        ))
        .with_children(|btn| {
            btn.spawn((
                Text::new(label.to_owned()),
                TextFont {
                    font: font.clone(),
                    font_size: FontSize::Px(13.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
        });
}

fn spawn_next_audio_sfx(
    state: &mut ShowcaseState,
    manifest: &AssetManifest,
    asset_server: &AssetServer,
    existing_audio: &Query<Entity, With<ShowcaseAudio>>,
    commands: &mut Commands,
) {
    if manifest.audio.is_empty() {
        return;
    }
    for e in existing_audio.iter() {
        commands.entity(e).try_despawn();
    }
    let track = &manifest.audio[state.audio_index % manifest.audio.len()];
    state.audio_index = (state.audio_index + 1) % manifest.audio.len();
    commands.spawn((
        ShowcaseAudio,
        AudioPlayer::new(asset_server.load(track.asset_path.clone())),
        PlaybackSettings::DESPAWN,
    ));
}

fn apply_hud_action(
    action: HudAction,
    state: &mut ShowcaseState,
    manifest: &AssetManifest,
    asset_server: &AssetServer,
    existing_audio: &Query<Entity, With<ShowcaseAudio>>,
    player_q: &mut Query<&mut PlayerCharacter>,
    commands: &mut Commands,
) {
    let models = combined_models(manifest);
    match action {
        HudAction::SetMode(m) => {
            state.mode = m;
            state.dirty_rebuild = true;
        }
        HudAction::PrevScene if !manifest.scenes.is_empty() => {
            state.scene_index =
                (state.scene_index + manifest.scenes.len() - 1) % manifest.scenes.len();
            state.dirty_rebuild = true;
        }
        HudAction::NextScene if !manifest.scenes.is_empty() => {
            state.scene_index = (state.scene_index + 1) % manifest.scenes.len();
            state.dirty_rebuild = true;
        }
        HudAction::PrevModel if !models.is_empty() => {
            state.model_index = (state.model_index + models.len() - 1) % models.len();
            state.clip_index = 0;
            state.dirty_rebuild = true;
        }
        HudAction::NextModel if !models.is_empty() => {
            state.model_index = (state.model_index + 1) % models.len();
            state.clip_index = 0;
            state.dirty_rebuild = true;
        }
        HudAction::PrevUiPanel if !manifest.ui_panels.is_empty() => {
            state.ui_panel_index =
                (state.ui_panel_index + manifest.ui_panels.len() - 1) % manifest.ui_panels.len();
            state.mode = ViewMode::UiPanels;
            state.dirty_rebuild = true;
        }
        HudAction::NextUiPanel if !manifest.ui_panels.is_empty() => {
            state.ui_panel_index = (state.ui_panel_index + 1) % manifest.ui_panels.len();
            state.mode = ViewMode::UiPanels;
            state.dirty_rebuild = true;
        }
        HudAction::ToggleAttachProp => {
            state.prop_attached = !state.prop_attached;
            state.dirty_prop_attach = true;
        }
        HudAction::NextAttachProp if !manifest.props.is_empty() => {
            state.prop_index = (state.prop_index + 1) % manifest.props.len();
            state.prop_attached = true;
            state.dirty_prop_attach = true;
        }
        HudAction::TriggerAction => {
            for mut p in player_q.iter_mut() {
                p.attack_timer = 0.55;
                p.manual_override = false;
            }
            spawn_next_audio_sfx(state, manifest, asset_server, existing_audio, commands);
        }
        HudAction::NextClip => {
            state.clip_index = state.clip_index.wrapping_add(1);
            state.dirty_clip_switch = true;
            for mut p in player_q.iter_mut() {
                p.manual_override = true;
            }
        }
        HudAction::TogglePause => {
            state.anim_paused = !state.anim_paused;
            state.dirty_clip_switch = true;
        }
        HudAction::ToggleControlMode => {
            state.control_mode = state.control_mode.next();
        }
        HudAction::PlayNextAudio => {
            spawn_next_audio_sfx(state, manifest, asset_server, existing_audio, commands);
        }
        HudAction::ToggleGizmos => state.show_gizmos = !state.show_gizmos,
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_keyboard_shortcuts(
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<ShowcaseState>,
    manifest: Res<AssetManifest>,
    asset_server: Res<AssetServer>,
    existing_audio: Query<Entity, With<ShowcaseAudio>>,
    mut player_q: Query<&mut PlayerCharacter>,
    mut hud_vis: Query<&mut Visibility, With<HudRoot>>,
    mut commands: Commands,
) {
    if keys.just_pressed(KeyCode::F1) {
        apply_hud_action(HudAction::SetMode(ViewMode::SceneExplorer), &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    } else if keys.just_pressed(KeyCode::F2) {
        apply_hud_action(HudAction::SetMode(ViewMode::BattleStage), &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    } else if keys.just_pressed(KeyCode::F3) {
        apply_hud_action(HudAction::SetMode(ViewMode::ModelStudio), &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    } else if keys.just_pressed(KeyCode::F4) {
        apply_hud_action(HudAction::SetMode(ViewMode::UiPanels), &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyU) {
        apply_hud_action(HudAction::PrevUiPanel, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    } else if keys.just_pressed(KeyCode::KeyI) {
        apply_hud_action(HudAction::NextUiPanel, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    }
    if keys.just_pressed(KeyCode::BracketLeft) {
        apply_hud_action(HudAction::PrevScene, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    } else if keys.just_pressed(KeyCode::BracketRight) {
        apply_hud_action(HudAction::NextScene, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    }
    if keys.just_pressed(KeyCode::Comma) {
        apply_hud_action(HudAction::PrevModel, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    } else if keys.just_pressed(KeyCode::Period) {
        apply_hud_action(HudAction::NextModel, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyV) {
        apply_hud_action(HudAction::ToggleAttachProp, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyB) {
        apply_hud_action(HudAction::NextAttachProp, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyF) {
        apply_hud_action(HudAction::TriggerAction, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyN) {
        apply_hud_action(HudAction::NextClip, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyP) {
        apply_hud_action(HudAction::TogglePause, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyC) || keys.just_pressed(KeyCode::Tab) {
        apply_hud_action(HudAction::ToggleControlMode, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyM) {
        apply_hud_action(HudAction::PlayNextAudio, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyG) {
        apply_hud_action(HudAction::ToggleGizmos, &mut state, &manifest, &asset_server, &existing_audio, &mut player_q, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyH) {
        state.show_hud = !state.show_hud;
        for mut v in &mut hud_vis {
            *v = if state.show_hud { Visibility::Visible } else { Visibility::Hidden };
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rebuild_stage_if_dirty(
    mut commands: Commands,
    mut state: ResMut<ShowcaseState>,
    manifest: Res<AssetManifest>,
    asset_server: Res<AssetServer>,
    existing_stage: Query<Entity, (With<StageContent>, Without<ChildOf>)>,
    existing_ui_panels: Query<Entity, (With<UiPanelRoot>, Without<ChildOf>)>,
    windows: Query<&Window>,
    mut orbit_q: Query<&mut OrbitRig>,
) {
    if !state.dirty_rebuild {
        return;
    }
    state.dirty_rebuild = false;
    state.stage_gen = state.stage_gen.wrapping_add(1);
    state.dirty_prop_attach = true;

    for e in existing_stage.iter() {
        commands.entity(e).try_despawn();
    }
    for e in existing_ui_panels.iter() {
        commands.entity(e).try_despawn();
    }

    let spawn_glb = |commands: &mut Commands,
                     asset_server: &AssetServer,
                     entry: &GlbAssetEntry,
                     transform: Transform,
                     phase: f32|
     -> Entity {
        let scene_handle: Handle<WorldAsset> =
            asset_server.load(format!("{}#Scene0", entry.asset_path));
        let gltf_handle: Handle<Gltf> = asset_server.load(entry.asset_path.clone());
        commands
            .spawn((
                StageContent,
                Name::new(entry.name.clone()),
                WorldAssetRoot(scene_handle),
                StageActorGltf {
                    label: entry.name.clone(),
                    gltf: gltf_handle,
                    phase_offset_secs: phase,
                },
                transform,
            ))
            .id()
    };

    match state.mode {
        ViewMode::SceneExplorer => {
            if let Some(scene) = manifest.scenes.get(state.scene_index % manifest.scenes.len().max(1))
            {
                spawn_glb(
                    &mut commands,
                    &asset_server,
                    scene,
                    Transform::IDENTITY,
                    0.0,
                );
            }
            if let Ok(mut rig) = orbit_q.single_mut() {
                rig.focus = Vec3::new(0.0, 2.0, 0.0);
                rig.radius = 18.0;
            }
        }
        ViewMode::BattleStage => {
            // Optional backdrop scene placed behind the combat showcase pad.
            if !manifest.scenes.is_empty() {
                let scene = &manifest.scenes[state.scene_index % manifest.scenes.len()];
                spawn_glb(
                    &mut commands,
                    &asset_server,
                    scene,
                    Transform::from_xyz(0.0, 0.0, -8.0),
                    0.0,
                );
            }
            // Spawn playable hero + arena partner from extracted characters (zero combat logic).
            if !manifest.characters.is_empty() {
                let count = manifest.characters.len();
                let hero = &manifest.characters[state.model_index % count];
                let hero_entity = spawn_glb(
                    &mut commands,
                    &asset_server,
                    hero,
                    Transform::from_xyz(-2.2, 0.0, 0.0)
                        .looking_at(Vec3::new(2.2, 0.0, 0.0), Vec3::Y),
                    0.0,
                );
                commands.entity(hero_entity).insert(PlayerCharacter {
                    stage_gen: state.stage_gen,
                    ..default()
                });

                if count > 1 {
                    let partner = &manifest.characters[(state.model_index + 1) % count];
                    let partner_entity = spawn_glb(
                        &mut commands,
                        &asset_server,
                        partner,
                        Transform::from_xyz(2.2, 0.0, 0.0)
                            .looking_at(Vec3::new(-2.2, 0.0, 0.0), Vec3::Y),
                        0.35,
                    );
                    commands.entity(partner_entity).insert(PartnerCharacter);
                }
            }
            // Spawn extracted weapon/prop models as display plinth props around the arena edge.
            for (i, prop) in manifest.props.iter().take(4).enumerate() {
                let x = -3.0 + (i as f32) * 2.0;
                spawn_glb(
                    &mut commands,
                    &asset_server,
                    prop,
                    Transform::from_xyz(x, 0.4, 3.2),
                    0.0,
                );
            }
            if let Ok(mut rig) = orbit_q.single_mut() {
                rig.focus = Vec3::new(-2.2, 1.15, 0.0);
                rig.radius = 6.2;
            }
        }
        ViewMode::ModelStudio => {
            let models = combined_models(&manifest);
            if !models.is_empty() {
                let entry = models[state.model_index % models.len()];
                let e = spawn_glb(
                    &mut commands,
                    &asset_server,
                    entry,
                    Transform::IDENTITY,
                    0.0,
                );
                if entry.skin_count > 0 {
                    commands.entity(e).insert(PlayerCharacter {
                        stage_gen: state.stage_gen,
                        ..default()
                    });
                }
            }
            if let Ok(mut rig) = orbit_q.single_mut() {
                rig.focus = Vec3::new(0.0, 1.0, 0.0);
                rig.radius = 4.5;
            }
        }
        ViewMode::UiPanels => {
            let win_size = windows
                .iter()
                .next()
                .map(|w| Vec2::new(w.width(), w.height()))
                .unwrap_or(Vec2::new(1440.0, 900.0));
            spawn_active_ui_panel(&mut commands, &manifest, &mut state, &asset_server, win_size);
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
struct UiPanelDoc {
    name: String,
    reference_width: f32,
    reference_height: f32,
    root: UiPanelNodeDto,
    #[serde(default)]
    textures: Vec<UiTextureDto>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
struct UiTextureDto {
    key: String,
    #[serde(default)]
    asset_path: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
struct UiPanelNodeDto {
    name: String,
    #[serde(default)]
    active_in_hierarchy: bool,
    #[serde(default)]
    canvas_rect: [f32; 4],
    #[serde(default)]
    canvas_render_rect: Option<[f32; 4]>,
    #[serde(default)]
    canvas_rotation_z_deg: Option<f32>,
    #[serde(default)]
    rotation_z_deg: f32,
    #[serde(default)]
    flip_x: bool,
    #[serde(default)]
    flip_y: bool,
    #[serde(default = "default_one")]
    effective_alpha: f32,
    #[serde(default)]
    clips_children: bool,
    #[serde(default = "default_true")]
    show_mask_graphic: bool,
    #[serde(default = "default_true")]
    graphic_enabled: bool,
    #[serde(default)]
    is_interactive: bool,
    #[serde(default)]
    widget_kind: Option<String>,
    #[serde(default)]
    graphic: Option<UiPanelGraphicDto>,
    #[serde(default)]
    children: Vec<UiPanelNodeDto>,
}

fn default_one() -> f32 {
    1.0
}

fn default_true() -> bool {
    true
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum UiPanelGraphicDto {
    Image {
        color: [f32; 4],
        #[serde(default)]
        texture_key: Option<String>,
        #[serde(default)]
        missing_texture: bool,
        #[serde(default)]
        image_type: String,
        #[serde(default)]
        border: [f32; 4],
        #[serde(default)]
        raw_name: Option<String>,
    },
    RawImage {
        color: [f32; 4],
        #[serde(default)]
        texture_key: Option<String>,
        #[serde(default)]
        missing_texture: bool,
        #[serde(default)]
        raw_name: Option<String>,
    },
    Text {
        text: String,
        color: [f32; 4],
        font_size: f32,
        #[serde(default)]
        alignment: String,
        #[serde(default)]
        rich_text: bool,
    },
}

fn compute_panel_fit_scale(ref_w: f32, ref_h: f32, win_size: Vec2) -> f32 {
    let avail_w = (win_size.x - 40.0).max(200.0);
    let avail_h = (win_size.y - 170.0).max(160.0);
    (avail_w / ref_w.max(1.0))
        .min(avail_h / ref_h.max(1.0))
        .clamp(0.1, 2.5)
}

fn spawn_active_ui_panel(
    commands: &mut Commands,
    manifest: &AssetManifest,
    state: &mut ShowcaseState,
    asset_server: &AssetServer,
    win_size: Vec2,
) {
    let font_source = state
        .font
        .clone()
        .map(FontSource::from)
        .unwrap_or_else(FontSource::sans_serif);

    if manifest.ui_panels.is_empty() {
        state.active_ui_panel_name = "None".to_owned();
        commands
            .spawn((
                UiPanelRoot,
                ZIndex(5),
                Pickable::IGNORE,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(14.0),
                    right: Val::Px(14.0),
                    top: Val::Px(88.0),
                    bottom: Val::Px(64.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
            ))
            .with_children(|root| {
                root.spawn((
                    Text::new("No UI panels exported in assets/ui_panels/. Re-export with --ui-panel or increase Max UI."),
                    TextFont {
                        font: font_source.clone(),
                        font_size: FontSize::Px(18.0),
                        ..default()
                    },
                    TextColor(Color::srgb(0.85, 0.88, 0.94)),
                ));
            });
        return;
    }

    let entry = &manifest.ui_panels[state.ui_panel_index % manifest.ui_panels.len()];
    state.active_ui_panel_name = entry.name.clone();
    let json_path = std::path::Path::new("assets").join(&entry.asset_path);
    let Ok(raw_json) = std::fs::read_to_string(&json_path) else {
        state.active_ui_panel_name = format!("{} (Missing JSON)", entry.name);
        return;
    };
    let Ok(doc) = serde_json::from_str::<UiPanelDoc>(&raw_json) else {
        state.active_ui_panel_name = format!("{} (Parse error)", entry.name);
        return;
    };

    let mut tex_handles: HashMap<String, Handle<Image>> = HashMap::new();
    for tex in &doc.textures {
        if let Some(ref rel_path) = tex.asset_path {
            tex_handles.insert(tex.key.clone(), asset_server.load(rel_path.clone()));
        }
    }

    let ref_w = doc.reference_width.max(64.0);
    let ref_h = doc.reference_height.max(64.0);
    let fit_scale = compute_panel_fit_scale(ref_w, ref_h, win_size);

    commands
        .spawn((
            UiPanelRoot,
            ZIndex(5),
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(14.0),
                right: Val::Px(14.0),
                top: Val::Px(88.0),
                bottom: Val::Px(64.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
        ))
        .with_children(|root| {
            root.spawn((
                UiPanelFrame { ref_w, ref_h },
                Pickable::IGNORE,
                Node {
                    position_type: PositionType::Relative,
                    width: Val::Px(ref_w * fit_scale),
                    height: Val::Px(ref_h * fit_scale),
                    overflow: Overflow::clip(),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.08, 0.10, 0.14, 0.90)),
                BorderColor::all(Color::srgba(0.35, 0.52, 0.78, 0.85)),
            ))
            .with_children(|frame| {
                spawn_ui_panel_nodes_depth_first(
                    frame,
                    &doc.root,
                    ref_w,
                    ref_h,
                    fit_scale,
                    &tex_handles,
                    &font_source,
                );
            });
        });
}

fn spawn_ui_panel_nodes_depth_first(
    frame: &mut ChildSpawnerCommands,
    node: &UiPanelNodeDto,
    ref_w: f32,
    ref_h: f32,
    fit_scale: f32,
    tex_handles: &HashMap<String, Handle<Image>>,
    font_source: &FontSource,
) {
    if !node.active_in_hierarchy {
        return;
    }

    let [_, _, rw, rh] = node.canvas_rect;
    let has_rect = rw >= 1.0 && rh >= 1.0;
    let draw_graphic = node.graphic_enabled && (!node.clips_children || node.show_mask_graphic);

    if has_rect && (draw_graphic && node.graphic.is_some() || node.is_interactive) {
        if let Some(ref g) = node.graphic.as_ref().filter(|_| draw_graphic) {
            match g {
                UiPanelGraphicDto::Image {
                    color,
                    texture_key,
                    missing_texture,
                    image_type,
                    border,
                    ..
                } => {
                    let alpha = (color[3] * node.effective_alpha).clamp(0.0, 1.0);
                    if alpha > 0.004 {
                        let tint = Color::srgba(color[0], color[1], color[2], alpha);
                        let tex_handle =
                            texture_key.as_deref().and_then(|k| tex_handles.get(k)).cloned();
                        if let Some(h) = tex_handle {
                            let mut img_node = ImageNode::new(h).with_color(tint);
                            img_node.flip_x = node.flip_x;
                            img_node.flip_y = node.flip_y;
                            if image_type == "sliced" && border.iter().any(|&b| b > 0.5) {
                                img_node.image_mode = NodeImageMode::Sliced(TextureSlicer {
                                    border: BorderRect {
                                        min_inset: Vec2::new(border[0], border[1]),
                                        max_inset: Vec2::new(border[2], border[3]),
                                    },
                                    ..default()
                                });
                            }
                            spawn_ui_element(
                                frame,
                                node,
                                ref_w,
                                ref_h,
                                Some(img_node),
                                None,
                            );
                        } else if *missing_texture {
                            spawn_ui_element(
                                frame,
                                node,
                                ref_w,
                                ref_h,
                                None,
                                Some(Color::srgba(color[0], color[1], color[2], (alpha * 0.14).max(0.04))),
                            );
                        } else if color[0] > 0.90 && color[1] > 0.90 && color[2] > 0.90 && rw > 48.0 && rh > 48.0 {
                            spawn_ui_element(
                                frame,
                                node,
                                ref_w,
                                ref_h,
                                None,
                                None,
                            );
                        } else {
                            spawn_ui_element(
                                frame,
                                node,
                                ref_w,
                                ref_h,
                                None,
                                Some(tint),
                            );
                        }
                    }
                }
                UiPanelGraphicDto::RawImage {
                    color,
                    texture_key,
                    missing_texture,
                    ..
                } => {
                    let alpha = (color[3] * node.effective_alpha).clamp(0.0, 1.0);
                    if alpha > 0.004 {
                        let tint = Color::srgba(color[0], color[1], color[2], alpha);
                        let tex_handle =
                            texture_key.as_deref().and_then(|k| tex_handles.get(k)).cloned();
                        if let Some(h) = tex_handle {
                            let mut img_node = ImageNode::new(h).with_color(tint);
                            img_node.flip_x = node.flip_x;
                            img_node.flip_y = node.flip_y;
                            spawn_ui_element(
                                frame,
                                node,
                                ref_w,
                                ref_h,
                                Some(img_node),
                                None,
                            );
                        } else if *missing_texture {
                            spawn_ui_element(
                                frame,
                                node,
                                ref_w,
                                ref_h,
                                None,
                                Some(Color::srgba(color[0], color[1], color[2], (alpha * 0.14).max(0.04))),
                            );
                        } else if color[0] > 0.90 && color[1] > 0.90 && color[2] > 0.90 && rw > 48.0 && rh > 48.0 {
                            spawn_ui_element(
                                frame,
                                node,
                                ref_w,
                                ref_h,
                                None,
                                None,
                            );
                        } else {
                            spawn_ui_element(
                                frame,
                                node,
                                ref_w,
                                ref_h,
                                None,
                                Some(tint),
                            );
                        }
                    }
                }
                UiPanelGraphicDto::Text {
                    text,
                    color,
                    font_size,
                    alignment,
                    rich_text,
                } => {
                    let alpha = (color[3] * node.effective_alpha).clamp(0.0, 1.0);
                    if alpha > 0.004 && !text.is_empty() {
                        let clean = if *rich_text && text.contains('<') {
                            strip_rich_text_tags(text)
                        } else {
                            text.clone()
                        };
                        if !clean.is_empty() {
                            let justify = match alignment.as_str() {
                                "upper_center" | "middle_center" | "lower_center" => {
                                    Justify::Center
                                }
                                "upper_right" | "middle_right" | "lower_right" => Justify::Right,
                                _ => Justify::Left,
                            };
                            frame.spawn((
                                Pickable::IGNORE,
                                ui_panel_layout(
                                    node.canvas_render_rect.unwrap_or(node.canvas_rect),
                                    [0.0, 0.0, ref_w, ref_h],
                                ),
                                ui_panel_artwork_transform(node),
                                UiPanelScaledText {
                                    base_px: (*font_size).max(8.0),
                                },
                                Text::new(clean),
                                TextFont {
                                    font: font_source.clone(),
                                    font_size: FontSize::Px((*font_size * fit_scale).clamp(7.0, 128.0)),
                                    ..default()
                                },
                                TextLayout::justify(justify),
                                TextColor(Color::srgba(color[0], color[1], color[2], alpha)),
                            ));
                        }
                    }
                }
            }
        } else if node.is_interactive {
            spawn_ui_element(
                frame,
                node,
                ref_w,
                ref_h,
                None,
                Some(Color::srgba(0.2, 0.65, 0.9, 0.08)),
            );
        }
    }

    for child in &node.children {
        spawn_ui_panel_nodes_depth_first(
            frame,
            child,
            ref_w,
            ref_h,
            fit_scale,
            tex_handles,
            font_source,
        );
    }
}

fn ui_panel_layout(rect: [f32; 4], parent_rect: [f32; 4]) -> Node {
    let [x, y, w, h] = rect;
    let [px, py, pw, ph] = parent_rect;
    Node {
        position_type: PositionType::Absolute,
        left: Val::Percent(((x - px) / pw.max(1.0)) * 100.0),
        top: Val::Percent(((y - py) / ph.max(1.0)) * 100.0),
        width: Val::Percent((w / pw.max(1.0)) * 100.0),
        height: Val::Percent((h / ph.max(1.0)) * 100.0),
        overflow: Overflow::clip(),
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        ..default()
    }
}

fn ui_panel_artwork_transform(node: &UiPanelNodeDto) -> UiTransform {
    // Exported canvas rotation already includes ancestors. The viewer flattens descendants
    // onto the frame, so apply it once to artwork rather than rotating logical containers.
    UiTransform::from_rotation(Rot2::degrees(
        node.canvas_rotation_z_deg.unwrap_or(node.rotation_z_deg),
    ))
}

fn spawn_ui_element(
    frame: &mut ChildSpawnerCommands,
    node: &UiPanelNodeDto,
    ref_w: f32,
    ref_h: f32,
    image_node: Option<ImageNode>,
    bg_color: Option<Color>,
) {
    let canvas = [0.0, 0.0, ref_w, ref_h];
    let artwork_rect = node.canvas_render_rect.unwrap_or(node.canvas_rect);
    let mut layout = ui_panel_layout(
        if node.is_interactive { node.canvas_rect } else { artwork_rect },
        canvas,
    );
    if node.is_interactive {
        layout.overflow = Overflow::visible();
    }
    let mut ec = frame.spawn((Name::new(node.name.clone()), layout));
    if node.is_interactive {
        // Picking keeps the exported logical bounds. The child renders the original artwork
        // dimensions around its center, avoiding a second stretch into the rotated AABB.
        ec.with_children(|parent| {
            let mut artwork = parent.spawn((
                Pickable::IGNORE,
                ui_panel_layout(artwork_rect, node.canvas_rect),
                ui_panel_artwork_transform(node),
            ));
            if let Some(img) = image_node {
                artwork.insert(img);
            }
            if let Some(bg) = bg_color {
                artwork.insert(BackgroundColor(bg));
            }
        });
        ec.insert((
            Button,
            Hovered::default(),
            UiPanelInteractiveLabel(node.name.clone()),
            observe(
                |activate: On<Activate>,
                 labels: Query<&UiPanelInteractiveLabel>,
                 mut state: ResMut<ShowcaseState>,
                 manifest: Res<AssetManifest>,
                 asset_server: Res<AssetServer>,
                 existing_audio: Query<Entity, With<ShowcaseAudio>>,
                 mut commands: Commands| {
                    if let Ok(lbl) = labels.get(activate.entity) {
                        state.last_ui_click_label = format!("Clicked `{}`", lbl.0);
                        spawn_next_audio_sfx(
                            &mut state,
                            &manifest,
                            &asset_server,
                            &existing_audio,
                            &mut commands,
                        );
                    }
                },
            ),
        ));
    } else {
        ec.insert((Pickable::IGNORE, ui_panel_artwork_transform(node)));
        if let Some(img) = image_node {
            ec.insert(img);
        }
        if let Some(bg) = bg_color {
            ec.insert(BackgroundColor(bg));
        }
    }
}

fn strip_rich_text_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
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
                let known = low == "b"
                    || low == "/b"
                    || low == "i"
                    || low == "/i"
                    || low == "/color"
                    || low == "/size"
                    || low.starts_with("color=")
                    || low.starts_with("size=");
                if !known {
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

fn resize_ui_panel_frames(
    windows: Query<&Window>,
    mut frame_q: Query<(&UiPanelFrame, &mut Node)>,
    mut text_q: Query<(&UiPanelScaledText, &mut TextFont)>,
) {
    let Some(win) = windows.iter().next() else {
        return;
    };
    let win_size = Vec2::new(win.width(), win.height());
    let mut active_scale = None;
    for (frame, mut node) in &mut frame_q {
        let scale = compute_panel_fit_scale(frame.ref_w, frame.ref_h, win_size);
        node.width = Val::Px(frame.ref_w * scale);
        node.height = Val::Px(frame.ref_h * scale);
        active_scale = Some(scale);
    }
    if let Some(scale) = active_scale {
        for (scaled, mut tf) in &mut text_q {
            tf.font_size = FontSize::Px((scaled.base_px * scale).clamp(7.0, 128.0));
        }
    }
}

fn pick_clip_for_state(
    anim_state: AnimState,
    nodes: &[(String, AnimationNodeIndex)],
) -> Option<(usize, AnimationNodeIndex)> {
    if nodes.is_empty() {
        return None;
    }
    let patterns: &[&str] = match anim_state {
        AnimState::Attack => &["attack", "fire", "shoot", "slash", "strike", "gj", "hit", "skill"],
        AnimState::Jump => &["jump", "tiao", "leap", "air"],
        AnimState::Run => &["run", "sprint", "dash", "pao", "jog"],
        AnimState::Walk => &["walk", "move", "zou"],
        AnimState::Idle => &["idle", "stand", "daiji", "wait", "breath"],
    };
    for pat in patterns {
        if let Some((idx, (_, node))) = nodes
            .iter()
            .enumerate()
            .find(|(_, (name, _))| name.to_ascii_lowercase().contains(pat))
        {
            return Some((idx, *node));
        }
    }
    // Fallback cleanly to Run/Walk for locomotion or Idle/first clip otherwise.
    if anim_state == AnimState::Run
        && let Some(hit) = pick_clip_for_state(AnimState::Walk, nodes)
    {
        return Some(hit);
    }
    if matches!(anim_state, AnimState::Attack | AnimState::Jump | AnimState::Walk)
        && let Some(hit) = pick_clip_for_state(AnimState::Idle, nodes)
    {
        return Some(hit);
    }
    Some((0, nodes[0].1))
}

fn bind_loaded_gltf_animations(
    mut commands: Commands,
    parents: Query<&ChildOf>,
    actors: Query<(&StageActorGltf, Has<PlayerCharacter>)>,
    gltfs: Res<Assets<Gltf>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut players: Query<(Entity, &mut AnimationPlayer), Without<AnimationGraphHandle>>,
    state: Res<ShowcaseState>,
) {
    for (player_entity, mut player) in &mut players {
        // Walk up `ChildOf` chain to locate owning `StageActorGltf`.
        let mut cursor = player_entity;
        let mut found_actor = None;
        for _ in 0..16 {
            if let Ok(actor_info) = actors.get(cursor) {
                found_actor = Some(actor_info);
                break;
            }
            if let Ok(parent) = parents.get(cursor) {
                cursor = parent.parent();
            } else {
                break;
            }
        }
        let Some((actor, is_player)) = found_actor else {
            continue;
        };
        let Some(gltf) = gltfs.get(&actor.gltf) else {
            continue;
        };
        if gltf.animations.is_empty() {
            continue;
        }

        let mut graph = AnimationGraph::new();
        let mut nodes = Vec::with_capacity(gltf.animations.len());
        for (idx, clip) in gltf.animations.iter().enumerate() {
            let clip_name = gltf
                .named_animations
                .iter()
                .find(|(_, h)| *h == clip)
                .map(|(k, _)| k.to_string())
                .unwrap_or_else(|| format!("{}_Clip{idx}", actor.label));
            let node_idx = graph.add_clip(clip.clone(), 1.0, graph.root);
            nodes.push((clip_name, node_idx));
        }

        let start_node = if is_player {
            pick_clip_for_state(AnimState::Idle, &nodes)
                .map(|(_, n)| n)
                .unwrap_or(nodes[0].1)
        } else if let Some((_, idle_n)) = pick_clip_for_state(AnimState::Idle, &nodes) {
            idle_n
        } else {
            nodes[state.clip_index % nodes.len()].1
        };

        let mut transitions = AnimationTransitions::new();
        let active = transitions.play(&mut player, start_node, Duration::ZERO).repeat();
        active.seek_to(actor.phase_offset_secs);
        if state.anim_paused {
            active.pause();
        }

        let role = if is_player {
            BoundActorRole::Player
        } else {
            BoundActorRole::Other
        };

        commands.entity(player_entity).try_insert((
            AnimationGraphHandle(graphs.add(graph)),
            transitions,
            BoundAnimationSet { role, nodes },
        ));
    }
}

fn third_person_player_movement_system(
    time: Res<Time>,
    state: Res<ShowcaseState>,
    keys: Res<ButtonInput<KeyCode>>,
    cam_q: Query<&Transform, (With<Camera3d>, Without<PlayerCharacter>)>,
    mut player_q: Query<(&mut Transform, &mut PlayerCharacter)>,
) {
    if state.control_mode != ControlMode::PlayThirdPerson {
        return;
    }
    let Ok((mut tf, mut player)) = player_q.single_mut() else {
        return;
    };
    let dt = time.delta_secs();

    let (cam_fwd, cam_right) = if let Ok(cam_tf) = cam_q.single() {
        (
            Vec3::new(cam_tf.forward().x, 0.0, cam_tf.forward().z).normalize_or_zero(),
            Vec3::new(cam_tf.right().x, 0.0, cam_tf.right().z).normalize_or_zero(),
        )
    } else {
        (Vec3::NEG_Z, Vec3::X)
    };

    let mut input_dir = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) || keys.pressed(KeyCode::ArrowUp) {
        input_dir += cam_fwd;
    }
    if keys.pressed(KeyCode::KeyS) || keys.pressed(KeyCode::ArrowDown) {
        input_dir -= cam_fwd;
    }
    if keys.pressed(KeyCode::KeyD) || keys.pressed(KeyCode::ArrowRight) {
        input_dir += cam_right;
    }
    if keys.pressed(KeyCode::KeyA) || keys.pressed(KeyCode::ArrowLeft) {
        input_dir -= cam_right;
    }

    let is_sprinting = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let move_speed = if is_sprinting { 6.8 } else { 3.5 };
    let wish = input_dir.normalize_or_zero() * move_speed;
    player.velocity = Vec2::new(wish.x, wish.z);

    if wish.length_squared() > 0.01 {
        player.manual_override = false;
        tf.translation.x += wish.x * dt;
        tf.translation.z += wish.z * dt;

        let target_rot = Transform::from_translation(tf.translation)
            .looking_at(tf.translation + wish.normalize_or_zero(), Vec3::Y)
            .rotation;
        let turn_alpha = (12.0 * dt).min(1.0);
        tf.rotation = tf.rotation.slerp(target_rot, turn_alpha);
    }

    // Clamp onto the 14m stage plinth radius smoothly.
    let horiz = Vec2::new(tf.translation.x, tf.translation.z);
    if horiz.length() > 13.0 {
        let clamped = horiz.normalize() * 13.0;
        tf.translation.x = clamped.x;
        tf.translation.z = clamped.y;
    }

    if keys.just_pressed(KeyCode::Space) && player.grounded {
        player.vertical_vel = 6.8;
        player.grounded = false;
        player.manual_override = false;
    }

    player.vertical_vel -= 18.0 * dt;
    tf.translation.y += player.vertical_vel * dt;
    if tf.translation.y <= 0.0 {
        tf.translation.y = 0.0;
        player.vertical_vel = 0.0;
        player.grounded = true;
    }

    if player.attack_timer > 0.0 {
        player.attack_timer = (player.attack_timer - dt).max(0.0);
    }

    player.anim_state = if player.attack_timer > 0.0 {
        AnimState::Attack
    } else if !player.grounded {
        AnimState::Jump
    } else if wish.length_squared() > 0.05 {
        if is_sprinting {
            AnimState::Run
        } else {
            AnimState::Walk
        }
    } else {
        AnimState::Idle
    };
}

fn find_hand_socket_entity(
    root: Entity,
    children_q: &Query<&Children>,
    names_q: &Query<&Name>,
) -> Option<Entity> {
    const SOCKET_PATTERNS: &[&str] = &[
        "weapon",
        "r_hand",
        "righthand",
        "hand_r",
        "bip001 r hand",
        "bip001_r_hand",
        "mixamorig:righthand",
        "wrist_r",
        "r_wrist",
        "hand.r",
        "grip",
        "socket",
    ];
    let mut stack = vec![root];
    let mut best: Option<(usize, Entity)> = None;
    while let Some(e) = stack.pop() {
        if let Ok(name) = names_q.get(e) {
            let low = name.as_str().to_ascii_lowercase();
            for (rank, pat) in SOCKET_PATTERNS.iter().enumerate() {
                if low.contains(pat) {
                    if best.is_none_or(|(best_rank, _)| rank < best_rank) {
                        best = Some((rank, e));
                    }
                    break;
                }
            }
        }
        if let Ok(children) = children_q.get(e) {
            for child in children.iter() {
                stack.push(child);
            }
        }
    }
    best.map(|(_, e)| e)
}

#[allow(clippy::too_many_arguments)]
fn sync_attached_prop_socket_system(
    mut commands: Commands,
    mut state: ResMut<ShowcaseState>,
    manifest: Res<AssetManifest>,
    asset_server: Res<AssetServer>,
    existing_attached: Query<Entity, With<AttachedProp>>,
    player_q: Query<(Entity, &PlayerCharacter, Option<&Children>, Has<PropSocketSynced>)>,
    children_q: Query<&Children>,
    names_q: Query<&Name>,
) {
    let Ok((player_entity, player, root_children, synced)) = player_q.single() else {
        return;
    };
    if player.stage_gen != state.stage_gen {
        return;
    }

    if !state.dirty_prop_attach && synced {
        return;
    }

    // Wait until the WorldAsset scene hierarchy children have materialized before scanning sockets.
    let Some(children) = root_children else {
        return;
    };
    if children.is_empty() {
        return;
    }
    state.dirty_prop_attach = false;

    for e in existing_attached.iter() {
        commands.entity(e).try_despawn();
    }

    if state.prop_attached && !manifest.props.is_empty() {
        let prop = &manifest.props[state.prop_index % manifest.props.len()];
        let scene_handle: Handle<WorldAsset> =
            asset_server.load(format!("{}#Scene0", prop.asset_path));
        let (mount_target, local_tf) =
            if let Some(socket) = find_hand_socket_entity(player_entity, &children_q, &names_q) {
                (socket, Transform::IDENTITY)
            } else {
                (player_entity, Transform::from_xyz(0.32, 0.95, 0.25))
            };
        commands.spawn((
            AttachedProp,
            Name::new(format!("Attached_{}", prop.name)),
            WorldAssetRoot(scene_handle),
            ChildOf(mount_target),
            local_tf,
        ));
    }

    if !synced {
        commands.entity(player_entity).try_insert(PropSocketSynced);
    }
}

fn update_animation_controls(
    mut state: ResMut<ShowcaseState>,
    mut player_char_q: Query<&mut PlayerCharacter>,
    mut q: Query<(
        &mut AnimationPlayer,
        &mut AnimationTransitions,
        &BoundAnimationSet,
    )>,
) {
    let manual_switch = state.dirty_clip_switch;
    state.dirty_clip_switch = false;

    let player_state = player_char_q.single_mut().ok();
    let (cur_anim_state, last_applied, manual_override) = if let Some(ref p) = player_state {
        (p.anim_state, p.last_applied_state, p.manual_override)
    } else {
        (AnimState::Idle, None, true)
    };

    let auto_state_changed = state.control_mode == ControlMode::PlayThirdPerson
        && !manual_override
        && last_applied != Some(cur_anim_state);

    let mut applied_now = false;
    for (mut anim_player, mut transitions, set) in &mut q {
        if set.nodes.is_empty() {
            continue;
        }
        if manual_switch {
            let (_, node) = &set.nodes[state.clip_index % set.nodes.len()];
            let active = transitions
                .play(&mut anim_player, *node, Duration::from_millis(180))
                .repeat();
            if state.anim_paused {
                active.pause();
            } else {
                active.resume();
            }
        } else if auto_state_changed
            && set.role == BoundActorRole::Player
            && let Some((_, node)) = pick_clip_for_state(cur_anim_state, &set.nodes)
        {
            let active = transitions
                .play(&mut anim_player, node, Duration::from_millis(160))
                .repeat();
            if state.anim_paused {
                active.pause();
            } else {
                active.resume();
            }
            applied_now = true;
        }
    }

    if applied_now && let Some(mut p) = player_state {
        p.last_applied_state = Some(cur_anim_state);
    }
}

#[allow(clippy::too_many_arguments)]
fn camera_controller_system(
    time: Res<Time>,
    state: Res<ShowcaseState>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse_btn: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    player_q: Query<&Transform, (With<PlayerCharacter>, Without<Camera3d>)>,
    mut cam_q: Query<(&mut Transform, &mut OrbitRig), With<Camera3d>>,
) {
    let Ok((mut tf, mut rig)) = cam_q.single_mut() else {
        return;
    };
    let dt = time.delta_secs();
    let drag_delta = motion.delta;

    match state.control_mode {
        ControlMode::PlayThirdPerson => {
            if mouse_btn.pressed(MouseButton::Left) || mouse_btn.pressed(MouseButton::Right) {
                rig.yaw -= drag_delta.x * 0.007;
                rig.pitch = (rig.pitch + drag_delta.y * 0.007).clamp(-1.25, 1.35);
            }
            let scroll_y = match scroll.unit {
                MouseScrollUnit::Line => scroll.delta.y * 0.65,
                MouseScrollUnit::Pixel => scroll.delta.y * 0.015,
            };
            rig.radius = (rig.radius - scroll_y).clamp(1.5, 60.0);

            if let Ok(p_tf) = player_q.single() {
                let desired = p_tf.translation + Vec3::new(0.0, 1.15, 0.0);
                rig.focus = rig.focus.lerp(desired, (10.0 * dt).min(1.0));
            } else {
                let speed = 6.0 * dt;
                let fwd = Vec3::new(tf.forward().x, 0.0, tf.forward().z).normalize_or_zero();
                let right = Vec3::new(tf.right().x, 0.0, tf.right().z).normalize_or_zero();
                if keys.pressed(KeyCode::KeyW) {
                    rig.focus += fwd * speed;
                }
                if keys.pressed(KeyCode::KeyS) {
                    rig.focus -= fwd * speed;
                }
                if keys.pressed(KeyCode::KeyA) {
                    rig.focus -= right * speed;
                }
                if keys.pressed(KeyCode::KeyD) {
                    rig.focus += right * speed;
                }
            }

            let cos_p = rig.pitch.cos();
            let offset = Vec3::new(
                rig.radius * cos_p * rig.yaw.sin(),
                rig.radius * rig.pitch.sin(),
                rig.radius * cos_p * rig.yaw.cos(),
            );
            *tf = Transform::from_translation(rig.focus + offset).looking_at(rig.focus, Vec3::Y);
        }
        ControlMode::Orbit => {
            if mouse_btn.pressed(MouseButton::Left) || mouse_btn.pressed(MouseButton::Right) {
                rig.yaw -= drag_delta.x * 0.007;
                rig.pitch = (rig.pitch + drag_delta.y * 0.007).clamp(-1.35, 1.35);
            }
            if mouse_btn.pressed(MouseButton::Middle) {
                let right = tf.right();
                let up = tf.up();
                let pan_scale = rig.radius * 0.0015;
                rig.focus += (-right * drag_delta.x + up * drag_delta.y) * pan_scale;
            }
            let scroll_y = match scroll.unit {
                MouseScrollUnit::Line => scroll.delta.y * 0.65,
                MouseScrollUnit::Pixel => scroll.delta.y * 0.015,
            };
            rig.radius = (rig.radius - scroll_y).clamp(1.0, 90.0);

            let speed = 6.0 * dt;
            let fwd = Vec3::new(tf.forward().x, 0.0, tf.forward().z).normalize_or_zero();
            let right = Vec3::new(tf.right().x, 0.0, tf.right().z).normalize_or_zero();
            if keys.pressed(KeyCode::KeyW) {
                rig.focus += fwd * speed;
            }
            if keys.pressed(KeyCode::KeyS) {
                rig.focus -= fwd * speed;
            }
            if keys.pressed(KeyCode::KeyA) {
                rig.focus -= right * speed;
            }
            if keys.pressed(KeyCode::KeyD) {
                rig.focus += right * speed;
            }

            let cos_p = rig.pitch.cos();
            let offset = Vec3::new(
                rig.radius * cos_p * rig.yaw.sin(),
                rig.radius * rig.pitch.sin(),
                rig.radius * cos_p * rig.yaw.cos(),
            );
            *tf = Transform::from_translation(rig.focus + offset).looking_at(rig.focus, Vec3::Y);
        }
        ControlMode::FreeFly => {
            if mouse_btn.pressed(MouseButton::Right) || mouse_btn.pressed(MouseButton::Left) {
                rig.yaw -= drag_delta.x * 0.004;
                rig.pitch = (rig.pitch - drag_delta.y * 0.004).clamp(-1.45, 1.45);
                tf.rotation = Quat::from_euler(EulerRot::YXZ, rig.yaw, rig.pitch, 0.0);
            }
            let mut move_dir = Vec3::ZERO;
            if keys.pressed(KeyCode::KeyW) {
                move_dir += *tf.forward();
            }
            if keys.pressed(KeyCode::KeyS) {
                move_dir -= *tf.forward();
            }
            if keys.pressed(KeyCode::KeyD) {
                move_dir += *tf.right();
            }
            if keys.pressed(KeyCode::KeyA) {
                move_dir -= *tf.right();
            }
            if keys.pressed(KeyCode::KeyE) {
                move_dir += Vec3::Y;
            }
            if keys.pressed(KeyCode::KeyQ) {
                move_dir -= Vec3::Y;
            }
            let mul = if keys.pressed(KeyCode::ShiftLeft) { 2.8 } else { 1.0 };
            tf.translation += move_dir.normalize_or_zero() * 9.0 * mul * dt;
        }
    }
}

fn draw_stage_gizmos(state: Res<ShowcaseState>, mut gizmos: Gizmos) {
    if !state.show_gizmos {
        return;
    }
    gizmos.grid(
        Isometry3d::from_translation(Vec3::new(0.0, 0.01, 0.0))
            * Isometry3d::from_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        UVec2::new(26, 26),
        Vec2::splat(1.0),
        Color::srgba(0.35, 0.45, 0.62, 0.35),
    );
    gizmos.axes(Transform::IDENTITY, 1.0);
}

type HudButtonHoverQuery<'w, 's> = Query<
    'w,
    's,
    (&'static Hovered, Has<Pressed>, &'static mut BackgroundColor),
    (With<Button>, With<HudButton>),
>;

fn style_hud_buttons(mut buttons: HudButtonHoverQuery<'_, '_>) {
    for (hovered, pressed, mut bg) in &mut buttons {
        *bg = match (pressed, hovered.get()) {
            (true, _) => BackgroundColor(Color::srgb(0.28, 0.56, 0.85)),
            (false, true) => BackgroundColor(Color::srgb(0.23, 0.33, 0.48)),
            (false, false) => BackgroundColor(Color::srgb(0.14, 0.19, 0.27)),
        };
    }
}

fn update_hud_status_text(
    state: Res<ShowcaseState>,
    manifest: Res<AssetManifest>,
    player_q: Query<&PlayerCharacter>,
    bound_anims: Query<&BoundAnimationSet>,
    mut text_q: Query<&mut Text, With<HudStatusText>>,
) {
    let Ok(mut text) = text_q.single_mut() else {
        return;
    };
    let models = combined_models(&manifest);
    let scene_label = manifest
        .scenes
        .get(state.scene_index % manifest.scenes.len().max(1))
        .map(|s| {
            format!(
                "{} ({} meshes, {} tex, {} tris)",
                s.name, s.mesh_count, s.texture_count, s.triangle_count
            )
        })
        .unwrap_or_else(|| "None".to_owned());
    let model_label = models
        .get(state.model_index % models.len().max(1))
        .map(|m| {
            format!(
                "{} ({} nodes, {} mats, {} skins, {} anims)",
                m.name, m.node_count, m.material_count, m.skin_count, m.animation_count
            )
        })
        .unwrap_or_else(|| "None".to_owned());
    let prop_label = if state.prop_attached && !manifest.props.is_empty() {
        manifest.props[state.prop_index % manifest.props.len()].name.as_str()
    } else {
        "Detached"
    };
    let player_ok = player_q.single().ok();
    let player_state = player_ok
        .map(|p| p.anim_state.label())
        .unwrap_or("None");
    let player_set = bound_anims
        .iter()
        .find(|s| s.role == BoundActorRole::Player && !s.nodes.is_empty())
        .or_else(|| bound_anims.iter().find(|s| !s.nodes.is_empty()));
    let active_clip = if let Some(set) = player_set {
        if let Some(p) = player_ok
            && state.control_mode == ControlMode::PlayThirdPerson
            && !p.manual_override
            && let Some((idx, _)) = pick_clip_for_state(p.anim_state, &set.nodes)
        {
            set.nodes[idx].0.as_str()
        } else {
            set.nodes[state.clip_index % set.nodes.len()].0.as_str()
        }
    } else {
        "None"
    };
    let audio_name = manifest
        .audio
        .get(state.audio_index % manifest.audio.len().max(1))
        .map(|a| a.name.as_str())
        .unwrap_or("None");

    let ui_info = if manifest.ui_panels.is_empty() {
        "None".to_owned()
    } else {
        let idx = (state.ui_panel_index % manifest.ui_panels.len()) + 1;
        let name = if state.active_ui_panel_name.is_empty() {
            manifest.ui_panels[state.ui_panel_index % manifest.ui_panels.len()]
                .name
                .as_str()
        } else {
            state.active_ui_panel_name.as_str()
        };
        if state.last_ui_click_label.is_empty() {
            format!("{name} ({idx}/{})", manifest.ui_panels.len())
        } else {
            format!("{name} ({idx}/{}) [{}]", manifest.ui_panels.len(), state.last_ui_click_label)
        }
    };

    **text = format!(
        "Unity {} | {} | Ctrl: {} | State: {} | Scene: {} | Hero: {} | Socket: {} | Clip: {} {} | UI: {} | SFX: {} ({})",
        manifest.unity_version,
        state.mode.label(),
        state.control_mode.label(),
        player_state,
        scene_label,
        model_label,
        prop_label,
        active_clip,
        if state.anim_paused { "(Paused)" } else { "" },
        ui_info,
        audio_name,
        manifest.audio.len(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_panel_artwork_preserves_hit_bounds_and_flat_rotation() {
        let node: UiPanelNodeDto = serde_json::from_str(r#"{
            "name": "RotatedButton",
            "active_in_hierarchy": true,
            "canvas_rect": [40.0, 20.0, 20.0, 40.0],
            "canvas_render_rect": [30.0, 30.0, 40.0, 20.0],
            "canvas_rotation_z_deg": 90.0,
            "rotation_z_deg": -90.0,
            "is_interactive": true,
            "graphic": {"kind": "image", "color": [0.0, 0.0, 0.0, 1.0]},
            "children": [{
                "name": "CounterRotatedArtwork",
                "active_in_hierarchy": true,
                "canvas_rect": [70.0, 20.0, 40.0, 20.0],
                "canvas_render_rect": [70.0, 20.0, 40.0, 20.0],
                "canvas_rotation_z_deg": 0.0,
                "rotation_z_deg": -90.0,
                "graphic": {"kind": "image", "color": [0.0, 0.0, 0.0, 1.0]}
            }, {
                "name": "RotatedText",
                "active_in_hierarchy": true,
                "canvas_rect": [120.0, 20.0, 20.0, 40.0],
                "canvas_render_rect": [110.0, 30.0, 40.0, 20.0],
                "canvas_rotation_z_deg": -90.0,
                "graphic": {"kind": "text", "text": "Rotated label", "color": [1.0, 1.0, 1.0, 1.0], "font_size": 12.0}
            }]
        }"#).expect("affine panel fixture");
        let mut world = World::new();
        let frame = world.spawn(Node::default()).id();
        world.commands().entity(frame).with_children(|parent| {
            spawn_ui_panel_nodes_depth_first(
                parent,
                &node,
                200.0,
                100.0,
                1.0,
                &HashMap::new(),
                &FontSource::sans_serif(),
            );
        });
        world.flush();

        let mut named = world.query::<(Entity, &Name)>();
        let hitbox = named.iter(&world)
            .find(|(_, name)| name.as_str() == "RotatedButton")
            .map(|(entity, _)| entity).expect("logical hitbox");
        let counter_rotated = named.iter(&world)
            .find(|(_, name)| name.as_str() == "CounterRotatedArtwork")
            .map(|(entity, _)| entity).expect("flat descendant artwork");
        let bounds = world.get::<Node>(hitbox).expect("hitbox bounds");
        assert_eq!(bounds.left, Val::Percent(20.0));
        assert_eq!(bounds.top, Val::Percent(20.0));
        assert_eq!(bounds.width, Val::Percent(10.0));
        assert_eq!(bounds.height, Val::Percent(40.0));
        assert_eq!(world.get::<UiTransform>(hitbox).expect("logical transform").rotation, Rot2::IDENTITY);

        let artwork = world.get::<Children>(hitbox).expect("artwork child")[0];
        let artwork_bounds = world.get::<Node>(artwork).expect("unrotated artwork box");
        assert_eq!(artwork_bounds.left, Val::Percent(-50.0));
        assert_eq!(artwork_bounds.top, Val::Percent(25.0));
        assert_eq!(artwork_bounds.width, Val::Percent(200.0));
        assert_eq!(artwork_bounds.height, Val::Percent(50.0));
        assert_eq!(world.get::<UiTransform>(artwork).expect("artwork transform").rotation, Rot2::degrees(90.0));
        assert_eq!(world.get::<ChildOf>(counter_rotated).expect("frame parent").parent(), frame);
        assert_eq!(world.get::<UiTransform>(counter_rotated).expect("cumulative transform").rotation, Rot2::IDENTITY);

        let mut texts = world.query::<(Entity, &Text)>();
        let text = texts.iter(&world).find(|(_, text)| text.0 == "Rotated label")
            .map(|(entity, _)| entity).expect("rotated text artwork");
        let text_bounds = world.get::<Node>(text).expect("text artwork bounds");
        assert_eq!(text_bounds.width, Val::Percent(20.0));
        assert_eq!(text_bounds.height, Val::Percent(20.0));
        assert_eq!(world.get::<UiTransform>(text).expect("text transform").rotation, Rot2::degrees(-90.0));

        let legacy: UiPanelNodeDto = serde_json::from_str(
            r#"{"name":"Legacy", "canvas_rect":[0.0,0.0,40.0,20.0], "rotation_z_deg":-90.0}"#,
        ).expect("legacy panel fixture");
        assert_eq!(legacy.canvas_render_rect, None);
        assert_eq!(ui_panel_artwork_transform(&legacy).rotation, Rot2::degrees(-90.0));
    }

    #[test]
    fn manifest_loads_and_referenced_assets_exist_on_disk() {
        let raw = std::fs::read_to_string("assets/manifest.json")
            .expect("assets/manifest.json must exist alongside the generated Bevy project");
        let manifest: AssetManifest =
            serde_json::from_str(&raw).expect("manifest.json must parse into AssetManifest");

        let total_glbs = manifest.scenes.len() + manifest.characters.len() + manifest.props.len();
        assert!(
            total_glbs > 0 || !manifest.ui_panels.is_empty(),
            "manifest must reference at least one exported .glb or .json UI panel"
        );

        for entry in manifest
            .scenes
            .iter()
            .chain(manifest.characters.iter())
            .chain(manifest.props.iter())
        {
            let full = std::path::Path::new("assets").join(&entry.asset_path);
            let bytes = std::fs::read(&full)
                .unwrap_or_else(|_| panic!("missing glb file: {}", full.display()));
            assert!(bytes.starts_with(b"glTF"), "invalid GLB header at {}", full.display());
        }

        for media in manifest
            .audio
            .iter()
            .chain(manifest.fonts.iter())
            .chain(manifest.ui_sprites.iter())
        {
            let full = std::path::Path::new("assets").join(&media.asset_path);
            assert!(full.is_file(), "missing media file: {}", full.display());
        }

        for audio_entry in &manifest.audio {
            let full = std::path::Path::new("assets").join(&audio_entry.asset_path);
            let bytes = std::fs::read(&full)
                .unwrap_or_else(|_| panic!("missing audio file: {}", full.display()));
            let source = bevy::audio::AudioSource {
                bytes: bytes.into(),
            };
            let mut decoder = <bevy::audio::AudioSource as bevy::audio::Decodable>::decoder(&source);
            assert!(
                decoder.next().is_some(),
                "exported audio asset `{}` decoded 0 samples",
                full.display()
            );
        }

        for panel in &manifest.ui_panels {
            let full = std::path::Path::new("assets").join(&panel.asset_path);
            let raw_panel = std::fs::read_to_string(&full)
                .unwrap_or_else(|_| panic!("missing UI panel file: {}", full.display()));
            let doc: UiPanelDoc = serde_json::from_str(&raw_panel)
                .unwrap_or_else(|e| panic!("invalid UI panel json {}: {e}", full.display()));
            for tex in &doc.textures {
                if let Some(ref rel) = tex.asset_path {
                    let tex_full = std::path::Path::new("assets").join(rel);
                    assert!(
                        tex_full.is_file(),
                        "missing UI panel texture file: {}",
                        tex_full.display()
                    );
                }
            }
        }
    }
}
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_crate_package_names() {
        assert_eq!(sanitize_package_name("My Game 2026!"), "my_game_2026_");
        assert_eq!(sanitize_package_name("42abc"), "devx_42abc");
    }

    #[test]
    fn bevy_manifest_roundtrips_ui_panels() {
        let manifest = BevyExportManifest {
            project_name: "demo".into(),
            unity_version: "2022.3.0f1".into(),
            ui_panels: vec![BevyUiPanelEntry {
                name: "SettingPanel".into(),
                asset_path: "ui_panels/setting_panel.json".into(),
                source_bundle: "ui/settingpanel.ab".into(),
                reference_width: 1920.0,
                reference_height: 1080.0,
                node_count: 120,
                graphic_count: 45,
                text_count: 18,
                resolved_texture_count: 10,
                missing_texture_count: 0,
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let parsed: BevyExportManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.ui_panels.len(), 1);
        assert_eq!(parsed.ui_panels[0].name, "SettingPanel");
        assert_eq!(parsed.ui_panels[0].reference_width, 1920.0);
    }

    #[test]
    fn matches_any_filter_supports_pipe_alternation() {
        let filters = vec!["man_tou | woman_tou".to_owned(), "loginaccout".to_owned()];
        assert!(matches_any_filter("assets/icon/head1/MAN_TOU_3_1.png", &filters));
        assert!(matches_any_filter("ui/loginaccoutpanel.ab", &filters));
        assert!(!matches_any_filter("sound/gunshot.ab", &filters));
        assert!(BevyExportOptions::default().ui_sprite_filters.is_empty());
    }
}
