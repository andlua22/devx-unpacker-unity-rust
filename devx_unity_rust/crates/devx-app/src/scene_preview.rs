//! Hierarchical 3D preview assembly (`GameObject` / `Transform` / `MeshFilter` /
//! `SkinnedMeshRenderer` / `Material` / `Texture2D`) for the interactive viewport.

use devx_codecs::animation::decode_animation_clip;
use devx_codecs::animator::{
    AnimatorSimState, AnimatorSimStep, DecodedAnimatorController, decode_animator_controller,
};
use devx_codecs::gltf_scene::GltfSceneOptions;
use devx_codecs::material::{MaterialAlphaMode, MaterialTextureSlot, decode_material};
use devx_codecs::mesh::{DecodedBlendShape, apply_blend_shapes, decode_mesh_full};
use devx_codecs::particle::DecodedParticleSystem;
use devx_codecs::scene_extract::{
    EvaluatedScenePose, SceneDocument, SceneMaterial, SceneMesh, blend_scene_poses,
    blend_scene_poses_weighted, deform_node_mesh_world_with_weights, evaluate_scene_pose,
    extract_unity_scene, resolve_scene_root_transforms, resolve_standalone_mesh_roots,
};
use devx_codecs::texture::{TextureDecodeOptions, decode_texture2d};
use devx_codecs::terrain::DecodedTerrainData;
use devx_core::ReferenceGraph;
use devx_plugin_api::{AssetRef, DecodedImage, DecodedMesh, SubMeshSlice};
use devx_unity_io::{
    UnityObject, UnityValue, UnityWorkspaceData, extract_i32_field, extract_i64_field, read_object,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Base un-morphed vertex positions and normals cached for static BlendShape parts.
pub type StaticPositionsNormals = (Vec<[f32; 3]>, Vec<[f32; 3]>);

/// Resolved PBR material slot attached to one or more submeshes of a [`ViewportMeshPart`].
#[derive(Clone)]
pub struct ViewportMaterialSlot {
    pub name: String,
    pub albedo_image: Option<Arc<DecodedImage>>,
    pub albedo_texture: Option<egui::TextureHandle>,
    pub normal_image: Option<Arc<DecodedImage>>,
    pub normal_texture: Option<egui::TextureHandle>,
    pub emissive_image: Option<Arc<DecodedImage>>,
    pub emissive_texture: Option<egui::TextureHandle>,
    pub metallic_gloss_image: Option<Arc<DecodedImage>>,
    pub metallic_gloss_texture: Option<egui::TextureHandle>,
    pub base_color: [f32; 4],
    pub emissive_color: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    pub alpha_mode: MaterialAlphaMode,
    pub double_sided: bool,
}

impl Default for ViewportMaterialSlot {
    fn default() -> Self {
        Self {
            name: String::new(),
            albedo_image: None,
            albedo_texture: None,
            normal_image: None,
            normal_texture: None,
            emissive_image: None,
            emissive_texture: None,
            metallic_gloss_image: None,
            metallic_gloss_texture: None,
            base_color: [1.0, 1.0, 1.0, 1.0],
            emissive_color: [0.0, 0.0, 0.0],
            metallic: 0.0,
            roughness: 0.7,
            alpha_mode: MaterialAlphaMode::Opaque,
            double_sided: false,
        }
    }
}

impl ViewportMaterialSlot {
    /// Creates a minimal diffuse slot from optional albedo texture and RGBA tint.
    pub fn from_albedo(albedo_image: Option<Arc<DecodedImage>>, base_color: [f32; 4]) -> Self {
        Self {
            albedo_image,
            base_color,
            ..Default::default()
        }
    }
}

/// Triangle index span mapped to a material slot inside [`ViewportMeshPart::materials`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewportSubMesh {
    pub index_start: usize,
    pub index_count: usize,
    pub material_slot: usize,
}

/// One renderable mesh part in a 3D hierarchy viewport with per-submesh PBR material slots.
#[derive(Clone)]
pub struct ViewportMeshPart {
    pub label: String,
    pub mesh: DecodedMesh,
    pub albedo_image: Option<Arc<DecodedImage>>,
    pub albedo_texture: Option<egui::TextureHandle>,
    pub base_color: [f32; 4],
    pub materials: Vec<ViewportMaterialSlot>,
    pub submeshes: Vec<ViewportSubMesh>,
    pub visible: bool,
    pub scene_node_idx: Option<usize>,
    pub blend_shapes: Vec<DecodedBlendShape>,
    pub blend_weights: Vec<f32>,
    pub base_static_geometry: Option<StaticPositionsNormals>,
}

impl ViewportMeshPart {
    /// Creates a visible static viewport mesh part with a single diffuse material slot.
    pub fn static_part(
        label: String,
        mesh: DecodedMesh,
        albedo_image: Option<Arc<DecodedImage>>,
        base_color: [f32; 4],
    ) -> Self {
        let slot = ViewportMaterialSlot::from_albedo(albedo_image, base_color);
        let submeshes = build_default_submeshes(&mesh, 1);
        Self::from_materials(label, mesh, vec![slot], submeshes, None)
    }

    /// Creates a visible viewport mesh part with explicit per-submesh PBR material slots.
    pub fn from_materials(
        label: String,
        mesh: DecodedMesh,
        mut materials: Vec<ViewportMaterialSlot>,
        mut submeshes: Vec<ViewportSubMesh>,
        scene_node_idx: Option<usize>,
    ) -> Self {
        if materials.is_empty() {
            materials.push(ViewportMaterialSlot::default());
        }
        if submeshes.is_empty() {
            submeshes = build_default_submeshes(&mesh, materials.len());
        }
        let first = &materials[0];
        let albedo_image = first.albedo_image.clone();
        let base_color = first.base_color;
        Self {
            label,
            mesh,
            albedo_image,
            albedo_texture: None,
            base_color,
            materials,
            submeshes,
            visible: true,
            scene_node_idx,
            blend_shapes: Vec::new(),
            blend_weights: Vec::new(),
            base_static_geometry: None,
        }
    }

    /// Returns the resolved material slot for `submesh_idx`, falling back to slot 0 / default.
    pub fn material_for_submesh(&self, submesh: &ViewportSubMesh) -> Option<&ViewportMaterialSlot> {
        self.materials
            .get(submesh.material_slot)
            .or_else(|| self.materials.first())
    }

    /// Attaches Unity blendshape channels and optional normalized initial weights (`0.0..=1.0`).
    pub fn with_blend_shapes(
        mut self,
        blend_shapes: Vec<DecodedBlendShape>,
        initial_weights: Option<Vec<f32>>,
    ) -> Self {
        if !blend_shapes.is_empty() {
            let mut weights: Vec<f32> = blend_shapes
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    initial_weights
                        .as_ref()
                        .and_then(|ws| ws.get(i).copied())
                        .unwrap_or(b.default_weight)
                        .clamp(0.0, 1.0)
                })
                .collect();
            weights.resize(blend_shapes.len(), 0.0);
            if self.scene_node_idx.is_none() {
                self.base_static_geometry =
                    Some((self.mesh.positions.clone(), self.mesh.normals.clone()));
            }
            self.blend_shapes = blend_shapes;
            self.blend_weights = weights;
        }
        self
    }
}

/// Complete multi-part 3D scene/character/mesh preview ready for `render_scene_viewport`.
#[derive(Clone)]
pub struct HierarchyPreviewData {
    pub title: String,
    pub root_ref: AssetRef,
    pub parts: Vec<ViewportMeshPart>,
    pub bone_segments: Vec<([f32; 3], [f32; 3])>,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    pub scene_doc: Option<Arc<SceneDocument>>,
    pub anim_clip_idx: Option<usize>,
    pub anim_playing: bool,
    pub anim_looping: bool,
    pub anim_time_secs: f32,
    pub anim_speed: f32,
    pub last_ui_time: Option<f64>,
    pub show_parts_panel: bool,
    pub show_blendshapes_panel: bool,
    pub attached_controller: Option<DecodedAnimatorController>,
    pub animator_sim: Option<AnimatorSimState>,
    pub animator_mode: bool,
    pub controller_state_clips: HashMap<(usize, usize), usize>,
    pub controller_link_clips: HashMap<usize, usize>,
}

impl HierarchyPreviewData {
    /// Constructs a static (non-animated) preview from prebuilt parts and bounds.
    pub fn static_from_parts(
        title: String,
        root_ref: AssetRef,
        parts: Vec<ViewportMeshPart>,
        bone_segments: Vec<([f32; 3], [f32; 3])>,
        bounds_min: [f32; 3],
        bounds_max: [f32; 3],
    ) -> Self {
        Self {
            title,
            root_ref,
            parts,
            bone_segments,
            bounds_min,
            bounds_max,
            scene_doc: None,
            anim_clip_idx: None,
            anim_playing: false,
            anim_looping: true,
            anim_time_secs: 0.0,
            anim_speed: 1.0,
            last_ui_time: None,
            show_parts_panel: false,
            show_blendshapes_panel: false,
            attached_controller: None,
            animator_sim: None,
            animator_mode: false,
            controller_state_clips: HashMap::new(),
            controller_link_clips: HashMap::new(),
        }
    }

    /// Returns `true` when at least one mesh part exposes morph target channels.
    pub fn has_blend_shapes(&self) -> bool {
        self.parts.iter().any(|p| !p.blend_shapes.is_empty())
    }
}

fn write_part_positions_normals(
    mesh: &mut DecodedMesh,
    positions: impl IntoIterator<Item = [f32; 3]>,
    normals: impl IntoIterator<Item = [f32; 3]>,
    negate_x: bool,
) {
    let mut min_v = [f32::INFINITY; 3];
    let mut max_v = [f32::NEG_INFINITY; 3];
    mesh.positions.clear();
    for [x, y, z] in positions {
        let p = [if negate_x { -x } else { x }, y, z];
        for i in 0..3 {
            min_v[i] = min_v[i].min(p[i]);
            max_v[i] = max_v[i].max(p[i]);
        }
        mesh.positions.push(p);
    }
    mesh.normals = normals
        .into_iter()
        .map(|[nx, ny, nz]| [if negate_x { -nx } else { nx }, ny, nz])
        .collect();
    if min_v[0].is_finite() {
        mesh.bounds_min = min_v;
        mesh.bounds_max = max_v;
    }
}

fn apply_evaluated_pose_to_preview(
    data: &mut HierarchyPreviewData,
    doc: Option<&SceneDocument>,
    pose: Option<&EvaluatedScenePose>,
) {
    if let Some(p) = pose {
        data.bone_segments = p
            .bone_segments
            .iter()
            .map(|&(a, b)| ([-a[0], a[1], a[2]], [-b[0], b[1], b[2]]))
            .collect();
    }

    for part in &mut data.parts {
        if let (Some(scene), Some(p), Some(node_idx)) = (doc, pose, part.scene_node_idx) {
            let weights = (!part.blend_shapes.is_empty()).then_some(part.blend_weights.as_slice());
            if let Some(deformed) = deform_node_mesh_world_with_weights(scene, node_idx, p, weights)
            {
                write_part_positions_normals(
                    &mut part.mesh,
                    deformed.positions,
                    deformed.normals,
                    true,
                );
            }
        } else if !part.blend_shapes.is_empty()
            && let Some((base_pos, base_norm)) = part.base_static_geometry.as_ref()
        {
            let (morphed_pos, morphed_norm) =
                apply_blend_shapes(base_pos, base_norm, &part.blend_shapes, &part.blend_weights);
            write_part_positions_normals(&mut part.mesh, morphed_pos, morphed_norm, false);
        }
    }
}

/// Evaluates [`SceneDocument`] skeletal/TRS animation at `(clip_idx, time_secs)` and updates all
/// linked [`ViewportMeshPart`]s (including active BlendShape weights) + bone overlay segments.
pub fn apply_scene_animation_pose(
    data: &mut HierarchyPreviewData,
    clip_idx: Option<usize>,
    time_secs: f32,
) {
    if let Some(doc) = data.scene_doc.clone() {
        let pose = evaluate_scene_pose(&doc, clip_idx, time_secs);
        apply_evaluated_pose_to_preview(data, Some(&doc), Some(&pose));
    } else {
        apply_evaluated_pose_to_preview(data, None, None);
    }
}

/// Evaluates and cross-fades between `(clip_a, time_a)` and `(clip_b, time_b)` with blend weight
/// `alpha_to_b` (`0.0..=1.0`), updating all parts (including BlendShape weights) and bone segments.
pub fn apply_scene_blended_pose(
    data: &mut HierarchyPreviewData,
    clip_a: Option<usize>,
    time_a: f32,
    clip_b: Option<usize>,
    time_b: f32,
    alpha_to_b: f32,
) {
    if let Some(doc) = data.scene_doc.clone() {
        let pose_a = evaluate_scene_pose(&doc, clip_a, time_a);
        let pose_b = evaluate_scene_pose(&doc, clip_b, time_b);
        let blended = blend_scene_poses(&doc, &pose_a, &pose_b, alpha_to_b);
        apply_evaluated_pose_to_preview(data, Some(&doc), Some(&blended));
    } else {
        apply_evaluated_pose_to_preview(data, None, None);
    }
}

/// Decodes missing linked `AnimationClip`s from `controller`, builds state->clip lookup mappings,
/// and initializes [`AnimatorSimState`] for live state-machine playback on `data`.
pub fn attach_controller_to_preview(
    data: &mut HierarchyPreviewData,
    workspace: &UnityWorkspaceData,
    controller: DecodedAnimatorController,
) {
    if let Some(doc_arc) = data.scene_doc.as_mut() {
        let doc = Arc::make_mut(doc_arc);
        let mut resolved_link_names: HashMap<usize, String> = HashMap::new();

        for link in &controller.clip_links {
            if !link.name_hint.is_empty() {
                resolved_link_names.insert(link.index, link.name_hint.clone());
            }
            let Some(clip_ref) = link.asset_ref else {
                continue;
            };
            let already_named = !link.name_hint.is_empty()
                && doc.animations.iter().any(|a| a.name == link.name_hint);
            if already_named {
                continue;
            }
            let Some((loaded, info)) = workspace.object_info(clip_ref.file_index, clip_ref.path_id)
            else {
                continue;
            };
            if info.type_id != 74 {
                continue;
            }
            let Ok(obj) = read_object(loaded, info) else {
                continue;
            };
            if let Ok(decoded) = decode_animation_clip(&obj) {
                resolved_link_names.insert(link.index, decoded.name.clone());
                if !doc.animations.iter().any(|a| a.name == decoded.name) {
                    doc.attach_decoded_clip(&decoded);
                }
            }
        }

        let mut link_clips = HashMap::new();
        for (&link_idx, cname) in &resolved_link_names {
            if let Some(pos) = doc
                .animations
                .iter()
                .position(|a| a.name.eq_ignore_ascii_case(cname))
            {
                link_clips.insert(link_idx, pos);
            }
        }

        let mut state_clips = HashMap::new();
        for (layer_idx, layer) in controller.layers.iter().enumerate() {
            let Some(sm) = controller.state_machines.get(layer.state_machine_idx) else {
                continue;
            };
            for st in &sm.states {
                let mut found = None;
                for link_idx in &st.leaf_clip_indices {
                    if let Some(&pos) = link_clips.get(link_idx) {
                        found = Some(pos);
                        break;
                    }
                }
                if found.is_none() {
                    found = doc
                        .animations
                        .iter()
                        .position(|a| a.name.eq_ignore_ascii_case(&st.name));
                }
                if let Some(anim_idx) = found {
                    state_clips.insert((layer_idx, st.index), anim_idx);
                }
            }
        }
        data.controller_state_clips = state_clips;
        data.controller_link_clips = link_clips;
    }

    let sim = AnimatorSimState::from_controller(&controller, 0);
    data.attached_controller = Some(controller);
    data.animator_sim = Some(sim);
}

/// Advances `data.animator_sim` by `dt` seconds and applies the active/BlendTree/cross-faded pose.
pub fn step_and_apply_animator_sim(
    data: &mut HierarchyPreviewData,
    dt: f32,
) -> Option<AnimatorSimStep> {
    let ctrl = data.attached_controller.as_ref()?;
    let doc = data.scene_doc.as_ref()?.clone();
    let mut sim = data.animator_sim.take()?;
    let layer_idx = sim.layer_idx;

    let state_duration = |state_idx: usize, params: &HashMap<String, devx_codecs::animator::SimParamValue>| -> f32 {
        let sm = ctrl
            .layers
            .get(layer_idx)
            .and_then(|l| ctrl.state_machines.get(l.state_machine_idx));
        if let Some(st) = sm.and_then(|m| m.states.iter().find(|s| s.index == state_idx)) {
            let weights = st.evaluate_clip_weights(params);
            let mut w_sum = 0.0f32;
            let mut d_sum = 0.0f32;
            for (link_idx, wt) in weights {
                if let Some(&anim_idx) = data.controller_link_clips.get(&link_idx)
                    && let Some(clip) = doc.animations.get(anim_idx)
                {
                    d_sum += clip.duration_secs.max(1e-3) * wt;
                    w_sum += wt;
                }
            }
            if w_sum > 1e-4 {
                return (d_sum / w_sum).max(1e-3);
            }
        }
        data.controller_state_clips
            .get(&(layer_idx, state_idx))
            .and_then(|&anim_idx| doc.animations.get(anim_idx))
            .map_or(1.0, |clip| clip.duration_secs.max(1e-3))
    };

    let params_snapshot = sim.params.clone();
    let step = sim.step(ctrl, dt, |state_idx| state_duration(state_idx, &params_snapshot));
    let final_params = sim.params.clone();
    data.animator_sim = Some(sim);

    let state_poses = |state_idx: usize, clip_weights: &[(usize, f32)], state_time: f32, layer_alpha: f32| -> Vec<(EvaluatedScenePose, f32)> {
        let total_dur = state_duration(state_idx, &final_params).max(1e-3);
        let phase = (state_time / total_dur).clamp(0.0, 1.0);
        let mut out = Vec::new();
        for &(link_idx, wt) in clip_weights {
            if let Some(&anim_idx) = data.controller_link_clips.get(&link_idx)
                && let Some(clip) = doc.animations.get(anim_idx)
            {
                let t = phase * clip.duration_secs.max(1e-3);
                out.push((evaluate_scene_pose(&doc, Some(anim_idx), t), wt * layer_alpha));
            }
        }
        if out.is_empty()
            && let Some(&anim_idx) = data.controller_state_clips.get(&(layer_idx, state_idx))
        {
            out.push((evaluate_scene_pose(&doc, Some(anim_idx), state_time), layer_alpha));
        }
        out
    };

    let mut poses = if let Some(target_idx) = step.transition_target_idx {
        let mut combined = state_poses(
            step.current_state_idx,
            &step.state_clip_weights,
            step.state_time_secs,
            1.0 - step.transition_alpha,
        );
        combined.extend(state_poses(
            target_idx,
            &step.target_clip_weights,
            step.transition_time_secs,
            step.transition_alpha,
        ));
        combined
    } else {
        state_poses(
            step.current_state_idx,
            &step.state_clip_weights,
            step.state_time_secs,
            1.0,
        )
    };

    if poses.len() == 1 {
        let pose = &poses.remove(0).0;
        apply_evaluated_pose_to_preview(data, Some(&doc), Some(pose));
    } else if poses.len() > 1 {
        let refs: Vec<(&EvaluatedScenePose, f32)> = poses.iter().map(|(p, w)| (p, *w)).collect();
        let blended = blend_scene_poses_weighted(&doc, &refs);
        apply_evaluated_pose_to_preview(data, Some(&doc), Some(&blended));
    } else {
        let clip_a = data
            .controller_state_clips
            .get(&(layer_idx, step.current_state_idx))
            .copied();
        apply_scene_animation_pose(data, clip_a, step.state_time_secs);
    }

    Some(step)
}

/// Decodes a hierarchical 3D scene/prefab or textured/rigged mesh preview if materials, bones,
/// or child meshes exist for `root_ref`. Returns `None` when no 3D renderables are attached
/// (or for plain standalone `Mesh` assets that have no material links).
pub fn build_hierarchy_preview(
    workspace: &UnityWorkspaceData,
    graph: Option<&ReferenceGraph>,
    root_ref: AssetRef,
    max_parts: usize,
) -> Option<HierarchyPreviewData> {
    let mut ctx = PreviewBuildCtx {
        workspace,
        tex_cache: HashMap::new(),
        visited_transforms: HashSet::new(),
        parts: Vec::new(),
        bone_segments: Vec::new(),
        max_parts: max_parts.max(1),
    };

    let (loaded, info) = workspace.object_info(root_ref.file_index, root_ref.path_id)?;
    let root_obj = read_object(loaded, info).ok()?;
    let unity_ver = &loaded.file.unity_version;
    let root_class = info.type_id;

    // Special case: standalone Mesh (43) — see if reverse graph locates a bound Material/SMR.
    if root_class == 43 {
        let mut mat_slots: Vec<ViewportMaterialSlot> = Vec::new();
        let mut renderer_go: Option<AssetRef> = None;
        let mut smr_weights: Option<Vec<f32>> = None;

        if let Some(g) = graph
            && let Some(edges) = g.reverse.get(&root_ref)
        {
            for edge in edges {
                let Some((rl, ri)) =
                    workspace.object_info(edge.from.file_index, edge.from.path_id)
                else {
                    continue;
                };
                if ri.type_id == 137 {
                    // SkinnedMeshRenderer owns both mesh and materials directly.
                    if let Ok(smr_obj) = read_object(rl, ri) {
                        mat_slots = ctx.resolve_materials(edge.from.file_index, &smr_obj);
                        renderer_go = ctx.resolve_pptr(
                            edge.from.file_index,
                            smr_obj.class.properties().get("m_GameObject"),
                        );
                        smr_weights = extract_smr_blend_weights(&smr_obj);
                        if mat_slots.iter().any(|m| m.albedo_image.is_some()) {
                            break;
                        }
                    }
                } else if ri.type_id == 33
                    && let Ok(mf_obj) = read_object(rl, ri)
                    && let Some(go_ref) =
                        ctx.resolve_pptr(edge.from.file_index, mf_obj.class.properties().get("m_GameObject"))
                    && let Some((gl, gi)) = workspace.object_info(go_ref.file_index, go_ref.path_id)
                    && let Ok(go_obj) = read_object(gl, gi)
                {
                    let (mr_ref, mr_obj) = ctx.find_component_of_type(go_ref.file_index, &go_obj, 23)?;
                    mat_slots = ctx.resolve_materials(mr_ref.file_index, &mr_obj);
                    renderer_go = Some(go_ref);
                    if mat_slots.iter().any(|m| m.albedo_image.is_some()) {
                        break;
                    }
                }
            }
        }

        if mat_slots.is_empty() {
            return None;
        }

        let full = decode_mesh_full(&root_obj, unity_ver, &workspace.streams).ok()?;
        let (bounds_min, bounds_max) = (full.base.bounds_min, full.base.bounds_max);
        let title = full.base.name.clone();
        let vp_subs = build_default_submeshes(&full.base, mat_slots.len());
        ctx.parts.push(
            ViewportMeshPart::from_materials(title.clone(), full.base, mat_slots, vp_subs, None)
                .with_blend_shapes(full.blend_shapes, smr_weights),
        );
        return Some(HierarchyPreviewData::static_from_parts(
            title,
            renderer_go.unwrap_or(root_ref),
            ctx.parts,
            Vec::new(),
            bounds_min,
            bounds_max,
        ));
    }

    let start_trs: Vec<AssetRef> = resolve_scene_root_transforms(workspace, &[root_ref])
        .into_iter()
        .map(|(file_index, path_id)| AssetRef {
            file_index,
            path_id,
        })
        .collect();
    let standalone_meshes = if start_trs.is_empty() && root_class == 142 {
        resolve_standalone_mesh_roots(workspace, &[root_ref])
    } else {
        Vec::new()
    };
    if start_trs.is_empty() && standalone_meshes.is_empty() {
        return None;
    }

    let title = if root_class == 142 {
        root_obj
            .name()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                workspace
                    .files
                    .get(root_ref.file_index)
                    .and_then(|f| f.container_name.clone())
            })
            .unwrap_or_else(|| "AssetBundle Scene".to_owned())
    } else if let Some(&first_tr) = start_trs.first() {
        ctx.transform_title(first_tr)
            .unwrap_or_else(|| "Hierarchy".to_owned())
    } else {
        "Mesh Bundle".to_owned()
    };

    // Prefer unified right-handed SceneDocument extractor so rigged/skinned/animated characters work live.
    let scene_opts = GltfSceneOptions {
        include_animations: true,
        include_materials: true,
        include_textures: true,
        max_texture_dim: Some(1024),
        ..GltfSceneOptions::default()
    };
    let scene_roots = if !start_trs.is_empty() {
        &start_trs
    } else {
        &standalone_meshes
    };
    if let Ok(doc) = extract_unity_scene(workspace, scene_roots, &[], &scene_opts) {
        let initial_clip = (!doc.animations.is_empty()).then_some(0);
        let pose = evaluate_scene_pose(&doc, None, 0.0);
        let mut parts = Vec::new();
        for (node_idx, node) in doc.nodes.iter().enumerate() {
            if parts.len() >= ctx.max_parts {
                break;
            }
            if let Some(mesh_idx) = node.mesh_idx
                && let Some(smesh) = doc.meshes.get(mesh_idx)
                && let Some(part) = build_part_from_scene_node(&doc, node_idx, smesh, &pose)
            {
                parts.push(part);
            }
        }
        if !parts.is_empty() {
            let bones: Vec<([f32; 3], [f32; 3])> = pose
                .bone_segments
                .iter()
                .map(|&(a, b)| ([-a[0], a[1], a[2]], [-b[0], b[1], b[2]]))
                .collect();
            let (bounds_min, bounds_max) = compute_combined_bounds(&parts, &bones);
            let mut preview = HierarchyPreviewData::static_from_parts(
                title,
                root_ref,
                parts,
                bones,
                bounds_min,
                bounds_max,
            );
            preview.scene_doc = Some(Arc::new(doc));
            preview.anim_clip_idx = initial_clip;
            if let Some(ctrl) = start_trs
                .iter()
                .find_map(|&tr| find_attached_animator_controller(&ctx, workspace, tr))
            {
                attach_controller_to_preview(&mut preview, workspace, ctrl);
                if preview.anim_clip_idx.is_none()
                    && let Some(doc_ref) = preview.scene_doc.as_ref()
                    && !doc_ref.animations.is_empty()
                {
                    preview.anim_clip_idx = Some(0);
                }
            }
            return Some(preview);
        }
    }

    for tr in start_trs {
        ctx.walk_transform(tr, IDENTITY_MAT4, None, 0);
    }

    if ctx.parts.is_empty() {
        return None;
    }

    let (bounds_min, bounds_max) = compute_combined_bounds(&ctx.parts, &ctx.bone_segments);
    Some(HierarchyPreviewData::static_from_parts(
        title,
        root_ref,
        ctx.parts,
        ctx.bone_segments,
        bounds_min,
        bounds_max,
    ))
}

fn build_part_from_scene_node(
    doc: &SceneDocument,
    node_idx: usize,
    smesh: &SceneMesh,
    pose: &EvaluatedScenePose,
) -> Option<ViewportMeshPart> {
    let blend_shapes = smesh.blend_shapes.clone();
    let blend_weights: Vec<f32> = blend_shapes
        .iter()
        .map(|b| b.default_weight.clamp(0.0, 1.0))
        .collect();
    let weights_opt = (!blend_shapes.is_empty()).then_some(blend_weights.as_slice());
    let deformed = deform_node_mesh_world_with_weights(doc, node_idx, pose, weights_opt)?;

    // SceneMesh stores RH reversed triangles `[a, c, b]`; restore Unity LH winding `[a, b, c]`.
    let mut indices = Vec::with_capacity(smesh.indices.len());
    for tri in smesh.indices.as_chunks::<3>().0 {
        indices.push(tri[0]);
        indices.push(tri[2]);
        indices.push(tri[1]);
    }

    let mut materials: Vec<ViewportMaterialSlot> = Vec::new();
    let mut doc_mat_to_slot: HashMap<usize, usize> = HashMap::new();
    let mut vp_submeshes: Vec<ViewportSubMesh> = Vec::with_capacity(smesh.submeshes.len().max(1));

    for s in &smesh.submeshes {
        let slot_idx = match s.material_idx.and_then(|mi| doc.materials.get(mi).map(|m| (mi, m))) {
            Some((mi, mat)) => *doc_mat_to_slot.entry(mi).or_insert_with(|| {
                let idx = materials.len();
                materials.push(slot_from_scene_material(doc, mat));
                idx
            }),
            None => {
                if materials.is_empty() {
                    materials.push(ViewportMaterialSlot::default());
                }
                0
            }
        };
        vp_submeshes.push(ViewportSubMesh {
            index_start: s.index_start,
            index_count: s.index_count,
            material_slot: slot_idx,
        });
    }

    let submeshes = smesh
        .submeshes
        .iter()
        .map(|s| SubMeshSlice {
            first_index: s.index_start as u32,
            index_count: s.index_count as u32,
            topology: 0,
        })
        .collect();

    let label = doc
        .nodes
        .get(node_idx)
        .map(|n| n.name.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| smesh.name.clone());

    let mut mesh = DecodedMesh {
        name: label.clone(),
        positions: Vec::with_capacity(deformed.positions.len()),
        normals: Vec::new(),
        uvs: smesh.uvs.clone(),
        indices,
        submeshes,
        bounds_min: [0.0; 3],
        bounds_max: [0.0; 3],
    };
    write_part_positions_normals(&mut mesh, deformed.positions, deformed.normals, true);

    let mut part = ViewportMeshPart::from_materials(
        label,
        mesh,
        materials,
        vp_submeshes,
        Some(node_idx),
    );
    part.blend_shapes = blend_shapes;
    part.blend_weights = blend_weights;
    Some(part)
}

fn extract_smr_blend_weights(smr_obj: &UnityObject) -> Option<Vec<f32>> {
    let arr = smr_obj
        .class
        .properties()
        .get("m_BlendShapeWeights")?
        .as_array()?;
    Some(
        arr.iter()
            .map(|v| (v.as_f64().unwrap_or(0.0) as f32 / 100.0).clamp(0.0, 1.0))
            .collect(),
    )
}

fn find_attached_animator_controller(
    ctx: &PreviewBuildCtx<'_>,
    workspace: &UnityWorkspaceData,
    start_tr: AssetRef,
) -> Option<DecodedAnimatorController> {
    let (_, _, tr_obj) = ctx.read_asset(start_tr)?;
    let mut candidate_trs = vec![start_tr];
    if let Some(children) = tr_obj
        .class
        .properties()
        .get("m_Children")
        .and_then(|v| v.as_array())
    {
        for c in children.iter().take(16) {
            if let Some(cr) = ctx.resolve_pptr(start_tr.file_index, Some(c)) {
                candidate_trs.push(cr);
            }
        }
    }

    for tr_ref in candidate_trs {
        let Some((_, _, cur_tr)) = ctx.read_asset(tr_ref) else {
            continue;
        };
        let Some(go_ref) =
            ctx.resolve_pptr(tr_ref.file_index, cur_tr.class.properties().get("m_GameObject"))
        else {
            continue;
        };
        let Some((_, _, go_obj)) = ctx.read_asset(go_ref) else {
            continue;
        };
        if let Some((anim_ref, anim_obj)) =
            ctx.find_component_of_type(go_ref.file_index, &go_obj, 95)
            && let Some(ctrl_ref) = ctx.resolve_pptr(
                anim_ref.file_index,
                anim_obj.class.properties().get("m_Controller"),
            )
            && let Some((91, _, ctrl_obj)) = ctx.read_asset(ctrl_ref)
            && let Ok(ctrl) = decode_animator_controller(&ctrl_obj, ctrl_ref.file_index, workspace)
        {
            return Some(ctrl);
        }
    }
    None
}

struct PreviewBuildCtx<'a> {
    workspace: &'a UnityWorkspaceData,
    tex_cache: HashMap<AssetRef, Option<Arc<DecodedImage>>>,
    visited_transforms: HashSet<AssetRef>,
    parts: Vec<ViewportMeshPart>,
    bone_segments: Vec<([f32; 3], [f32; 3])>,
    max_parts: usize,
}

impl<'a> PreviewBuildCtx<'a> {
    fn resolve_pptr(&self, src_file: usize, val: Option<&UnityValue>) -> Option<AssetRef> {
        let obj = val?.as_object()?;
        let fid = obj.get("m_FileID").and_then(extract_i32_field)?;
        let pid = obj.get("m_PathID").and_then(extract_i64_field)?;
        if pid == 0 {
            return None;
        }
        let target_file = self.workspace.resolve_external_file(src_file, fid)?;
        Some(AssetRef {
            file_index: target_file,
            path_id: pid,
        })
    }

    fn read_asset(&self, r: AssetRef) -> Option<(i32, String, UnityObject)> {
        let (loaded, info) = self.workspace.object_info(r.file_index, r.path_id)?;
        let obj = read_object(loaded, info).ok()?;
        Some((info.type_id, loaded.file.unity_version.clone(), obj))
    }

    fn find_component_of_type(
        &self,
        go_file: usize,
        go_obj: &UnityObject,
        target_class: i32,
    ) -> Option<(AssetRef, UnityObject)> {
        let comps = go_obj
            .class
            .properties()
            .get("m_Component")
            .and_then(|v| v.as_array())?;
        for item in comps {
            let pptr_val = item
                .as_object()
                .and_then(|m| m.get("component"))
                .or(Some(item));
            if let Some(comp_ref) = self.resolve_pptr(go_file, pptr_val)
                && let Some((cid, _, comp_obj)) = self.read_asset(comp_ref)
                && cid == target_class
            {
                return Some((comp_ref, comp_obj));
            }
        }
        None
    }

    fn transform_title(&self, tr_ref: AssetRef) -> Option<String> {
        let (_, _, tr_obj) = self.read_asset(tr_ref)?;
        let go_ref = self.resolve_pptr(
            tr_ref.file_index,
            tr_obj.class.properties().get("m_GameObject"),
        )?;
        let (_, _, go_obj) = self.read_asset(go_ref)?;
        go_obj.name()
    }

    fn walk_transform(
        &mut self,
        tr_ref: AssetRef,
        parent_mat: [f32; 16],
        parent_origin: Option<[f32; 3]>,
        depth: usize,
    ) {
        if depth > 64
            || self.parts.len() >= self.max_parts
            || !self.visited_transforms.insert(tr_ref)
        {
            return;
        }
        let Some((_, _, tr_obj)) = self.read_asset(tr_ref) else {
            return;
        };
        let props = tr_obj.class.properties();
        let t = read_vec3(props.get("m_LocalPosition"), [0.0, 0.0, 0.0]);
        let r = read_quat(props.get("m_LocalRotation"), [0.0, 0.0, 0.0, 1.0]);
        let s = read_vec3(props.get("m_LocalScale"), [1.0, 1.0, 1.0]);
        let world_mat = mul_mat4(parent_mat, trs_mat4(t, r, s));
        let origin = [world_mat[3], world_mat[7], world_mat[11]];

        if let Some(p_orig) = parent_origin {
            let d2 = (origin[0] - p_orig[0]).powi(2)
                + (origin[1] - p_orig[1]).powi(2)
                + (origin[2] - p_orig[2]).powi(2);
            if d2 > 1e-8 {
                self.bone_segments.push((p_orig, origin));
            }
        }

        if let Some(go_ref) = self.resolve_pptr(tr_ref.file_index, props.get("m_GameObject"))
            && let Some((_, _, go_obj)) = self.read_asset(go_ref)
        {
            self.extract_gameobject_meshes(go_ref, &go_obj, world_mat);
        }

        if let Some(children) = props.get("m_Children").and_then(|v| v.as_array()) {
            for child in children {
                if self.parts.len() >= self.max_parts {
                    break;
                }
                if let Some(child_ref) = self.resolve_pptr(tr_ref.file_index, Some(child)) {
                    self.walk_transform(child_ref, world_mat, Some(origin), depth + 1);
                }
            }
        }
    }

    fn extract_gameobject_meshes(
        &mut self,
        go_ref: AssetRef,
        go_obj: &UnityObject,
        world_mat: [f32; 16],
    ) {
        let Some(comps) = go_obj
            .class
            .properties()
            .get("m_Component")
            .and_then(|v| v.as_array())
        else {
            return;
        };

        let mut mesh_filter: Option<(AssetRef, UnityObject)> = None;
        let mut mesh_renderer: Option<(AssetRef, UnityObject)> = None;
        let mut skinned: Option<(AssetRef, UnityObject)> = None;

        for item in comps {
            let pptr_val = item
                .as_object()
                .and_then(|m| m.get("component"))
                .or(Some(item));
            let Some(cr) = self.resolve_pptr(go_ref.file_index, pptr_val) else {
                continue;
            };
            let Some((cid, _, cobj)) = self.read_asset(cr) else {
                continue;
            };
            match cid {
                33 if mesh_filter.is_none() => mesh_filter = Some((cr, cobj)),
                23 if mesh_renderer.is_none() => mesh_renderer = Some((cr, cobj)),
                137 if skinned.is_none() => skinned = Some((cr, cobj)),
                _ => {}
            }
        }

        let go_name = go_obj.name().unwrap_or_else(|| "Node".to_owned());

        if let Some((mf_ref, mf_obj)) = mesh_filter
            && let Some(mesh_ref) =
                self.resolve_pptr(mf_ref.file_index, mf_obj.class.properties().get("m_Mesh"))
            && let Some((43, ver, mesh_obj)) = self.read_asset(mesh_ref)
            && let Ok(full) = decode_mesh_full(&mesh_obj, &ver, &self.workspace.streams)
            && self.parts.len() < self.max_parts
        {
            let mats = if let Some((mr_ref, mr_obj)) = &mesh_renderer {
                self.resolve_materials(mr_ref.file_index, mr_obj)
            } else {
                vec![ViewportMaterialSlot::default()]
            };
            let transformed = transform_decoded_mesh(full.base, world_mat);
            let vp_subs = build_default_submeshes(&transformed, mats.len());
            self.parts.push(ViewportMeshPart::from_materials(
                go_name.clone(),
                transformed,
                mats,
                vp_subs,
                None,
            ));
        }

        if let Some((smr_ref, smr_obj)) = skinned
            && let Some(mesh_ref) =
                self.resolve_pptr(smr_ref.file_index, smr_obj.class.properties().get("m_Mesh"))
            && let Some((43, ver, mesh_obj)) = self.read_asset(mesh_ref)
            && let Ok(full) = decode_mesh_full(&mesh_obj, &ver, &self.workspace.streams)
            && self.parts.len() < self.max_parts
        {
            let mats = self.resolve_materials(smr_ref.file_index, &smr_obj);
            let transformed = transform_decoded_mesh(full.base, world_mat);
            let vp_subs = build_default_submeshes(&transformed, mats.len());
            self.parts.push(ViewportMeshPart::from_materials(
                go_name,
                transformed,
                mats,
                vp_subs,
                None,
            ));
        }
    }

    fn resolve_materials(
        &mut self,
        renderer_file: usize,
        renderer_obj: &UnityObject,
    ) -> Vec<ViewportMaterialSlot> {
        let Some(mats) = renderer_obj
            .class
            .properties()
            .get("m_Materials")
            .and_then(|v| v.as_array())
        else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(mats.len());
        for mat_val in mats {
            let slot = self
                .resolve_pptr(renderer_file, Some(mat_val))
                .and_then(|r| self.decode_material_slot(r))
                .unwrap_or_default();
            out.push(slot);
        }
        out
    }

    fn decode_material_slot(&mut self, mat_ref: AssetRef) -> Option<ViewportMaterialSlot> {
        let (21, _, mat_obj) = self.read_asset(mat_ref)? else {
            return None;
        };
        let mat = decode_material(&mat_obj).ok()?;
        let albedo_image = self.decode_tex_slot(mat_ref.file_index, mat.albedo_slot());
        let normal_image = self.decode_tex_slot(mat_ref.file_index, mat.normal_slot());
        let emissive_image = self.decode_tex_slot(mat_ref.file_index, mat.emission_slot());
        let metallic_gloss_image =
            self.decode_tex_slot(mat_ref.file_index, mat.metallic_gloss_slot());
        let emissive_color = mat.emissive_color().unwrap_or_else(|| {
            if emissive_image.is_some() {
                [1.0, 1.0, 1.0]
            } else {
                [0.0, 0.0, 0.0]
            }
        });
        Some(ViewportMaterialSlot {
            name: mat.name.clone(),
            albedo_image,
            albedo_texture: None,
            normal_image,
            normal_texture: None,
            emissive_image,
            emissive_texture: None,
            metallic_gloss_image,
            metallic_gloss_texture: None,
            base_color: mat.base_color(),
            emissive_color,
            metallic: mat.metallic_factor(),
            roughness: mat.roughness_factor(),
            alpha_mode: mat.alpha_mode(),
            double_sided: mat.double_sided(),
        })
    }

    fn decode_tex_slot(
        &mut self,
        mat_file: usize,
        slot: Option<&MaterialTextureSlot>,
    ) -> Option<Arc<DecodedImage>> {
        let slot = slot?;
        if slot.texture_path_id == 0 {
            return None;
        }
        let tex_file = self
            .workspace
            .resolve_external_file(mat_file, slot.texture_file_id)?;
        let tex_ref = AssetRef {
            file_index: tex_file,
            path_id: slot.texture_path_id,
        };
        if let Some(cached) = self.tex_cache.get(&tex_ref) {
            return cached.clone();
        }
        let decoded = self
            .read_asset(tex_ref)
            .filter(|(cid, _, _)| *cid == 28)
            .and_then(|(_, ver, tex_obj)| {
                decode_texture2d(
                    &tex_obj,
                    &ver,
                    &self.workspace.streams,
                    TextureDecodeOptions::default(),
                )
                .ok()
                .map(Arc::new)
            });
        self.tex_cache.insert(tex_ref, decoded.clone());
        decoded
    }
}

fn slot_from_scene_material(doc: &SceneDocument, mat: &SceneMaterial) -> ViewportMaterialSlot {
    let tex = |idx: Option<usize>| idx.and_then(|ti| doc.textures.get(ti)).map(|t| Arc::clone(&t.image));
    let emissive_image = tex(mat.emissive_tex);
    let emissive_color = if mat.emissive == [0.0, 0.0, 0.0] && emissive_image.is_some() {
        [1.0, 1.0, 1.0]
    } else {
        mat.emissive
    };
    ViewportMaterialSlot {
        name: mat.name.clone(),
        albedo_image: tex(mat.albedo_tex),
        albedo_texture: None,
        normal_image: tex(mat.normal_tex),
        normal_texture: None,
        emissive_image,
        emissive_texture: None,
        metallic_gloss_image: tex(mat.metallic_gloss_tex),
        metallic_gloss_texture: None,
        base_color: mat.base_color,
        emissive_color,
        metallic: mat.metallic,
        roughness: mat.roughness,
        alpha_mode: mat.alpha_mode,
        double_sided: mat.double_sided,
    }
}

fn build_default_submeshes(mesh: &DecodedMesh, material_count: usize) -> Vec<ViewportSubMesh> {
    let slot_cap = material_count.max(1);
    if mesh.submeshes.is_empty() {
        return vec![ViewportSubMesh {
            index_start: 0,
            index_count: (mesh.indices.len() / 3) * 3,
            material_slot: 0,
        }];
    }
    mesh.submeshes
        .iter()
        .enumerate()
        .map(|(sub_i, s)| {
            let start = (s.first_index as usize).min(mesh.indices.len());
            let avail = mesh.indices.len().saturating_sub(start);
            let count = ((s.index_count as usize).min(avail) / 3) * 3;
            ViewportSubMesh {
                index_start: start,
                index_count: count,
                material_slot: sub_i.min(slot_cap - 1),
            }
        })
        .collect()
}

const IDENTITY_MAT4: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

fn read_vec3(val: Option<&UnityValue>, default: [f32; 3]) -> [f32; 3] {
    let Some(obj) = val.and_then(|v| v.as_object()) else {
        return default;
    };
    let x = obj.get("x").and_then(UnityValue::as_f64).unwrap_or(f64::from(default[0])) as f32;
    let y = obj.get("y").and_then(UnityValue::as_f64).unwrap_or(f64::from(default[1])) as f32;
    let z = obj.get("z").and_then(UnityValue::as_f64).unwrap_or(f64::from(default[2])) as f32;
    [x, y, z]
}

fn read_quat(val: Option<&UnityValue>, default: [f32; 4]) -> [f32; 4] {
    let Some(obj) = val.and_then(|v| v.as_object()) else {
        return default;
    };
    let x = obj.get("x").and_then(UnityValue::as_f64).unwrap_or(f64::from(default[0])) as f32;
    let y = obj.get("y").and_then(UnityValue::as_f64).unwrap_or(f64::from(default[1])) as f32;
    let z = obj.get("z").and_then(UnityValue::as_f64).unwrap_or(f64::from(default[2])) as f32;
    let w = obj.get("w").and_then(UnityValue::as_f64).unwrap_or(f64::from(default[3])) as f32;
    let len = (x * x + y * y + z * z + w * w).sqrt();
    if len > 1e-6 {
        [x / len, y / len, z / len, w / len]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

fn trs_mat4(t: [f32; 3], q: [f32; 4], s: [f32; 3]) -> [f32; 16] {
    let [x, y, z, w] = q;
    let x2 = x + x;
    let y2 = y + y;
    let z2 = z + z;
    let xx = x * x2;
    let xy = x * y2;
    let xz = x * z2;
    let yy = y * y2;
    let yz = y * z2;
    let zz = z * z2;
    let wx = w * x2;
    let wy = w * y2;
    let wz = w * z2;

    [
        (1.0 - (yy + zz)) * s[0],
        (xy - wz) * s[1],
        (xz + wy) * s[2],
        t[0],
        (xy + wz) * s[0],
        (1.0 - (xx + zz)) * s[1],
        (yz - wx) * s[2],
        t[1],
        (xz - wy) * s[0],
        (yz + wx) * s[1],
        (1.0 - (xx + yy)) * s[2],
        t[2],
        0.0,
        0.0,
        0.0,
        1.0,
    ]
}

fn mul_mat4(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for r in 0..4 {
        for c in 0..4 {
            out[r * 4 + c] = a[r * 4] * b[c]
                + a[r * 4 + 1] * b[4 + c]
                + a[r * 4 + 2] * b[8 + c]
                + a[r * 4 + 3] * b[12 + c];
        }
    }
    out
}

fn transform_decoded_mesh(mut mesh: DecodedMesh, m: [f32; 16]) -> DecodedMesh {
    let mut min_v = [f32::INFINITY; 3];
    let mut max_v = [f32::NEG_INFINITY; 3];
    for p in &mut mesh.positions {
        let [x, y, z] = *p;
        let nx = m[0] * x + m[1] * y + m[2] * z + m[3];
        let ny = m[4] * x + m[5] * y + m[6] * z + m[7];
        let nz = m[8] * x + m[9] * y + m[10] * z + m[11];
        *p = [nx, ny, nz];
        for i in 0..3 {
            min_v[i] = min_v[i].min((*p)[i]);
            max_v[i] = max_v[i].max((*p)[i]);
        }
    }
    for n in &mut mesh.normals {
        let [x, y, z] = *n;
        let rx = m[0] * x + m[1] * y + m[2] * z;
        let ry = m[4] * x + m[5] * y + m[6] * z;
        let rz = m[8] * x + m[9] * y + m[10] * z;
        let l = (rx * rx + ry * ry + rz * rz).sqrt();
        if l > 1e-6 {
            *n = [rx / l, ry / l, rz / l];
        }
    }
    if min_v[0].is_finite() {
        mesh.bounds_min = min_v;
        mesh.bounds_max = max_v;
    }
    mesh
}

fn compute_combined_bounds(
    parts: &[ViewportMeshPart],
    bones: &[([f32; 3], [f32; 3])],
) -> ([f32; 3], [f32; 3]) {
    let mut min_v = [f32::INFINITY; 3];
    let mut max_v = [f32::NEG_INFINITY; 3];
    for part in parts {
        for i in 0..3 {
            min_v[i] = min_v[i].min(part.mesh.bounds_min[i]);
            max_v[i] = max_v[i].max(part.mesh.bounds_max[i]);
        }
    }
    for &(a, b) in bones {
        for p in [a, b] {
            for i in 0..3 {
                min_v[i] = min_v[i].min(p[i]);
                max_v[i] = max_v[i].max(p[i]);
            }
        }
    }
    if min_v[0].is_finite() {
        (min_v, max_v)
    } else {
        ([-1.0; 3], [1.0; 3])
    }
}

/// Builds an interactive 3D viewport preview from a decoded `TerrainData` (`class_id == 156`).
pub fn build_terrain_preview(
    terrain: &DecodedTerrainData,
    root_ref: AssetRef,
) -> HierarchyPreviewData {
    let (mesh, albedo) = terrain.build_preview_mesh_and_albedo(129);
    let mut slot = ViewportMaterialSlot::from_albedo(Some(Arc::new(albedo)), [1.0, 1.0, 1.0, 1.0]);
    slot.name = format!("{} (Heightmap)", terrain.name);
    slot.roughness = 0.88;
    slot.double_sided = true;

    let part = ViewportMeshPart::from_materials(
        terrain.name.clone(),
        mesh,
        vec![slot],
        Vec::new(),
        None,
    );
    let parts = if part.mesh.positions.is_empty() {
        Vec::new()
    } else {
        vec![part]
    };
    let (bounds_min, bounds_max) = compute_combined_bounds(&parts, &[]);
    HierarchyPreviewData::static_from_parts(
        terrain.name.clone(),
        root_ref,
        parts,
        Vec::new(),
        bounds_min,
        bounds_max,
    )
}

/// Builds an interactive 3D emitter shape + sampled particle cloud preview for a `ParticleSystem`
/// (`class_id == 198`), pulling textures/materials from a sibling `ParticleSystemRenderer` (`199`)
/// on the same `GameObject` when present.
pub fn build_particle_preview(
    workspace: &UnityWorkspaceData,
    particle: &DecodedParticleSystem,
    root_ref: AssetRef,
    obj: &UnityObject,
) -> HierarchyPreviewData {
    let (wire_mesh, cloud_mesh) = particle.build_preview_meshes();

    let mut ctx = PreviewBuildCtx {
        workspace,
        tex_cache: HashMap::new(),
        visited_transforms: HashSet::new(),
        parts: Vec::new(),
        bone_segments: Vec::new(),
        max_parts: 8,
    };
    let renderer_slot = ctx
        .resolve_pptr(root_ref.file_index, obj.class.properties().get("m_GameObject"))
        .and_then(|go_ref| {
            let (1, _, go_obj) = ctx.read_asset(go_ref)? else {
                return None;
            };
            let (rend_ref, rend_obj) = ctx.find_component_of_type(go_ref.file_index, &go_obj, 199)?;
            let mut mats = ctx.resolve_materials(rend_ref.file_index, &rend_obj);
            mats.drain(..).next()
        });

    let mut parts = Vec::new();
    if !cloud_mesh.positions.is_empty() {
        let mut cloud_slot = match renderer_slot {
            Some(mut slot) => {
                for c in 0..4 {
                    slot.base_color[c] *= particle.start_color_min[c];
                }
                slot
            }
            None => ViewportMaterialSlot::from_albedo(None, particle.start_color_min),
        };
        cloud_slot.double_sided = true;
        parts.push(ViewportMeshPart::from_materials(
            format!("{} (Particles)", particle.name),
            cloud_mesh,
            vec![cloud_slot],
            Vec::new(),
            None,
        ));
    }
    if !wire_mesh.positions.is_empty() {
        let mut wire_slot = ViewportMaterialSlot::from_albedo(None, [0.40, 0.82, 1.0, 0.85]);
        wire_slot.alpha_mode = MaterialAlphaMode::Blend;
        wire_slot.double_sided = true;
        parts.push(ViewportMeshPart::from_materials(
            format!("{} (Emitter: {})", particle.name, particle.shape.shape_label()),
            wire_mesh,
            vec![wire_slot],
            Vec::new(),
            None,
        ));
    }

    let (bounds_min, bounds_max) = compute_combined_bounds(&parts, &[]);
    HierarchyPreviewData::static_from_parts(
        particle.name.clone(),
        root_ref,
        parts,
        Vec::new(),
        bounds_min,
        bounds_max,
    )
}
