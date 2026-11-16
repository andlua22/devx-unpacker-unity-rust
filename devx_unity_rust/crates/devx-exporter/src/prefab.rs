//! Real multi-object Unity `.prefab` and `.unity` scene YAML exporters (`%YAML 1.1`),
//! complete with positive `&fileID` remapping, external companion assets (`.mat`, `.fbx`,
//! `.png`, `.anim`, `.shader` + `.meta` GUIDs), and `.unitypackage` packing.

use crate::hierarchy::sanitize_stem;
use crate::unitypackage::{UnityPackageInputEntry, deterministic_guid, write_unitypackage};
use crate::{ExportError, ExportResult};
use devx_codecs::animator::{
    AnimatorLayerDef, AnimatorParamDef, AnimatorStateDef, AnimatorStateMachineDef,
    AnimatorTransitionDef, BlendTreeKind, DecodedAnimatorController, decode_animator_controller,
};
use devx_codecs::fbx::export_mesh_to_fbx;
use devx_codecs::mesh::decode_mesh;
use devx_codecs::shader::decode_shader;
use devx_codecs::texture::{TextureDecodeOptions, decode_texture2d, encode_png};
use devx_plugin_api::AssetRef;
use devx_unity_io::{
    UnityWorkspaceData, extract_i32_field, extract_i64_field, read_object,
};
use indexmap::IndexMap;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::Path;
use unity_asset_binary::object::UnityObject;
use unity_asset_core::{UnityClass, UnityValue};

/// Options controlling `.prefab` / `.unity` scene YAML export.
#[derive(Debug, Clone)]
pub struct PrefabExportOptions {
    /// Emit `.unity` scene instead of `.prefab` (also collects scene settings singletons).
    pub is_scene: bool,
    /// Export referenced `Material` (`.mat`), `Texture2D` (`.png`), `Mesh` (`.fbx`),
    /// `AnimationClip` (`.anim`), and `Shader` (`.shader`) sidecars + `.meta` files.
    pub include_companion_assets: bool,
    /// Virtual project prefix used to seed deterministic GUIDs and `.unitypackage` paths.
    pub package_prefix: String,
}

impl Default for PrefabExportOptions {
    fn default() -> Self {
        Self {
            is_scene: false,
            include_companion_assets: true,
            package_prefix: "Assets/DevXExport".to_owned(),
        }
    }
}

/// Companion asset or `.meta` sidecar generated alongside a `.prefab` / `.unity`.
#[derive(Debug, Clone)]
pub struct PrefabCompanionFile {
    /// Path relative to the export base folder (e.g. `"Materials/mat.mat"`).
    pub relative_path: String,
    pub bytes: Vec<u8>,
}

/// Complete exported `.prefab` / `.unity` YAML stream, metadata, and sidecars.
#[derive(Debug, Clone)]
pub struct PrefabExportBundle {
    pub root_file_name: String,
    pub yaml_text: String,
    pub root_meta_text: String,
    pub companion_files: Vec<PrefabCompanionFile>,
    pub object_count: usize,
    pub game_object_count: usize,
    pub material_count: usize,
    pub texture_count: usize,
    pub mesh_count: usize,
}

/// Reference to an exported companion YAML asset used when rewriting Unity `PPtr`s.
#[derive(Debug, Clone)]
pub struct ExternalYamlRef {
    pub file_id: i64,
    pub guid: String,
    pub asset_type: i64,
}

/// Collects the full object subtree rooted at `roots`, remaps internal `PathID`s to unique
/// positive YAML `&fileID`s, exports referenced companion assets if requested, and produces a
/// valid `%YAML 1.1` `.prefab` or `.unity` bundle.
pub fn build_prefab_bundle(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    name: &str,
    opts: &PrefabExportOptions,
) -> ExportResult<PrefabExportBundle> {
    build_prefab_bundle_with_external_refs(workspace, roots, name, opts, None)
}

/// Like [`build_prefab_bundle`], optionally seeded with project-wide pre-resolved GUID references
/// so shared materials, meshes, textures, controllers, and MonoScripts link directly without duplicating files.
pub fn build_prefab_bundle_with_external_refs(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    name: &str,
    opts: &PrefabExportOptions,
    pre_resolved_refs: Option<&HashMap<(usize, i64), ExternalYamlRef>>,
) -> ExportResult<PrefabExportBundle> {
    if roots.is_empty() {
        return Err(ExportError::Yaml("no root objects provided for prefab/scene export".to_owned()));
    }

    let stem = sanitize_stem(name);
    let root_ext = if opts.is_scene { "unity" } else { "prefab" };
    let root_file_name = format!("{stem}.{root_ext}");
    let package_root = format!("{}/{stem}", opts.package_prefix.trim_end_matches('/'));

    let mut queue: VecDeque<AssetRef> = VecDeque::new();
    let mut visited: HashSet<(usize, i64)> = HashSet::new();
    let mut ordered_objects: Vec<(AssetRef, UnityObject)> = Vec::new();
    let mut root_file_indices: HashSet<usize> = HashSet::new();

    for &r in roots {
        if r.path_id != 0 {
            queue.push_back(r);
            root_file_indices.insert(r.file_index);
        }
    }

    // For `.unity` scenes, also include scene singleton settings objects from the source file.
    if opts.is_scene {
        for &fi in &root_file_indices {
            if let Some(loaded) = workspace.get_file(fi) {
                for info in &loaded.file.objects {
                    if matches!(info.type_id, 29 | 104 | 157 | 196 | 258 | 1001) {
                        queue.push_back(AssetRef {
                            file_index: fi,
                            path_id: info.path_id,
                        });
                    }
                }
            }
        }
    }

    while let Some(cur) = queue.pop_front() {
        if cur.path_id == 0 || !visited.insert((cur.file_index, cur.path_id)) {
            continue;
        }
        let Some((loaded, info)) = workspace.object_info(cur.file_index, cur.path_id) else {
            continue;
        };
        let Ok(obj) = read_object(loaded, info) else {
            continue;
        };
        let cid = obj.class.class_id;
        let props = obj.class.properties();

        match cid {
            // GameObject: follow every component PPtr (skip null).
            1 => {
                if let Some(arr) = props.get("m_Component").and_then(UnityValue::as_array) {
                    for entry in arr {
                        let pptr = entry
                            .as_object()
                            .and_then(|m| m.get("component").or_else(|| m.get("second")))
                            .or(Some(entry));
                        if let Some(target) = pptr.and_then(|v| resolve_pptr_ref(workspace, cur.file_index, v)) {
                            queue.push_back(target);
                        }
                    }
                }
            }
            // Transform / RectTransform: follow GameObject + parent (if needed) + children recursively.
            4 | 224 => {
                if let Some(go_ref) = props
                    .get("m_GameObject")
                    .and_then(|v| resolve_pptr_ref(workspace, cur.file_index, v))
                {
                    queue.push_back(go_ref);
                }
                if let Some(children) = props.get("m_Children").and_then(UnityValue::as_array) {
                    for ch in children {
                        if let Some(ch_ref) = resolve_pptr_ref(workspace, cur.file_index, ch) {
                            queue.push_back(ch_ref);
                        }
                    }
                }
            }
            _ => {}
        }

        ordered_objects.push((cur, obj));
    }

    if ordered_objects.is_empty() {
        return Err(ExportError::Yaml(
            "selected prefab/scene roots resolved zero readable Unity objects".to_owned(),
        ));
    }

    // Map every internal object to a unique positive YAML anchor (`&fileID`).
    let fileid_map = build_positive_fileid_map(&ordered_objects);

    // Discover & export external companion assets referenced via PPtrs (Textures, Materials, Meshes, Clips, Shaders).
    let mut external_refs: HashMap<(usize, i64), ExternalYamlRef> =
        pre_resolved_refs.cloned().unwrap_or_default();
    let mut companion_files: Vec<PrefabCompanionFile> = Vec::new();
    let mut used_companion_paths: HashSet<String> = HashSet::new();
    let mut stats = CompanionStats::default();

    if opts.include_companion_assets {
        let mut referenced_assets: Vec<AssetRef> = Vec::new();
        let mut seen_ext: HashSet<(usize, i64)> = HashSet::new();

        for (src_ref, obj) in &ordered_objects {
            collect_external_pptrs(
                workspace,
                src_ref.file_index,
                &UnityValue::Object(obj.class.properties().clone()),
                &fileid_map,
                &mut seen_ext,
                &mut referenced_assets,
            );
        }

        // Pull textures/shaders from Materials and AnimationClips from AnimatorControllers
        // before emitting `other_refs` so `.mat` and `.controller` assets can link to their GUIDs.
        let mut mat_objects: Vec<(AssetRef, UnityObject)> = Vec::new();
        let mut controller_objects: Vec<(AssetRef, UnityObject)> = Vec::new();
        let mut other_refs: Vec<AssetRef> = Vec::new();
        for ext_ref in referenced_assets {
            if external_refs.contains_key(&(ext_ref.file_index, ext_ref.path_id)) {
                continue;
            }
            if let Some((loaded, info)) = workspace.object_info(ext_ref.file_index, ext_ref.path_id)
                && let Ok(obj) = read_object(loaded, info)
            {
                if obj.class.class_id == 21 {
                    collect_external_pptrs(
                        workspace,
                        ext_ref.file_index,
                        &UnityValue::Object(obj.class.properties().clone()),
                        &fileid_map,
                        &mut seen_ext,
                        &mut other_refs,
                    );
                    mat_objects.push((ext_ref, obj));
                } else if obj.class.class_id == 91 {
                    collect_external_pptrs(
                        workspace,
                        ext_ref.file_index,
                        &UnityValue::Object(obj.class.properties().clone()),
                        &fileid_map,
                        &mut seen_ext,
                        &mut other_refs,
                    );
                    controller_objects.push((ext_ref, obj));
                } else {
                    other_refs.push(ext_ref);
                }
            }
        }

        // First emit non-material dependencies (Textures, Meshes, Shaders, AnimationClips) so `.mat` can link to them.
        for dep_ref in other_refs {
            export_companion_object(
                workspace,
                dep_ref,
                &package_root,
                &mut used_companion_paths,
                &mut companion_files,
                &mut external_refs,
                &mut stats,
            );
        }

        // Then emit Materials (`.mat` + `.mat.meta`) with Texture & Shader PPtrs rewritten to their GUIDs.
        for (mat_ref, mut mat_obj) in mat_objects {
            let mat_stem = unique_companion_stem(
                &mut used_companion_paths,
                "Materials",
                object_display_stem(&mat_obj, "Material", mat_ref.path_id),
                "mat",
            );
            let rel_path = format!("Materials/{mat_stem}.mat");
            let pkg_path = format!("{package_root}/{rel_path}");
            let mat_guid = deterministic_guid(&pkg_path);

            let mut rewritten = UnityValue::Object(mat_obj.class.properties().clone());
            rewrite_pptrs_in_value(workspace, mat_ref.file_index, &mut rewritten, &fileid_map, &external_refs);
            if let UnityValue::Object(map) = rewritten {
                *mat_obj.class.properties_mut() = map;
            }
            mat_obj.class.anchor = "2100000".to_owned();

            if let Ok(yaml) = serialize_unity_classes_yaml(&[mat_obj.class]) {
                let meta = format!(
                    "fileFormatVersion: 2\nguid: {mat_guid}\nNativeFormatImporter:\n  externalObjects: {{}}\n  mainObjectFileID: 2100000\n"
                );
                companion_files.push(PrefabCompanionFile {
                    relative_path: rel_path.clone(),
                    bytes: yaml.into_bytes(),
                });
                companion_files.push(PrefabCompanionFile {
                    relative_path: format!("{rel_path}.meta"),
                    bytes: meta.into_bytes(),
                });
                external_refs.insert(
                    (mat_ref.file_index, mat_ref.path_id),
                    ExternalYamlRef {
                        file_id: 2_100_000,
                        guid: mat_guid,
                        asset_type: 2,
                    },
                );
                stats.materials += 1;
            }
        }

        // Emit AnimatorControllers (`.controller` + `.controller.meta`) with linked AnimationClip GUIDs.
        for (ctrl_ref, ctrl_obj) in controller_objects {
            export_animator_controller_companion(
                workspace,
                ctrl_ref,
                &ctrl_obj,
                &package_root,
                &mut used_companion_paths,
                &mut companion_files,
                &mut external_refs,
            );
        }
    }

    // Rewrite every object in the main prefab/scene and serialize to a single `%YAML 1.1` stream.
    let mut classes: Vec<UnityClass> = Vec::with_capacity(ordered_objects.len());
    let mut game_object_count = 0usize;

    for (obj_ref, obj) in ordered_objects {
        let mut class = obj.class;
        if class.class_id == 1 {
            game_object_count += 1;
        }
        let anchor_id = fileid_map
            .get(&(obj_ref.file_index, obj_ref.path_id))
            .copied()
            .unwrap_or(1);
        class.anchor = anchor_id.to_string();

        let mut rewritten = UnityValue::Object(class.properties().clone());
        rewrite_pptrs_in_value(workspace, obj_ref.file_index, &mut rewritten, &fileid_map, &external_refs);
        if let UnityValue::Object(map) = rewritten {
            *class.properties_mut() = map;
        }
        classes.push(class);
    }

    let yaml_text = serialize_unity_classes_yaml(&classes)?;
    let root_pkg_path = format!("{package_root}/{root_file_name}");
    let root_guid = deterministic_guid(&root_pkg_path);
    let root_meta_text = if opts.is_scene {
        format!("fileFormatVersion: 2\nguid: {root_guid}\nDefaultAsset:\n  externalObjects: {{}}\n")
    } else {
        format!("fileFormatVersion: 2\nguid: {root_guid}\nPrefabImporter:\n  externalObjects: {{}}\n")
    };

    Ok(PrefabExportBundle {
        root_file_name,
        yaml_text,
        root_meta_text,
        companion_files,
        object_count: classes.len(),
        game_object_count,
        material_count: stats.materials,
        texture_count: stats.textures,
        mesh_count: stats.meshes,
    })
}

/// Serializes any list of [`UnityClass`] records (with non-empty positive anchors) into a
/// complete multi-document Unity `%YAML 1.1` string without truncating nested mappings.
pub fn serialize_unity_classes_yaml(classes: &[UnityClass]) -> ExportResult<String> {
    let mut out = String::with_capacity(classes.len() * 512 + 64);
    out.push_str("%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n");
    for class in classes {
        let anchor = if class.anchor.trim().is_empty() {
            "100000".to_owned()
        } else {
            class.anchor.trim().to_owned()
        };
        let _ = writeln!(out, "--- !u!{} &{anchor}", class.class_id);
        let _ = writeln!(out, "{}:", class.class_name);
        write_yaml_map(&mut out, class.properties(), 2);
    }
    Ok(out)
}

fn is_scalar_value(v: &UnityValue) -> bool {
    matches!(
        v,
        UnityValue::Null
            | UnityValue::Bool(_)
            | UnityValue::Integer(_)
            | UnityValue::Float(_)
            | UnityValue::String(_)
    )
}

fn is_inline_scalar_map(map: &IndexMap<String, UnityValue>) -> bool {
    !map.is_empty()
        && map.len() <= 5
        && map.values().all(is_scalar_value)
        && map
            .values()
            .filter_map(UnityValue::as_str)
            .map(str::len)
            .sum::<usize>()
            <= 64
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push(' ');
    }
}

fn format_yaml_scalar(v: &UnityValue) -> String {
    match v {
        UnityValue::Null => "~".to_owned(),
        UnityValue::Bool(b) => if *b { "1" } else { "0" }.to_owned(),
        UnityValue::Integer(i) => i.to_string(),
        UnityValue::Float(f) => {
            if !f.is_finite() {
                "0".to_owned()
            } else if (*f - f.round()).abs() < 1e-12 && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                // Trim redundant float noise when f32-precision is sufficient.
                let f32v = *f as f32;
                if (f64::from(f32v) - *f).abs() < 1e-12 {
                    format!("{f32v}")
                } else {
                    format!("{f}")
                }
            }
        }
        UnityValue::String(s) => format_yaml_string(s),
        UnityValue::Bytes(b) => {
            if b.is_empty() {
                "\"\"".to_owned()
            } else {
                let mut hex = String::with_capacity(b.len() * 2);
                for byte in b {
                    let _ = write!(hex, "{byte:02x}");
                }
                hex
            }
        }
        UnityValue::Array(_) => "[]".to_owned(),
        UnityValue::Object(m) => {
            let mut s = String::from("{");
            for (i, (k, val)) in m.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                let _ = write!(s, "{}: {}", format_yaml_string(k), format_yaml_scalar(val));
            }
            s.push('}');
            s
        }
    }
}

fn format_yaml_string(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".to_owned();
    }
    let lower = s.to_ascii_lowercase();
    let reserved = matches!(lower.as_str(), "true" | "false" | "yes" | "no" | "null" | "~");
    let numeric_like = s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok();
    let needs_quotes = reserved
        || numeric_like
        || s.starts_with(['-', '?', ' ', '\t'])
        || s.ends_with([' ', '\t'])
        || s.bytes().any(|b| {
            matches!(
                b,
                b':' | b'#'
                    | b'{'
                    | b'}'
                    | b'['
                    | b']'
                    | b','
                    | b'&'
                    | b'*'
                    | b'!'
                    | b'|'
                    | b'>'
                    | b'\''
                    | b'"'
                    | b'%'
                    | b'@'
                    | b'`'
                    | b'\n'
                    | b'\r'
                    | b'\t'
                    | b'\\'
            )
        });
    if !needs_quotes {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn write_yaml_map(out: &mut String, map: &IndexMap<String, UnityValue>, indent: usize) {
    for (k, v) in map {
        push_indent(out, indent);
        out.push_str(&format_yaml_string(k));
        out.push(':');
        write_yaml_field_value(out, v, indent);
    }
}

fn write_yaml_field_value(out: &mut String, v: &UnityValue, key_indent: usize) {
    match v {
        UnityValue::Object(sub) => {
            if sub.is_empty() {
                out.push_str(" {}\n");
            } else if is_inline_scalar_map(sub) {
                out.push(' ');
                out.push_str(&format_yaml_scalar(v));
                out.push('\n');
            } else {
                out.push('\n');
                write_yaml_map(out, sub, key_indent + 2);
            }
        }
        UnityValue::Array(arr) => {
            if arr.is_empty() {
                out.push_str(" []\n");
            } else {
                out.push('\n');
                for item in arr {
                    push_indent(out, key_indent + 2);
                    out.push_str("- ");
                    write_yaml_array_item(out, item, key_indent + 4);
                }
            }
        }
        _ => {
            out.push(' ');
            out.push_str(&format_yaml_scalar(v));
            out.push('\n');
        }
    }
}

fn write_yaml_array_item(out: &mut String, item: &UnityValue, item_field_indent: usize) {
    match item {
        UnityValue::Object(sub) if !sub.is_empty() && !is_inline_scalar_map(sub) => {
            for (idx, (k, v)) in sub.iter().enumerate() {
                if idx > 0 {
                    push_indent(out, item_field_indent);
                }
                out.push_str(&format_yaml_string(k));
                out.push(':');
                write_yaml_field_value(out, v, item_field_indent);
            }
        }
        UnityValue::Array(nested) if !nested.is_empty() => {
            out.push('\n');
            for child in nested {
                push_indent(out, item_field_indent);
                out.push_str("- ");
                write_yaml_array_item(out, child, item_field_indent + 2);
            }
        }
        _ => {
            out.push_str(&format_yaml_scalar(item));
            out.push('\n');
        }
    }
}

/// Returns a standard Unity `.controller.meta` YAML payload pointing to root `AnimatorController` (`9100000`).
pub fn animator_controller_meta_text(guid: &str) -> String {
    format!(
        "fileFormatVersion: 2\nguid: {guid}\nNativeFormatImporter:\n  externalObjects: {{}}\n  mainObjectFileID: 9100000\n"
    )
}

/// Serializes a decoded runtime [`DecodedAnimatorController`] into native Unity Editor `.controller`
/// (`%YAML 1.1`, class `91`) YAML linking to exported `.anim` clip GUIDs via `clip_external_refs`.
pub fn build_animator_controller_yaml(
    controller: &DecodedAnimatorController,
    clip_external_refs: &HashMap<(usize, i64), ExternalYamlRef>,
) -> ExportResult<String> {
    const ROOT_FILE_ID: i64 = 9_100_000;
    let mut next_anchor: i64 = 1_107_000_000;
    let mut alloc_anchor = || {
        let id = next_anchor;
        next_anchor += 1;
        id
    };

    let mut sm_anchors: Vec<i64> = Vec::with_capacity(controller.state_machines.len());
    let mut state_anchors: Vec<Vec<i64>> = Vec::with_capacity(controller.state_machines.len());
    let mut state_trans_anchors: Vec<Vec<Vec<i64>>> =
        Vec::with_capacity(controller.state_machines.len());
    let mut any_trans_anchors: Vec<Vec<i64>> =
        Vec::with_capacity(controller.state_machines.len());
    let mut state_bt_anchors: Vec<Vec<Vec<Option<i64>>>> =
        Vec::with_capacity(controller.state_machines.len());

    for sm in &controller.state_machines {
        sm_anchors.push(alloc_anchor());
        let mut s_ids = Vec::with_capacity(sm.states.len());
        let mut st_ids = Vec::with_capacity(sm.states.len());
        let mut bt_ids = Vec::with_capacity(sm.states.len());
        for state in &sm.states {
            s_ids.push(alloc_anchor());
            let mut tr_ids = Vec::with_capacity(state.transitions.len());
            for _ in &state.transitions {
                tr_ids.push(alloc_anchor());
            }
            st_ids.push(tr_ids);

            let uses_tree = state_uses_blend_tree(state);
            let mut node_ids = Vec::with_capacity(state.blend_tree.len());
            for node in &state.blend_tree {
                if uses_tree && (node.kind != BlendTreeKind::SimpleClip || !node.child_indices.is_empty()) {
                    node_ids.push(Some(alloc_anchor()));
                } else {
                    node_ids.push(None);
                }
            }
            bt_ids.push(node_ids);
        }
        state_anchors.push(s_ids);
        state_trans_anchors.push(st_ids);
        state_bt_anchors.push(bt_ids);

        let mut any_ids = Vec::with_capacity(sm.any_state_transitions.len());
        for _ in &sm.any_state_transitions {
            any_ids.push(alloc_anchor());
        }
        any_trans_anchors.push(any_ids);
    }

    let mut root = UnityClass::new(91, "AnimatorController".to_owned(), ROOT_FILE_ID.to_string());
    root.set("m_ObjectHideFlags".to_owned(), UnityValue::Integer(0));
    root.set("m_Name".to_owned(), UnityValue::String(controller.name.clone()));
    root.set(
        "m_AnimatorParameters".to_owned(),
        UnityValue::Array(
            controller
                .parameters
                .iter()
                .map(|p| build_ctrl_param_value(p, ROOT_FILE_ID))
                .collect(),
        ),
    );
    root.set(
        "m_AnimatorLayers".to_owned(),
        UnityValue::Array(
            controller
                .layers
                .iter()
                .map(|l| {
                    let sm_fid = sm_anchors.get(l.state_machine_idx).copied().unwrap_or(0);
                    build_ctrl_layer_value(l, sm_fid, ROOT_FILE_ID)
                })
                .collect(),
        ),
    );

    let mut classes = vec![root];
    for (sm_idx, sm) in controller.state_machines.iter().enumerate() {
        let layer_name = controller
            .layers
            .iter()
            .find(|l| l.state_machine_idx == sm_idx)
            .map(|l| l.name.as_str())
            .unwrap_or("Base Layer");
        let s_ids = &state_anchors[sm_idx];
        let any_ids = &any_trans_anchors[sm_idx];
        classes.push(build_state_machine_class(
            sm,
            layer_name,
            sm_anchors[sm_idx],
            s_ids,
            any_ids,
        ));

        for (t_idx, tr) in sm.any_state_transitions.iter().enumerate() {
            classes.push(build_transition_class(tr, any_ids[t_idx], s_ids));
        }

        for (s_idx, state) in sm.states.iter().enumerate() {
            let st_tr_ids = &state_trans_anchors[sm_idx][s_idx];
            let bt_ids = &state_bt_anchors[sm_idx][s_idx];
            let motion_pptr = if let Some(root_bt_fid) = bt_ids.first().copied().flatten() {
                yaml_pptr_internal(root_bt_fid)
            } else {
                resolve_ctrl_clip_pptr(
                    controller,
                    state.leaf_clip_indices.first().copied(),
                    clip_external_refs,
                )
            };
            classes.push(build_state_class(state, s_ids[s_idx], st_tr_ids, motion_pptr));

            for (t_idx, tr) in state.transitions.iter().enumerate() {
                classes.push(build_transition_class(tr, st_tr_ids[t_idx], s_ids));
            }

            for (n_idx, node) in state.blend_tree.iter().enumerate() {
                if let Some(bt_fid) = bt_ids[n_idx] {
                    classes.push(build_blend_tree_class(
                        controller,
                        state,
                        node,
                        bt_fid,
                        bt_ids,
                        clip_external_refs,
                    ));
                }
            }
        }
    }

    serialize_unity_classes_yaml(&classes)
}

fn state_uses_blend_tree(state: &AnimatorStateDef) -> bool {
    state.blend_tree.len() > 1
        || state
            .blend_tree
            .first()
            .is_some_and(|n| n.kind != BlendTreeKind::SimpleClip || !n.child_indices.is_empty())
}

fn yaml_pptr_internal(file_id: i64) -> UnityValue {
    let mut m = IndexMap::new();
    m.insert("fileID".to_owned(), UnityValue::Integer(file_id));
    UnityValue::Object(m)
}

fn yaml_pptr_external(ext: &ExternalYamlRef) -> UnityValue {
    let mut m = IndexMap::new();
    m.insert("fileID".to_owned(), UnityValue::Integer(ext.file_id));
    m.insert("guid".to_owned(), UnityValue::String(ext.guid.clone()));
    m.insert("type".to_owned(), UnityValue::Integer(ext.asset_type));
    UnityValue::Object(m)
}

fn yaml_vec2(x: f32, y: f32) -> UnityValue {
    let mut m = IndexMap::new();
    m.insert("x".to_owned(), UnityValue::Float(f64::from(x)));
    m.insert("y".to_owned(), UnityValue::Float(f64::from(y)));
    UnityValue::Object(m)
}

fn yaml_vec3(x: f32, y: f32, z: f32) -> UnityValue {
    let mut m = IndexMap::new();
    m.insert("x".to_owned(), UnityValue::Float(f64::from(x)));
    m.insert("y".to_owned(), UnityValue::Float(f64::from(y)));
    m.insert("z".to_owned(), UnityValue::Float(f64::from(z)));
    UnityValue::Object(m)
}

fn resolve_ctrl_clip_pptr(
    controller: &DecodedAnimatorController,
    clip_idx: Option<usize>,
    clip_external_refs: &HashMap<(usize, i64), ExternalYamlRef>,
) -> UnityValue {
    if let Some(idx) = clip_idx
        && let Some(link) = controller.clip_links.get(idx)
        && let Some(r) = link.asset_ref
        && let Some(ext) = clip_external_refs.get(&(r.file_index, r.path_id))
    {
        yaml_pptr_external(ext)
    } else {
        yaml_pptr_internal(0)
    }
}

fn build_ctrl_param_value(param: &AnimatorParamDef, ctrl_file_id: i64) -> UnityValue {
    let (m_type, def_float, def_int, def_bool) = match param.kind_label {
        "Int" => (3, 0.0, param.default_display.trim().parse::<i64>().unwrap_or(0), 0),
        "Bool" => (
            4,
            0.0,
            0,
            if param.default_display.trim().eq_ignore_ascii_case("true") {
                1
            } else {
                0
            },
        ),
        "Trigger" => (9, 0.0, 0, 0),
        _ => (1, param.default_display.trim().parse::<f64>().unwrap_or(0.0), 0, 0),
    };
    let mut m = IndexMap::new();
    m.insert("m_Name".to_owned(), UnityValue::String(param.name.clone()));
    m.insert("m_Type".to_owned(), UnityValue::Integer(m_type));
    m.insert("m_DefaultFloat".to_owned(), UnityValue::Float(def_float));
    m.insert("m_DefaultInt".to_owned(), UnityValue::Integer(def_int));
    m.insert("m_DefaultBool".to_owned(), UnityValue::Integer(def_bool));
    m.insert("m_Controller".to_owned(), yaml_pptr_internal(ctrl_file_id));
    UnityValue::Object(m)
}

fn build_ctrl_layer_value(
    layer: &AnimatorLayerDef,
    sm_file_id: i64,
    ctrl_file_id: i64,
) -> UnityValue {
    let mut m = IndexMap::new();
    m.insert("serializedVersion".to_owned(), UnityValue::Integer(5));
    m.insert("m_Name".to_owned(), UnityValue::String(layer.name.clone()));
    m.insert("m_StateMachine".to_owned(), yaml_pptr_internal(sm_file_id));
    m.insert("m_Mask".to_owned(), yaml_pptr_internal(0));
    m.insert("m_Motions".to_owned(), UnityValue::Array(Vec::new()));
    m.insert("m_Behaviours".to_owned(), UnityValue::Array(Vec::new()));
    m.insert("m_BlendingMode".to_owned(), UnityValue::Integer(i64::from(layer.blending_mode)));
    m.insert("m_SyncedLayerIndex".to_owned(), UnityValue::Integer(-1));
    m.insert("m_DefaultWeight".to_owned(), UnityValue::Float(f64::from(layer.default_weight)));
    m.insert("m_IKPass".to_owned(), UnityValue::Integer(0));
    m.insert("m_SyncedLayerAffectsTiming".to_owned(), UnityValue::Integer(0));
    m.insert("m_Controller".to_owned(), yaml_pptr_internal(ctrl_file_id));
    UnityValue::Object(m)
}

fn build_state_machine_class(
    sm: &AnimatorStateMachineDef,
    name: &str,
    sm_anchor: i64,
    state_anchors: &[i64],
    any_trans_anchors: &[i64],
) -> UnityClass {
    let child_states: Vec<UnityValue> = state_anchors
        .iter()
        .enumerate()
        .map(|(idx, &fid)| {
            let mut cs = IndexMap::new();
            cs.insert("serializedVersion".to_owned(), UnityValue::Integer(1));
            cs.insert("state".to_owned(), yaml_pptr_internal(fid));
            let col = (idx % 4) as f32;
            let row = (idx / 4) as f32;
            cs.insert(
                "position".to_owned(),
                yaml_vec3(260.0 + col * 240.0, 20.0 + row * 80.0, 0.0),
            );
            UnityValue::Object(cs)
        })
        .collect();
    let default_fid = state_anchors.get(sm.default_state).copied().unwrap_or(0);

    let mut c = UnityClass::new(1107, "AnimatorStateMachine".to_owned(), sm_anchor.to_string());
    c.set("serializedVersion".to_owned(), UnityValue::Integer(6));
    c.set("m_ObjectHideFlags".to_owned(), UnityValue::Integer(1));
    c.set("m_Name".to_owned(), UnityValue::String(name.to_owned()));
    c.set("m_ChildStates".to_owned(), UnityValue::Array(child_states));
    c.set("m_ChildStateMachines".to_owned(), UnityValue::Array(Vec::new()));
    c.set(
        "m_AnyStateTransitions".to_owned(),
        UnityValue::Array(any_trans_anchors.iter().copied().map(yaml_pptr_internal).collect()),
    );
    c.set("m_EntryTransitions".to_owned(), UnityValue::Array(Vec::new()));
    c.set("m_StateMachineTransitions".to_owned(), UnityValue::Object(IndexMap::new()));
    c.set("m_StateMachineBehaviours".to_owned(), UnityValue::Array(Vec::new()));
    c.set("m_AnyStatePosition".to_owned(), yaml_vec3(50.0, 20.0, 0.0));
    c.set("m_EntryPosition".to_owned(), yaml_vec3(50.0, 120.0, 0.0));
    c.set("m_ExitPosition".to_owned(), yaml_vec3(800.0, 120.0, 0.0));
    c.set("m_ParentStateMachinePosition".to_owned(), yaml_vec3(800.0, 20.0, 0.0));
    c.set("m_DefaultState".to_owned(), yaml_pptr_internal(default_fid));
    c
}

fn build_state_class(
    state: &AnimatorStateDef,
    anchor: i64,
    trans_anchors: &[i64],
    motion_pptr: UnityValue,
) -> UnityClass {
    let mut c = UnityClass::new(1102, "AnimatorState".to_owned(), anchor.to_string());
    c.set("serializedVersion".to_owned(), UnityValue::Integer(6));
    c.set("m_ObjectHideFlags".to_owned(), UnityValue::Integer(1));
    c.set("m_Name".to_owned(), UnityValue::String(state.name.clone()));
    c.set("m_Speed".to_owned(), UnityValue::Float(f64::from(state.speed)));
    c.set("m_CycleOffset".to_owned(), UnityValue::Float(0.0));
    c.set(
        "m_Transitions".to_owned(),
        UnityValue::Array(trans_anchors.iter().copied().map(yaml_pptr_internal).collect()),
    );
    c.set("m_StateMachineBehaviours".to_owned(), UnityValue::Array(Vec::new()));
    c.set("m_Position".to_owned(), yaml_vec3(50.0, 50.0, 0.0));
    c.set("m_IKOnFeet".to_owned(), UnityValue::Integer(0));
    c.set("m_WriteDefaultValues".to_owned(), UnityValue::Integer(1));
    c.set("m_Mirror".to_owned(), UnityValue::Integer(0));
    c.set("m_SpeedParameterActive".to_owned(), UnityValue::Integer(0));
    c.set("m_MirrorParameterActive".to_owned(), UnityValue::Integer(0));
    c.set("m_CycleOffsetParameterActive".to_owned(), UnityValue::Integer(0));
    c.set("m_TimeParameterActive".to_owned(), UnityValue::Integer(0));
    c.set("m_Motion".to_owned(), motion_pptr);
    c.set("m_Tag".to_owned(), UnityValue::String(state.tag.clone()));
    c.set("m_SpeedParameter".to_owned(), UnityValue::String(String::new()));
    c.set("m_MirrorParameter".to_owned(), UnityValue::String(String::new()));
    c.set("m_CycleOffsetParameter".to_owned(), UnityValue::String(String::new()));
    c.set("m_TimeParameter".to_owned(), UnityValue::String(String::new()));
    c
}

fn build_transition_class(
    tr: &AnimatorTransitionDef,
    anchor: i64,
    state_anchors: &[i64],
) -> UnityClass {
    let conditions: Vec<UnityValue> = tr
        .conditions
        .iter()
        .map(|cond| {
            let mode_code = match cond.mode_label {
                "If (true)" => 1,
                "IfNot (false)" => 2,
                ">" => 3,
                "<" => 4,
                "==" => 6,
                "!=" => 7,
                _ => 1,
            };
            let mut cm = IndexMap::new();
            cm.insert("m_ConditionMode".to_owned(), UnityValue::Integer(mode_code));
            cm.insert("m_ConditionEvent".to_owned(), UnityValue::String(cond.param_name.clone()));
            cm.insert("m_EventTreshold".to_owned(), UnityValue::Float(f64::from(cond.threshold)));
            UnityValue::Object(cm)
        })
        .collect();
    let dst_fid = tr
        .destination_state_idx
        .and_then(|idx| state_anchors.get(idx).copied())
        .unwrap_or(0);

    let mut c = UnityClass::new(1101, "AnimatorStateTransition".to_owned(), anchor.to_string());
    c.set("serializedVersion".to_owned(), UnityValue::Integer(3));
    c.set("m_ObjectHideFlags".to_owned(), UnityValue::Integer(1));
    c.set("m_Name".to_owned(), UnityValue::String(String::new()));
    c.set("m_Conditions".to_owned(), UnityValue::Array(conditions));
    c.set("m_DstStateMachine".to_owned(), yaml_pptr_internal(0));
    c.set("m_DstState".to_owned(), yaml_pptr_internal(dst_fid));
    c.set("m_Solo".to_owned(), UnityValue::Integer(0));
    c.set("m_Mute".to_owned(), UnityValue::Integer(0));
    c.set("m_IsExit".to_owned(), UnityValue::Integer(i64::from(tr.is_exit)));
    c.set("m_TransitionDuration".to_owned(), UnityValue::Float(f64::from(tr.duration)));
    c.set("m_TransitionOffset".to_owned(), UnityValue::Float(0.0));
    c.set("m_ExitTime".to_owned(), UnityValue::Float(f64::from(tr.exit_time)));
    c.set("m_HasExitTime".to_owned(), UnityValue::Integer(i64::from(tr.has_exit_time)));
    c.set("m_HasFixedDuration".to_owned(), UnityValue::Integer(1));
    c.set("m_InterruptionSource".to_owned(), UnityValue::Integer(0));
    c.set("m_OrderedInterruption".to_owned(), UnityValue::Integer(1));
    c.set("m_CanTransitionToSelf".to_owned(), UnityValue::Integer(1));
    c
}

fn build_blend_tree_class(
    controller: &DecodedAnimatorController,
    state: &AnimatorStateDef,
    node: &devx_codecs::animator::BlendTreeNodeDef,
    anchor: i64,
    bt_anchors: &[Option<i64>],
    clip_external_refs: &HashMap<(usize, i64), ExternalYamlRef>,
) -> UnityClass {
    let childs: Vec<UnityValue> = node
        .child_indices
        .iter()
        .enumerate()
        .map(|(k, &child_idx)| {
            let child_motion = if let Some(sub_bt_fid) = bt_anchors.get(child_idx).copied().flatten() {
                yaml_pptr_internal(sub_bt_fid)
            } else {
                let clip_id = state.blend_tree.get(child_idx).and_then(|n| n.clip_id);
                resolve_ctrl_clip_pptr(controller, clip_id, clip_external_refs)
            };
            let threshold = node.thresholds_1d.get(k).copied().unwrap_or(k as f32);
            let pos = node.positions_2d.get(k).copied().unwrap_or([0.0, 0.0]);
            let direct_param = node
                .direct_params
                .get(k)
                .and_then(|o| o.clone())
                .unwrap_or_else(|| "Blend".to_owned());

            let mut ch = IndexMap::new();
            ch.insert("m_Motion".to_owned(), child_motion);
            ch.insert("m_Threshold".to_owned(), UnityValue::Float(f64::from(threshold)));
            ch.insert("m_Position".to_owned(), yaml_vec2(pos[0], pos[1]));
            ch.insert("m_TimeScale".to_owned(), UnityValue::Float(1.0));
            ch.insert("m_CycleOffset".to_owned(), UnityValue::Float(0.0));
            ch.insert("m_DirectBlendParameter".to_owned(), UnityValue::String(direct_param));
            ch.insert("m_Mirror".to_owned(), UnityValue::Integer(0));
            UnityValue::Object(ch)
        })
        .collect();

    let min_t = node.thresholds_1d.iter().copied().fold(f32::INFINITY, f32::min);
    let max_t = node.thresholds_1d.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    // Editor YAML BlendTree uses 0..=4 (1D..Direct), 1 lower than runtime BlendTreeConstant (1..=5).
    let yaml_blend_type = match node.kind {
        BlendTreeKind::Blend1D | BlendTreeKind::SimpleClip => 0,
        BlendTreeKind::Blend2DSimpleDirectional => 1,
        BlendTreeKind::Blend2DFreeformDirectional => 2,
        BlendTreeKind::Blend2DFreeformCartesian => 3,
        BlendTreeKind::Direct => 4,
    };

    let mut c = UnityClass::new(206, "BlendTree".to_owned(), anchor.to_string());
    c.set("m_ObjectHideFlags".to_owned(), UnityValue::Integer(1));
    c.set("m_Name".to_owned(), UnityValue::String(format!("Blend Tree ({})", state.name)));
    c.set("m_Childs".to_owned(), UnityValue::Array(childs));
    c.set(
        "m_BlendParameter".to_owned(),
        UnityValue::String(node.param_x.clone().unwrap_or_else(|| "Blend".to_owned())),
    );
    c.set(
        "m_BlendParameterY".to_owned(),
        UnityValue::String(node.param_y.clone().unwrap_or_else(|| "Blend".to_owned())),
    );
    c.set(
        "m_MinThreshold".to_owned(),
        UnityValue::Float(f64::from(if min_t.is_finite() { min_t } else { 0.0 })),
    );
    c.set(
        "m_MaxThreshold".to_owned(),
        UnityValue::Float(f64::from(if max_t.is_finite() { max_t } else { 1.0 })),
    );
    c.set("m_UseAutomaticThresholds".to_owned(), UnityValue::Integer(0));
    c.set("m_NormalizedBlendValues".to_owned(), UnityValue::Integer(i64::from(node.normalized_direct)));
    c.set("m_BlendType".to_owned(), UnityValue::Integer(yaml_blend_type));
    c
}

fn export_animator_controller_companion(
    workspace: &UnityWorkspaceData,
    ctrl_ref: AssetRef,
    ctrl_obj: &UnityObject,
    package_root: &str,
    used_paths: &mut HashSet<String>,
    companion_files: &mut Vec<PrefabCompanionFile>,
    external_refs: &mut HashMap<(usize, i64), ExternalYamlRef>,
) {
    if external_refs.contains_key(&(ctrl_ref.file_index, ctrl_ref.path_id)) {
        return;
    }
    let Ok(decoded) = decode_animator_controller(ctrl_obj, ctrl_ref.file_index, workspace) else {
        return;
    };
    let Ok(yaml) = build_animator_controller_yaml(&decoded, external_refs) else {
        return;
    };
    let stem = unique_companion_stem(
        used_paths,
        "Controllers",
        object_display_stem(ctrl_obj, "AnimatorController", ctrl_ref.path_id),
        "controller",
    );
    let rel = format!("Controllers/{stem}.controller");
    let guid = deterministic_guid(&format!("{package_root}/{rel}"));
    let meta = animator_controller_meta_text(&guid);
    companion_files.push(PrefabCompanionFile {
        relative_path: rel.clone(),
        bytes: yaml.into_bytes(),
    });
    companion_files.push(PrefabCompanionFile {
        relative_path: format!("{rel}.meta"),
        bytes: meta.into_bytes(),
    });
    external_refs.insert(
        (ctrl_ref.file_index, ctrl_ref.path_id),
        ExternalYamlRef {
            file_id: 9_100_000,
            guid,
            asset_type: 2,
        },
    );
}

/// Writes a `.prefab` or `.unity` file, its `.meta`, and all companion files (`Materials/`,
/// `Textures/`, `Meshes/`, etc.) into `out_dir`.
pub fn export_prefab_to_dir(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    name: &str,
    out_dir: &Path,
    opts: &PrefabExportOptions,
) -> ExportResult<PrefabExportBundle> {
    let bundle = build_prefab_bundle(workspace, roots, name, opts)?;
    std::fs::create_dir_all(out_dir)?;
    std::fs::write(out_dir.join(&bundle.root_file_name), bundle.yaml_text.as_bytes())?;
    std::fs::write(
        out_dir.join(format!("{}.meta", bundle.root_file_name)),
        bundle.root_meta_text.as_bytes(),
    )?;
    for comp in &bundle.companion_files {
        let dest = out_dir.join(&comp.relative_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest, &comp.bytes)?;
    }
    Ok(bundle)
}

/// Builds a `.prefab` / `.unity` bundle and packs it with all companion assets & matching GUIDs
/// into a Unity Editor `.unitypackage` (`tar.gz`).
pub fn export_prefab_unitypackage(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    name: &str,
    out_path: &Path,
    opts: &PrefabExportOptions,
) -> ExportResult<PrefabExportBundle> {
    let bundle = build_prefab_bundle(workspace, roots, name, opts)?;
    let stem = sanitize_stem(name);
    let pkg_root = format!("{}/{stem}", opts.package_prefix.trim_end_matches('/'));

    let mut meta_map: HashMap<String, String> = HashMap::new();
    for comp in &bundle.companion_files {
        if let Some(base_rel) = comp.relative_path.strip_suffix(".meta") {
            meta_map.insert(
                base_rel.to_owned(),
                String::from_utf8_lossy(&comp.bytes).into_owned(),
            );
        }
    }

    let mut entries = Vec::with_capacity(1 + bundle.companion_files.len());
    let root_pkg_path = format!("{pkg_root}/{}", bundle.root_file_name);
    entries.push(UnityPackageInputEntry {
        guid: Some(deterministic_guid(&root_pkg_path)),
        asset_path: root_pkg_path,
        asset_bytes: bundle.yaml_text.as_bytes().to_vec(),
        meta_text: Some(bundle.root_meta_text.clone()),
    });

    for comp in &bundle.companion_files {
        if comp.relative_path.ends_with(".meta") {
            continue;
        }
        let pkg_path = format!("{pkg_root}/{}", comp.relative_path);
        entries.push(UnityPackageInputEntry {
            guid: Some(deterministic_guid(&pkg_path)),
            asset_path: pkg_path,
            asset_bytes: comp.bytes.clone(),
            meta_text: meta_map.get(&comp.relative_path).cloned(),
        });
    }

    write_unitypackage(out_path, &entries)?;
    Ok(bundle)
}

#[derive(Default)]
struct CompanionStats {
    materials: usize,
    textures: usize,
    meshes: usize,
}

fn build_positive_fileid_map(objects: &[(AssetRef, UnityObject)]) -> HashMap<(usize, i64), i64> {
    let mut map = HashMap::with_capacity(objects.len());
    let mut used = HashSet::with_capacity(objects.len());
    let mut next_synth = 1_000_000_000_000i64;

    for (r, _) in objects {
        let candidate = if r.path_id > 0 && !used.contains(&r.path_id) {
            r.path_id
        } else {
            while used.contains(&next_synth) {
                next_synth += 1;
            }
            let v = next_synth;
            next_synth += 1;
            v
        };
        used.insert(candidate);
        map.insert((r.file_index, r.path_id), candidate);
    }
    map
}

fn export_companion_object(
    workspace: &UnityWorkspaceData,
    dep_ref: AssetRef,
    package_root: &str,
    used_paths: &mut HashSet<String>,
    companion_files: &mut Vec<PrefabCompanionFile>,
    external_refs: &mut HashMap<(usize, i64), ExternalYamlRef>,
    stats: &mut CompanionStats,
) {
    if external_refs.contains_key(&(dep_ref.file_index, dep_ref.path_id)) {
        return;
    }
    let Some((loaded, info)) = workspace.object_info(dep_ref.file_index, dep_ref.path_id) else {
        return;
    };
    let unity_version = loaded.file.unity_version.clone();
    let Ok(mut obj) = read_object(loaded, info) else {
        return;
    };

    match obj.class.class_id {
        28 => {
            // Texture2D -> PNG + TextureImporter .meta
            let Ok(img) = decode_texture2d(
                &obj,
                &unity_version,
                &workspace.streams,
                TextureDecodeOptions::default(),
            ) else {
                return;
            };
            let Ok(png) = encode_png(&img) else {
                return;
            };
            let stem = unique_companion_stem(
                used_paths,
                "Textures",
                object_display_stem(&obj, "Texture", dep_ref.path_id),
                "png",
            );
            let rel = format!("Textures/{stem}.png");
            let guid = deterministic_guid(&format!("{package_root}/{rel}"));
            let meta = format!(
                "fileFormatVersion: 2\nguid: {guid}\nTextureImporter:\n  internalIDToNameTable: []\n  externalObjects: {{}}\n"
            );
            companion_files.push(PrefabCompanionFile {
                relative_path: rel.clone(),
                bytes: png,
            });
            companion_files.push(PrefabCompanionFile {
                relative_path: format!("{rel}.meta"),
                bytes: meta.into_bytes(),
            });
            external_refs.insert(
                (dep_ref.file_index, dep_ref.path_id),
                ExternalYamlRef {
                    file_id: 2_800_000,
                    guid,
                    asset_type: 3,
                },
            );
            stats.textures += 1;
        }
        43 => {
            // Mesh -> Binary FBX + ModelImporter .meta
            let Ok(mesh) = decode_mesh(&obj, &unity_version, &workspace.streams) else {
                return;
            };
            let Ok(fbx_bytes) = export_mesh_to_fbx(&mesh) else {
                return;
            };
            let stem = unique_companion_stem(
                used_paths,
                "Meshes",
                object_display_stem(&obj, "Mesh", dep_ref.path_id),
                "fbx",
            );
            let rel = format!("Meshes/{stem}.fbx");
            let guid = deterministic_guid(&format!("{package_root}/{rel}"));
            let meta = format!(
                "fileFormatVersion: 2\nguid: {guid}\nModelImporter:\n  fileIDToRecycleName:\n    4300000: {}\n  externalObjects: {{}}\n",
                mesh.name
            );
            companion_files.push(PrefabCompanionFile {
                relative_path: rel.clone(),
                bytes: fbx_bytes,
            });
            companion_files.push(PrefabCompanionFile {
                relative_path: format!("{rel}.meta"),
                bytes: meta.into_bytes(),
            });
            external_refs.insert(
                (dep_ref.file_index, dep_ref.path_id),
                ExternalYamlRef {
                    file_id: 4_300_000,
                    guid,
                    asset_type: 3,
                },
            );
            stats.meshes += 1;
        }
        74 => {
            // AnimationClip -> `.anim` YAML + .meta
            obj.class.anchor = "7400000".to_owned();
            let Ok(yaml) = serialize_unity_classes_yaml(&[obj.class.clone()]) else {
                return;
            };
            let stem = unique_companion_stem(
                used_paths,
                "Animations",
                object_display_stem(&obj, "Clip", dep_ref.path_id),
                "anim",
            );
            let rel = format!("Animations/{stem}.anim");
            let guid = deterministic_guid(&format!("{package_root}/{rel}"));
            let meta = format!(
                "fileFormatVersion: 2\nguid: {guid}\nNativeFormatImporter:\n  externalObjects: {{}}\n  mainObjectFileID: 7400000\n"
            );
            companion_files.push(PrefabCompanionFile {
                relative_path: rel.clone(),
                bytes: yaml.into_bytes(),
            });
            companion_files.push(PrefabCompanionFile {
                relative_path: format!("{rel}.meta"),
                bytes: meta.into_bytes(),
            });
            external_refs.insert(
                (dep_ref.file_index, dep_ref.path_id),
                ExternalYamlRef {
                    file_id: 7_400_000,
                    guid,
                    asset_type: 2,
                },
            );
        }
        48 => {
            // Shader -> `.shader` + ShaderImporter .meta
            let Ok(shader) = decode_shader(&obj) else {
                return;
            };
            let text = if !shader.shaderlab_text.is_empty() {
                shader.shaderlab_text
            } else {
                shader.decompiled_cg_text
            };
            if text.is_empty() {
                return;
            }
            let stem = unique_companion_stem(
                used_paths,
                "Shaders",
                object_display_stem(&obj, "Shader", dep_ref.path_id),
                "shader",
            );
            let rel = format!("Shaders/{stem}.shader");
            let guid = deterministic_guid(&format!("{package_root}/{rel}"));
            let meta = format!(
                "fileFormatVersion: 2\nguid: {guid}\nShaderImporter:\n  externalObjects: {{}}\n"
            );
            companion_files.push(PrefabCompanionFile {
                relative_path: rel.clone(),
                bytes: text.into_bytes(),
            });
            companion_files.push(PrefabCompanionFile {
                relative_path: format!("{rel}.meta"),
                bytes: meta.into_bytes(),
            });
            external_refs.insert(
                (dep_ref.file_index, dep_ref.path_id),
                ExternalYamlRef {
                    file_id: 4_800_000,
                    guid,
                    asset_type: 3,
                },
            );
        }
        _ => {}
    }
}

pub(crate) fn object_display_stem(obj: &UnityObject, fallback: &str, path_id: i64) -> String {
    let raw = obj
        .class
        .name()
        .or_else(|| obj.class.get("m_Name").and_then(UnityValue::as_str))
        .unwrap_or("");
    let cleaned = sanitize_stem(raw);
    if cleaned == "asset" || cleaned.is_empty() {
        format!("{fallback}_{path_id}")
    } else {
        cleaned
    }
}

fn unique_companion_stem(
    used_paths: &mut HashSet<String>,
    subdir: &str,
    base_stem: String,
    ext: &str,
) -> String {
    let mut candidate = base_stem.clone();
    let mut suffix = 2usize;
    while !used_paths.insert(format!("{subdir}/{candidate}.{ext}")) {
        candidate = format!("{base_stem}_{suffix}");
        suffix += 1;
    }
    candidate
}

fn is_pptr_map(map: &IndexMap<String, UnityValue>) -> bool {
    map.len() == 2 && map.contains_key("m_FileID") && map.contains_key("m_PathID")
}

/// Resolves a Unity `PPtr` JSON object `{ m_FileID, m_PathID }` into a workspace [`AssetRef`].
pub fn resolve_pptr_ref(
    workspace: &UnityWorkspaceData,
    source_file_index: usize,
    val: &UnityValue,
) -> Option<AssetRef> {
    let map = val.as_object()?;
    let path_id = map.get("m_PathID").and_then(extract_i64_field)?;
    if path_id == 0 {
        return None;
    }
    let file_id = map.get("m_FileID").and_then(extract_i32_field).unwrap_or(0);
    let target_file = workspace.resolve_external_file(source_file_index, file_id)?;
    Some(AssetRef {
        file_index: target_file,
        path_id,
    })
}

fn collect_external_pptrs(
    workspace: &UnityWorkspaceData,
    source_file_index: usize,
    val: &UnityValue,
    internal: &HashMap<(usize, i64), i64>,
    seen_ext: &mut HashSet<(usize, i64)>,
    out: &mut Vec<AssetRef>,
) {
    match val {
        UnityValue::Object(map) => {
            if is_pptr_map(map) {
                if let Some(target) = resolve_pptr_ref(workspace, source_file_index, val)
                    && !internal.contains_key(&(target.file_index, target.path_id))
                    && seen_ext.insert((target.file_index, target.path_id))
                {
                    out.push(target);
                }
                return;
            }
            for child in map.values() {
                collect_external_pptrs(workspace, source_file_index, child, internal, seen_ext, out);
            }
        }
        UnityValue::Array(arr) => {
            for item in arr {
                collect_external_pptrs(workspace, source_file_index, item, internal, seen_ext, out);
            }
        }
        _ => {}
    }
}

pub(crate) fn rewrite_pptrs_in_value(
    workspace: &UnityWorkspaceData,
    source_file_index: usize,
    val: &mut UnityValue,
    internal: &HashMap<(usize, i64), i64>,
    external: &HashMap<(usize, i64), ExternalYamlRef>,
) {
    match val {
        UnityValue::Object(map) => {
            if is_pptr_map(map) {
                let mut replacement = IndexMap::new();
                if let Some(target) = resolve_pptr_ref(workspace, source_file_index, &UnityValue::Object(map.clone())) {
                    if let Some(&internal_id) = internal.get(&(target.file_index, target.path_id)) {
                        replacement.insert("fileID".to_owned(), UnityValue::Integer(internal_id));
                    } else if let Some(ext_ref) = external.get(&(target.file_index, target.path_id)) {
                        replacement.insert("fileID".to_owned(), UnityValue::Integer(ext_ref.file_id));
                        replacement.insert("guid".to_owned(), UnityValue::String(ext_ref.guid.clone()));
                        replacement.insert("type".to_owned(), UnityValue::Integer(ext_ref.asset_type));
                    } else {
                        replacement.insert("fileID".to_owned(), UnityValue::Integer(0));
                    }
                } else {
                    replacement.insert("fileID".to_owned(), UnityValue::Integer(0));
                }
                *map = replacement;
                return;
            }
            for child in map.values_mut() {
                rewrite_pptrs_in_value(workspace, source_file_index, child, internal, external);
            }
        }
        UnityValue::Array(arr) => {
            for item in arr {
                rewrite_pptrs_in_value(workspace, source_file_index, item, internal, external);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_multi_object_prefab_yaml_with_positive_anchors_and_rewrites_pptrs() {
        let mut go = UnityClass::new(1, "GameObject".to_owned(), "100".to_owned());
        go.set("m_Name".to_owned(), UnityValue::String("HeroRoot".to_owned()));
        let mut comp_entry = IndexMap::new();
        let mut comp_pptr = IndexMap::new();
        comp_pptr.insert("m_FileID".to_owned(), UnityValue::Integer(0));
        comp_pptr.insert("m_PathID".to_owned(), UnityValue::Integer(-42));
        comp_entry.insert("component".to_owned(), UnityValue::Object(comp_pptr));
        go.set(
            "m_Component".to_owned(),
            UnityValue::Array(vec![UnityValue::Object(comp_entry)]),
        );

        let mut tr = UnityClass::new(4, "Transform".to_owned(), "1000000000000".to_owned());
        let mut go_pptr = IndexMap::new();
        go_pptr.insert("fileID".to_owned(), UnityValue::Integer(100));
        tr.set("m_GameObject".to_owned(), UnityValue::Object(go_pptr));

        let yaml = serialize_unity_classes_yaml(&[go, tr]).expect("yaml ok");
        assert!(yaml.starts_with("%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:"));
        assert!(yaml.contains("--- !u!1 &100"));
        assert!(yaml.contains("--- !u!4 &1000000000000"));
        assert!(yaml.contains("HeroRoot"));
    }

    #[test]
    fn rewrites_internal_and_null_pptr_maps_cleanly() {
        let ws = UnityWorkspaceData::new(std::path::PathBuf::from("test"));
        let mut internal = HashMap::new();
        internal.insert((0, -99), 555_i64);
        let mut external = HashMap::new();
        external.insert(
            (0, 777),
            ExternalYamlRef {
                file_id: 2_100_000,
                guid: "0123456789abcdef0123456789abcdef".to_owned(),
                asset_type: 2,
            },
        );

        let mut p_internal = IndexMap::new();
        p_internal.insert("m_FileID".to_owned(), UnityValue::Integer(0));
        p_internal.insert("m_PathID".to_owned(), UnityValue::Integer(-99));
        let mut v = UnityValue::Object(p_internal);
        rewrite_pptrs_in_value(&ws, 0, &mut v, &internal, &external);
        let m = v.as_object().expect("obj");
        assert_eq!(m.get("fileID").and_then(UnityValue::as_i64), Some(555));
        assert!(!m.contains_key("m_FileID"));

        let mut p_null = IndexMap::new();
        p_null.insert("m_FileID".to_owned(), UnityValue::Integer(0));
        p_null.insert("m_PathID".to_owned(), UnityValue::Integer(0));
        let mut v2 = UnityValue::Object(p_null);
        rewrite_pptrs_in_value(&ws, 0, &mut v2, &internal, &external);
        assert_eq!(
            v2.as_object().and_then(|o| o.get("fileID")).and_then(UnityValue::as_i64),
            Some(0)
        );
    }

    #[test]
    fn serializes_animator_controller_yaml_with_blend_tree_and_transitions() {
        use devx_codecs::animator::{
            AnimatorClipLink, AnimatorLayerDef, AnimatorParamDef, AnimatorStateDef,
            AnimatorStateMachineDef, AnimatorTransitionCondition, AnimatorTransitionDef,
            BlendTreeNodeDef,
        };

        let clip_a = AssetRef {
            file_index: 0,
            path_id: 101,
        };
        let clip_b = AssetRef {
            file_index: 0,
            path_id: 102,
        };
        let mut clip_refs = HashMap::new();
        clip_refs.insert(
            (0, 101),
            ExternalYamlRef {
                file_id: 7_400_000,
                guid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                asset_type: 2,
            },
        );
        clip_refs.insert(
            (0, 102),
            ExternalYamlRef {
                file_id: 7_400_000,
                guid: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
                asset_type: 2,
            },
        );

        let ctrl = DecodedAnimatorController {
            name: "HeroController".to_owned(),
            parameters: vec![AnimatorParamDef {
                name: "Speed".to_owned(),
                kind_label: "Float",
                default_display: "0.0".to_owned(),
            }],
            layers: vec![AnimatorLayerDef {
                name: "Base Layer".to_owned(),
                state_machine_idx: 0,
                default_weight: 1.0,
                blending_mode: 0,
            }],
            state_machines: vec![AnimatorStateMachineDef {
                default_state: 0,
                states: vec![
                    AnimatorStateDef {
                        index: 0,
                        name: "Locomotion".to_owned(),
                        tag: String::new(),
                        speed: 1.0,
                        is_loop: true,
                        is_default: true,
                        leaf_clip_indices: vec![0, 1],
                        blend_tree: vec![
                            BlendTreeNodeDef {
                                kind: BlendTreeKind::Blend1D,
                                clip_id: None,
                                param_x: Some("Speed".to_owned()),
                                param_y: None,
                                child_indices: vec![1, 2],
                                thresholds_1d: vec![0.0, 1.0],
                                positions_2d: Vec::new(),
                                direct_params: Vec::new(),
                                normalized_direct: false,
                            },
                            BlendTreeNodeDef {
                                kind: BlendTreeKind::SimpleClip,
                                clip_id: Some(0),
                                param_x: None,
                                param_y: None,
                                child_indices: Vec::new(),
                                thresholds_1d: Vec::new(),
                                positions_2d: Vec::new(),
                                direct_params: Vec::new(),
                                normalized_direct: false,
                            },
                            BlendTreeNodeDef {
                                kind: BlendTreeKind::SimpleClip,
                                clip_id: Some(1),
                                param_x: None,
                                param_y: None,
                                child_indices: Vec::new(),
                                thresholds_1d: Vec::new(),
                                positions_2d: Vec::new(),
                                direct_params: Vec::new(),
                                normalized_direct: false,
                            },
                        ],
                        transitions: vec![AnimatorTransitionDef {
                            destination_state_idx: Some(1),
                            is_exit: false,
                            is_any_state: false,
                            has_exit_time: false,
                            exit_time: 1.0,
                            duration: 0.15,
                            conditions: vec![AnimatorTransitionCondition {
                                param_name: "Speed".to_owned(),
                                mode_label: "<",
                                threshold: 0.01,
                            }],
                        }],
                    },
                    AnimatorStateDef {
                        index: 1,
                        name: "Idle".to_owned(),
                        tag: String::new(),
                        speed: 1.0,
                        is_loop: true,
                        is_default: false,
                        leaf_clip_indices: vec![0],
                        blend_tree: Vec::new(),
                        transitions: Vec::new(),
                    },
                ],
                any_state_transitions: Vec::new(),
            }],
            clip_links: vec![
                AnimatorClipLink {
                    index: 0,
                    asset_ref: Some(clip_a),
                    name_hint: "Idle".to_owned(),
                },
                AnimatorClipLink {
                    index: 1,
                    asset_ref: Some(clip_b),
                    name_hint: "Run".to_owned(),
                },
            ],
        };

        let yaml = build_animator_controller_yaml(&ctrl, &clip_refs).expect("controller yaml ok");
        assert!(yaml.contains("--- !u!91 &9100000"));
        assert!(yaml.contains("m_AnimatorParameters:"));
        assert!(yaml.contains("m_AnimatorLayers:"));
        assert!(yaml.contains("--- !u!1107 "));
        assert!(yaml.contains("m_ChildStates:"));
        assert!(yaml.contains("--- !u!1102 "));
        assert!(yaml.contains("m_Transitions:"));
        assert!(yaml.contains("--- !u!1101 "));
        assert!(yaml.contains("--- !u!206 "), "yaml:\n{yaml}");
        assert!(yaml.contains("m_BlendType: 0"), "yaml:\n{yaml}");
        assert!(!yaml.contains("{...}"), "yaml:\n{yaml}");
        assert!(yaml.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), "yaml:\n{yaml}");
        assert!(yaml.contains("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"), "yaml:\n{yaml}");
    }
}
