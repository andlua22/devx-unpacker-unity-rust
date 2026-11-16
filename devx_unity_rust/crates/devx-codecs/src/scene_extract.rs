//! Clean-room unified 3D right-handed scene extraction, CPU skeletal skinning,
//! and animation evaluation shared by FBX, COLLADA, OBJ+MTL exporters and the
//! interactive 3D workbench viewport.

use crate::animation::{
    DecodedAnimationClip, TrackProperty, decode_animation_clip, unity_path_crc32,
};
use crate::gltf_scene::{
    GltfSceneOptions, extract_local_trs_rh, unity_bindpose_row_major_to_gltf_col_major,
};
use crate::material::{DecodedMaterial, MaterialAlphaMode, decode_material};
use crate::mesh::{DecodedBlendShape, apply_blend_shapes, decode_mesh_full};
use crate::texture::{TextureDecodeOptions, decode_texture2d, encode_png};
use crate::{CodecError, CodecResult};
use devx_plugin_api::{AssetRef, DecodedImage};
use devx_unity_io::{
    UnityObject, UnityValue, UnityWorkspaceData, extract_i32_field, extract_i64_field, read_object,
};
use image::imageops::FilterType;
use image::{DynamicImage, RgbaImage};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

type UnityRef = (usize, i64);

pub const IDENTITY_MAT4: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0,
    0.0, 1.0, 0.0, 0.0,
    0.0, 0.0, 1.0, 0.0,
    0.0, 0.0, 0.0, 1.0,
];

/// Decoded RGBA texture plus cached PNG bytes for export consumers.
#[derive(Clone)]
pub struct SceneTexture {
    pub name: String,
    pub file_stem: String,
    pub image: Arc<DecodedImage>,
    pub png_bytes: Arc<Vec<u8>>,
}

/// PBR material definition extracted from a Unity `Material`.
#[derive(Clone, Debug)]
pub struct SceneMaterial {
    pub name: String,
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    pub double_sided: bool,
    pub alpha_mode: MaterialAlphaMode,
    pub albedo_tex: Option<usize>,
    pub normal_tex: Option<usize>,
    pub emissive_tex: Option<usize>,
    pub metallic_gloss_tex: Option<usize>,
}

/// Indexed triangle slice within a [`SceneMesh`].
#[derive(Clone, Debug)]
pub struct SceneSubMesh {
    pub index_start: usize,
    pub index_count: usize,
    pub material_idx: Option<usize>,
}

/// Mesh geometry in right-handed Y-up space (`[-x, y, z]`) with Unity bottom-up UVs,
/// optional per-vertex 4-bone skinning weights, and optional morph targets.
#[derive(Clone, Debug)]
pub struct SceneMesh {
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub submeshes: Vec<SceneSubMesh>,
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    pub bind_poses_row_major: Vec<[f32; 16]>,
    pub blend_shapes: Vec<DecodedBlendShape>,
}

/// Skinned mesh bone palette and right-handed column-major inverse bind matrices.
#[derive(Clone, Debug)]
pub struct SceneSkin {
    pub name: String,
    pub mesh_node_idx: usize,
    pub root_bone_idx: Option<usize>,
    pub joint_nodes: Vec<usize>,
    pub inverse_bind_matrices_col_major: Vec<[f32; 16]>,
}

/// Hierarchical scene / skeleton node in right-handed Y-up local TRS coordinates.
#[derive(Clone, Debug)]
pub struct SceneNode {
    pub name: String,
    pub unity_ref: Option<AssetRef>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    pub mesh_idx: Option<usize>,
    pub skin_idx: Option<usize>,
    pub is_bone: bool,
}

/// Single keyframed TRS track bound directly to a node index in [`SceneDocument`].
#[derive(Clone, Debug)]
pub struct SceneAnimTrack {
    pub node_idx: usize,
    pub property: TrackProperty,
    pub times: Vec<f32>,
    pub values: Vec<f32>,
}

/// Extracted animation clip bound to a [`SceneDocument`].
#[derive(Clone, Debug)]
pub struct SceneAnimClip {
    pub name: String,
    pub duration_secs: f32,
    pub sample_rate: f32,
    pub tracks: Vec<SceneAnimTrack>,
}

/// Complete extracted 3D scene/prefab IR in right-handed Y-up meters.
#[derive(Clone)]
pub struct SceneDocument {
    pub name: String,
    pub roots: Vec<usize>,
    pub nodes: Vec<SceneNode>,
    pub meshes: Vec<SceneMesh>,
    pub materials: Vec<SceneMaterial>,
    pub textures: Vec<SceneTexture>,
    pub skins: Vec<SceneSkin>,
    pub animations: Vec<SceneAnimClip>,
    pub avatar_tos: HashMap<u32, String>,
}

/// Result of evaluating a scene hierarchy and bone skeleton at a given animation time.
#[derive(Clone, Debug)]
pub struct EvaluatedScenePose {
    pub local_t: Vec<[f32; 3]>,
    pub local_r: Vec<[f32; 4]>,
    pub local_s: Vec<[f32; 3]>,
    pub world_matrices: Vec<[f32; 16]>,
    pub bone_segments: Vec<([f32; 3], [f32; 3])>,
}

impl SceneDocument {
    /// Resolves a decoded [`DecodedAnimationClip`] against this scene's node tree and `Avatar.m_TOS`
    /// and appends it to `self.animations` if at least one track binds cleanly.
    pub fn attach_decoded_clip(&mut self, clip: &DecodedAnimationClip) -> Option<usize> {
        let (hash_to_node, path_to_node) = self.build_node_path_maps();
        let mut tracks = Vec::new();
        let mut seen = HashSet::new();

        for track in &clip.tracks {
            if track.times.is_empty() {
                continue;
            }
            let target_node = hash_to_node.get(&track.path_hash).copied().or_else(|| {
                track
                    .path_name
                    .as_ref()
                    .and_then(|p| path_to_node.get(p).copied())
            });
            let Some(node_idx) = target_node else {
                continue;
            };
            let tag = match track.property {
                TrackProperty::Translation => 0u8,
                TrackProperty::Rotation => 1u8,
                TrackProperty::Scale => 2u8,
            };
            if !seen.insert((node_idx, tag)) {
                continue;
            }

            let stride = if track.property == TrackProperty::Rotation { 4 } else { 3 };
            let key_count = track.times.len().min(track.values.len() / stride);
            if key_count == 0 {
                continue;
            }

            let mut clean_times: Vec<f32> = Vec::with_capacity(key_count);
            let mut clean_vals = Vec::with_capacity(key_count * stride);
            let mut prev_quat: Option<[f32; 4]> = None;

            for k in 0..key_count {
                let t = track.times[k].max(0.0);
                if let Some(&last_t) = clean_times.last()
                    && (t - last_t).abs() < 1e-6_f32
                {
                    continue;
                }
                clean_times.push(t);
                let src = &track.values[k * stride..(k + 1) * stride];
                match track.property {
                    TrackProperty::Translation => {
                        clean_vals.extend_from_slice(&[-src[0], src[1], src[2]]);
                    }
                    TrackProperty::Scale => {
                        clean_vals.extend_from_slice(&[src[0], src[1], src[2]]);
                    }
                    TrackProperty::Rotation => {
                        let mut q = normalize_quat([src[0], -src[1], -src[2], src[3]]);
                        if let Some(pq) = prev_quat
                            && pq[0] * q[0] + pq[1] * q[1] + pq[2] * q[2] + pq[3] * q[3] < 0.0
                        {
                            q = [-q[0], -q[1], -q[2], -q[3]];
                        }
                        prev_quat = Some(q);
                        clean_vals.extend_from_slice(&q);
                    }
                }
            }

            if !clean_times.is_empty() {
                tracks.push(SceneAnimTrack {
                    node_idx,
                    property: track.property,
                    times: clean_times,
                    values: clean_vals,
                });
            }
        }

        if tracks.is_empty() {
            return None;
        }
        let duration = tracks
            .iter()
            .filter_map(|t| t.times.last().copied())
            .fold(clip.duration_secs.max(0.0), f32::max);
        let idx = self.animations.len();
        self.animations.push(SceneAnimClip {
            name: clip.name.clone(),
            duration_secs: duration,
            sample_rate: clip.sample_rate.max(1.0),
            tracks,
        });
        Some(idx)
    }

    fn build_node_path_maps(&self) -> (HashMap<u32, usize>, HashMap<String, usize>) {
        let mut hash_to_node = HashMap::new();
        let mut path_to_node = HashMap::new();
        let mut name_only = HashMap::new();

        if let Some(&first_root) = self.roots.first() {
            hash_to_node.insert(0, first_root);
            path_to_node.insert(String::new(), first_root);
        }

        let mut stack: Vec<(usize, Vec<String>)> = self
            .roots
            .iter()
            .map(|&idx| (idx, vec![self.nodes[idx].name.clone()]))
            .collect();
        let mut visited = HashSet::new();

        while let Some((node_idx, segs)) = stack.pop() {
            if !visited.insert(node_idx) {
                continue;
            }
            if let Some(short) = segs.last()
                && !short.is_empty()
            {
                name_only.entry(short.clone()).or_insert(node_idx);
            }
            for start in 0..segs.len() {
                let rel = segs[start..].join("/");
                if !rel.is_empty() {
                    hash_to_node
                        .entry(unity_path_crc32(&rel))
                        .or_insert(node_idx);
                    path_to_node.entry(rel).or_insert(node_idx);
                }
            }
            for &child in &self.nodes[node_idx].children {
                let mut child_segs = segs.clone();
                child_segs.push(self.nodes[child].name.clone());
                stack.push((child, child_segs));
            }
        }

        for (&tos_hash, tos_path) in &self.avatar_tos {
            if let Some(&n) = path_to_node.get(tos_path) {
                hash_to_node.entry(tos_hash).or_insert(n);
            } else if let Some((_, tail)) = tos_path.rsplit_once('/')
                && let Some(&n) = name_only.get(tail)
            {
                hash_to_node.entry(tos_hash).or_insert(n);
            } else if let Some(&n) = name_only.get(tos_path) {
                hash_to_node.entry(tos_hash).or_insert(n);
            }
        }

        (hash_to_node, path_to_node)
    }
}

/// Extracts a complete right-handed 3D [`SceneDocument`] (meshes, materials, textures,
/// skeletal skins, and attached animation clips) from Unity hierarchy roots.
pub fn extract_unity_scene(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    extra_clips: &[AssetRef],
    opts: &GltfSceneOptions,
) -> CodecResult<SceneDocument> {
    let mut ctx = ExtractCtx::new(workspace, opts);
    let initial_trs = ctx.resolve_initial_transforms(roots);

    for tr_ref in initial_trs {
        if let Some(root_idx) = ctx.build_transform_subtree(tr_ref, None)? {
            ctx.roots.push(root_idx);
        }
    }

    // Fallback for standalone `Mesh` roots when no `Transform` exists.
    if ctx.roots.is_empty() {
        for r in resolve_standalone_mesh_roots(workspace, roots) {
            let uref = (r.file_index, r.path_id);
            if let Some(obj) = ctx.try_read_object(uref)
                && obj.class_id() == 43
                && let Ok(mesh_idx) = ctx.get_or_build_mesh(uref, &[], false)
            {
                let name = obj.name().unwrap_or_else(|| format!("Mesh_{}", r.path_id));
                let node_idx = ctx.push_node(SceneNode {
                    name,
                    unity_ref: Some(r),
                    parent: None,
                    children: Vec::new(),
                    translation: [0.0, 0.0, 0.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                    scale: [1.0, 1.0, 1.0],
                    mesh_idx: Some(mesh_idx),
                    skin_idx: None,
                    is_bone: false,
                });
                ctx.roots.push(node_idx);
            }
        }
    }

    if ctx.roots.is_empty() {
        return Err(CodecError::MalformedData(
            "no exportable GameObject, Transform, or Mesh found for scene extraction".to_owned(),
        ));
    }

    ctx.finalize_pending_skins();

    let scene_name = ctx
        .roots
        .first()
        .and_then(|&i| ctx.nodes.get(i))
        .map(|n| n.name.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "UnityScene".to_owned());

    let decoded_clips: Vec<_> = if opts.include_animations {
        ctx.collect_clip_refs(roots, extra_clips)
            .into_iter()
            .filter_map(|c_ref| {
                let obj = ctx.try_read_object(c_ref)?;
                (obj.class_id() == 74)
                    .then(|| decode_animation_clip(&obj).ok())
                    .flatten()
            })
            .collect()
    } else {
        Vec::new()
    };

    let mut doc = SceneDocument {
        name: scene_name,
        roots: ctx.roots,
        nodes: ctx.nodes,
        meshes: ctx.meshes,
        materials: ctx.materials,
        textures: ctx.textures,
        skins: ctx.skins,
        animations: Vec::new(),
        avatar_tos: ctx.avatar_tos,
    };

    for decoded_clip in decoded_clips {
        doc.attach_decoded_clip(&decoded_clip);
    }

    Ok(doc)
}

/// Evaluates hierarchical world transforms and skeleton bone segments for a scene pose.
pub fn evaluate_scene_pose(
    scene: &SceneDocument,
    clip_idx: Option<usize>,
    time_secs: f32,
) -> EvaluatedScenePose {
    let n = scene.nodes.len();
    let mut local_t: Vec<[f32; 3]> = scene.nodes.iter().map(|node| node.translation).collect();
    let mut local_r: Vec<[f32; 4]> = scene.nodes.iter().map(|node| node.rotation).collect();
    let mut local_s: Vec<[f32; 3]> = scene.nodes.iter().map(|node| node.scale).collect();

    if let Some(ci) = clip_idx
        && let Some(clip) = scene.animations.get(ci)
    {
        for track in &clip.tracks {
            if track.node_idx >= n || track.times.is_empty() {
                continue;
            }
            match track.property {
                TrackProperty::Translation => {
                    local_t[track.node_idx] = sample_vec3(&track.times, &track.values, time_secs);
                }
                TrackProperty::Rotation => {
                    local_r[track.node_idx] = sample_quat(&track.times, &track.values, time_secs);
                }
                TrackProperty::Scale => {
                    local_s[track.node_idx] = sample_vec3(&track.times, &track.values, time_secs);
                }
            }
        }
    }

    finalize_scene_pose(scene, local_t, local_r, local_s)
}

/// Cross-fades two evaluated poses in local TRS space (`weight_to` clamped to `0.0..=1.0`)
/// and recomputes hierarchy world transforms + bone overlay segments.
pub fn blend_scene_poses(
    scene: &SceneDocument,
    from: &EvaluatedScenePose,
    to: &EvaluatedScenePose,
    weight_to: f32,
) -> EvaluatedScenePose {
    let a = weight_to.clamp(0.0, 1.0);
    blend_scene_poses_weighted(scene, &[(from, 1.0 - a), (to, a)])
}

/// Blends an arbitrary list of `(pose, weight)` contributions (used for 1D/2D/Direct BlendTrees
/// and Animator state cross-fades) in local TRS space with hemisphere-aligned NLERP.
pub fn blend_scene_poses_weighted(
    scene: &SceneDocument,
    weighted: &[(&EvaluatedScenePose, f32)],
) -> EvaluatedScenePose {
    let total_w: f32 = weighted
        .iter()
        .filter(|(_, w)| *w > 1e-6)
        .map(|(_, w)| *w)
        .sum();
    if total_w <= 1e-6 {
        return evaluate_scene_pose(scene, None, 0.0);
    }
    let inv_w = 1.0 / total_w;
    let n = scene.nodes.len();
    let mut local_t = Vec::with_capacity(n);
    let mut local_r = Vec::with_capacity(n);
    let mut local_s = Vec::with_capacity(n);

    for i in 0..n {
        let def_t = scene.nodes[i].translation;
        let def_r = scene.nodes[i].rotation;
        let def_s = scene.nodes[i].scale;
        let mut t_sum = [0.0f32; 3];
        let mut r_sum = [0.0f32; 4];
        let mut s_sum = [0.0f32; 3];
        let mut ref_q: Option<[f32; 4]> = None;

        for &(pose, raw_w) in weighted {
            if raw_w <= 1e-6 {
                continue;
            }
            let w = raw_w * inv_w;
            let t = pose.local_t.get(i).copied().unwrap_or(def_t);
            let mut r = pose.local_r.get(i).copied().unwrap_or(def_r);
            let s = pose.local_s.get(i).copied().unwrap_or(def_s);
            if let Some(rq) = ref_q {
                if rq[0] * r[0] + rq[1] * r[1] + rq[2] * r[2] + rq[3] * r[3] < 0.0 {
                    r = [-r[0], -r[1], -r[2], -r[3]];
                }
            } else {
                ref_q = Some(r);
            }
            for k in 0..3 {
                t_sum[k] += t[k] * w;
                s_sum[k] += s[k] * w;
            }
            for k in 0..4 {
                r_sum[k] += r[k] * w;
            }
        }
        local_t.push(t_sum);
        local_r.push(normalize_quat(r_sum));
        local_s.push(s_sum);
    }

    finalize_scene_pose(scene, local_t, local_r, local_s)
}

fn finalize_scene_pose(
    scene: &SceneDocument,
    local_t: Vec<[f32; 3]>,
    local_r: Vec<[f32; 4]>,
    local_s: Vec<[f32; 3]>,
) -> EvaluatedScenePose {
    let n = scene.nodes.len();
    let mut world_matrices = vec![IDENTITY_MAT4; n];
    let mut visited = vec![false; n];
    for &root_idx in &scene.roots {
        eval_world_recursive(
            scene,
            root_idx,
            &IDENTITY_MAT4,
            &local_t,
            &local_r,
            &local_s,
            &mut world_matrices,
            &mut visited,
        );
    }
    // Catch any unvisited orphan nodes gracefully.
    for i in 0..n {
        if !visited[i] && scene.nodes[i].parent.is_none() {
            eval_world_recursive(
                scene,
                i,
                &IDENTITY_MAT4,
                &local_t,
                &local_r,
                &local_s,
                &mut world_matrices,
                &mut visited,
            );
        }
    }

    let mut bone_segments = Vec::new();
    for (idx, node) in scene.nodes.iter().enumerate() {
        if !node.is_bone {
            continue;
        }
        let Some(p_idx) = node.parent else {
            continue;
        };
        if !scene.nodes.get(p_idx).is_some_and(|p| p.is_bone) {
            continue;
        }
        let pm = world_matrices[p_idx];
        let cm = world_matrices[idx];
        bone_segments.push(([pm[12], pm[13], pm[14]], [cm[12], cm[13], cm[14]]));
    }

    EvaluatedScenePose {
        local_t,
        local_r,
        local_s,
        world_matrices,
        bone_segments,
    }
}

/// World-space deformed vertex positions and normals produced by [`deform_node_mesh_world`].
#[derive(Debug, Clone)]
pub struct DeformedMeshWorld {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
}

/// Computes world-space deformed vertex positions for `node_idx` under `pose`,
/// performing CPU Linear Blend Skinning when a `SceneSkin` is bound or rigid transform otherwise.
pub fn deform_node_mesh_world(
    scene: &SceneDocument,
    node_idx: usize,
    pose: &EvaluatedScenePose,
) -> Option<DeformedMeshWorld> {
    deform_node_mesh_world_with_weights(scene, node_idx, pose, None)
}

/// Like [`deform_node_mesh_world`], additionally applying morph target `blend_weights` (`0.0..=1.0`)
/// in local space prior to Linear Blend Skinning and world transformation.
pub fn deform_node_mesh_world_with_weights(
    scene: &SceneDocument,
    node_idx: usize,
    pose: &EvaluatedScenePose,
    blend_weights: Option<&[f32]>,
) -> Option<DeformedMeshWorld> {
    let node = scene.nodes.get(node_idx)?;
    let mesh = scene.meshes.get(node.mesh_idx?)?;
    let node_world = pose
        .world_matrices
        .get(node_idx)
        .copied()
        .unwrap_or(IDENTITY_MAT4);

    let has_active_morphs = !mesh.blend_shapes.is_empty()
        && blend_weights.is_some_and(|ws| ws.iter().any(|w| w.abs() > 1e-5));
    let morphed = has_active_morphs.then(|| {
        apply_blend_shapes(
            &mesh.positions,
            &mesh.normals,
            &mesh.blend_shapes,
            blend_weights.unwrap_or(&[]),
        )
    });
    let base_positions: &[[f32; 3]] = morphed
        .as_ref()
        .map_or(mesh.positions.as_slice(), |(p, _)| p.as_slice());
    let base_normals: &[[f32; 3]] = morphed
        .as_ref()
        .map_or(mesh.normals.as_slice(), |(_, n)| n.as_slice());

    if let Some(skin_idx) = node.skin_idx
        && let Some(skin) = scene.skins.get(skin_idx)
        && mesh.joints.len() == base_positions.len()
        && mesh.weights.len() == base_positions.len()
        && !skin.joint_nodes.is_empty()
    {
        let skin_mats: Vec<[f32; 16]> = skin
            .joint_nodes
            .iter()
            .enumerate()
            .map(|(j_slot, &joint_node)| {
                let w = pose
                    .world_matrices
                    .get(joint_node)
                    .copied()
                    .unwrap_or(node_world);
                let ibm = skin
                    .inverse_bind_matrices_col_major
                    .get(j_slot)
                    .copied()
                    .unwrap_or(IDENTITY_MAT4);
                mat4_mul(&w, &ibm)
            })
            .collect();

        let mut out_pos = Vec::with_capacity(base_positions.len());
        let mut out_norm = Vec::with_capacity(base_normals.len());
        let has_normals = base_normals.len() == base_positions.len();

        for v in 0..base_positions.len() {
            let p = base_positions[v];
            let j = mesh.joints[v];
            let w = mesh.weights[v];
            let mut m = [0.0f32; 16];
            let mut total_w = 0.0f32;
            for k in 0..4 {
                let wk = w[k];
                if wk <= 1e-6 {
                    continue;
                }
                if let Some(sm) = skin_mats.get(j[k] as usize) {
                    for (dst, src) in m.iter_mut().zip(sm.iter()) {
                        *dst += *src * wk;
                    }
                    total_w += wk;
                }
            }
            if total_w <= 1e-6 {
                out_pos.push(mat4_transform_point(&node_world, p));
                if has_normals {
                    out_norm.push(mat4_transform_normal(&node_world, base_normals[v]));
                }
            } else {
                if (total_w - 1.0).abs() > 1e-3 {
                    let inv = 1.0 / total_w;
                    for c in &mut m {
                        *c *= inv;
                    }
                }
                out_pos.push(mat4_transform_point(&m, p));
                if has_normals {
                    out_norm.push(mat4_transform_normal(&m, base_normals[v]));
                }
            }
        }
        return Some(DeformedMeshWorld {
            positions: out_pos,
            normals: out_norm,
        });
    }

    let out_pos = base_positions
        .iter()
        .map(|&p| mat4_transform_point(&node_world, p))
        .collect();
    let out_norm = base_normals
        .iter()
        .map(|&n| mat4_transform_normal(&node_world, n))
        .collect();
    Some(DeformedMeshWorld {
        positions: out_pos,
        normals: out_norm,
    })
}

#[allow(clippy::too_many_arguments)]
fn eval_world_recursive(
    scene: &SceneDocument,
    node_idx: usize,
    parent_world: &[f32; 16],
    local_t: &[[f32; 3]],
    local_r: &[[f32; 4]],
    local_s: &[[f32; 3]],
    out_world: &mut [[f32; 16]],
    visited: &mut [bool],
) {
    if node_idx >= scene.nodes.len() || visited[node_idx] {
        return;
    }
    visited[node_idx] = true;
    let local = mat4_from_trs(local_t[node_idx], local_r[node_idx], local_s[node_idx]);
    let world = mat4_mul(parent_world, &local);
    out_world[node_idx] = world;
    for &child in &scene.nodes[node_idx].children {
        eval_world_recursive(
            scene, child, &world, local_t, local_r, local_s, out_world, visited,
        );
    }
}

fn sample_vec3(times: &[f32], values: &[f32], t: f32) -> [f32; 3] {
    if times.len() == 1 || t <= times[0] {
        return [values[0], values[1], values[2]];
    }
    let last = times.len() - 1;
    if t >= times[last] {
        return [values[last * 3], values[last * 3 + 1], values[last * 3 + 2]];
    }
    let hi = times.partition_point(|&kt| kt < t).clamp(1, last);
    let lo = hi - 1;
    let dt = (times[hi] - times[lo]).max(1e-6);
    let a = ((t - times[lo]) / dt).clamp(0.0, 1.0);
    [
        values[lo * 3] + (values[hi * 3] - values[lo * 3]) * a,
        values[lo * 3 + 1] + (values[hi * 3 + 1] - values[lo * 3 + 1]) * a,
        values[lo * 3 + 2] + (values[hi * 3 + 2] - values[lo * 3 + 2]) * a,
    ]
}

fn sample_quat(times: &[f32], values: &[f32], t: f32) -> [f32; 4] {
    if times.len() == 1 || t <= times[0] {
        return normalize_quat([values[0], values[1], values[2], values[3]]);
    }
    let last = times.len() - 1;
    if t >= times[last] {
        let base = last * 4;
        return normalize_quat([
            values[base],
            values[base + 1],
            values[base + 2],
            values[base + 3],
        ]);
    }
    let hi = times.partition_point(|&kt| kt < t).clamp(1, last);
    let lo = hi - 1;
    let dt = (times[hi] - times[lo]).max(1e-6);
    let a = ((t - times[lo]) / dt).clamp(0.0, 1.0);
    let q0 = [
        values[lo * 4],
        values[lo * 4 + 1],
        values[lo * 4 + 2],
        values[lo * 4 + 3],
    ];
    let q1 = [
        values[hi * 4],
        values[hi * 4 + 1],
        values[hi * 4 + 2],
        values[hi * 4 + 3],
    ];
    nlerp_quat(q0, q1, a)
}

fn nlerp_quat(q0: [f32; 4], mut q1: [f32; 4], a: f32) -> [f32; 4] {
    let dot = q0[0] * q1[0] + q0[1] * q1[1] + q0[2] * q1[2] + q0[3] * q1[3];
    if dot < 0.0 {
        q1 = [-q1[0], -q1[1], -q1[2], -q1[3]];
    }
    normalize_quat([
        q0[0] + (q1[0] - q0[0]) * a,
        q0[1] + (q1[1] - q0[1]) * a,
        q0[2] + (q1[2] - q0[2]) * a,
        q0[3] + (q1[3] - q0[3]) * a,
    ])
}

struct PendingSceneSkin {
    mesh_node_idx: usize,
    name: String,
    bind_poses_row_major: Vec<[f32; 16]>,
    bone_refs: Vec<Option<UnityRef>>,
    root_bone_ref: Option<UnityRef>,
}

struct ExtractCtx<'a> {
    workspace: &'a UnityWorkspaceData,
    opts: &'a GltfSceneOptions,
    roots: Vec<usize>,
    nodes: Vec<SceneNode>,
    meshes: Vec<SceneMesh>,
    materials: Vec<SceneMaterial>,
    textures: Vec<SceneTexture>,
    skins: Vec<SceneSkin>,
    transform_to_node: HashMap<UnityRef, usize>,
    visiting_transforms: HashSet<UnityRef>,
    tex_cache: HashMap<UnityRef, usize>,
    used_tex_stems: HashSet<String>,
    mat_cache: HashMap<UnityRef, usize>,
    mesh_cache: HashMap<(UnityRef, Vec<Option<usize>>), usize>,
    pending_skins: Vec<PendingSceneSkin>,
    collected_clips: Vec<UnityRef>,
    avatar_tos: HashMap<u32, String>,
}

impl<'a> ExtractCtx<'a> {
    fn new(workspace: &'a UnityWorkspaceData, opts: &'a GltfSceneOptions) -> Self {
        Self {
            workspace,
            opts,
            roots: Vec::new(),
            nodes: Vec::new(),
            meshes: Vec::new(),
            materials: Vec::new(),
            textures: Vec::new(),
            skins: Vec::new(),
            transform_to_node: HashMap::new(),
            visiting_transforms: HashSet::new(),
            tex_cache: HashMap::new(),
            used_tex_stems: HashSet::new(),
            mat_cache: HashMap::new(),
            mesh_cache: HashMap::new(),
            pending_skins: Vec::new(),
            collected_clips: Vec::new(),
            avatar_tos: HashMap::new(),
        }
    }

    fn unity_version_for(&self, file_index: usize) -> &str {
        self.workspace
            .files
            .get(file_index)
            .map(|f| f.file.unity_version.as_str())
            .filter(|v| !v.is_empty() && !v.starts_with("0."))
            .unwrap_or(&self.opts.fallback_unity_version)
    }

    fn try_read_object(&self, uref: UnityRef) -> Option<UnityObject> {
        let (loaded, info) = self.workspace.object_info(uref.0, uref.1)?;
        read_object(loaded, info).ok()
    }

    fn resolve_pptr(&self, source_file: usize, val: &UnityValue) -> Option<UnityRef> {
        let obj = val.as_object()?;
        let pid = obj.get("m_PathID").and_then(extract_i64_field)?;
        if pid == 0 {
            return None;
        }
        let fid = obj.get("m_FileID").and_then(extract_i32_field).unwrap_or(0);
        let target_file = self.workspace.resolve_external_file(source_file, fid)?;
        Some((target_file, pid))
    }

    fn push_node(&mut self, node: SceneNode) -> usize {
        let idx = self.nodes.len();
        let parent = node.parent;
        self.nodes.push(node);
        if let Some(p_idx) = parent
            && let Some(p) = self.nodes.get_mut(p_idx)
        {
            p.children.push(idx);
        }
        idx
    }

    fn resolve_initial_transforms(&self, roots: &[AssetRef]) -> Vec<UnityRef> {
        resolve_scene_root_transforms(self.workspace, roots)
    }

    fn build_transform_subtree(
        &mut self,
        tr_ref: UnityRef,
        parent_idx: Option<usize>,
    ) -> CodecResult<Option<usize>> {
        if let Some(&existing) = self.transform_to_node.get(&tr_ref) {
            return Ok(Some(existing));
        }
        if !self.visiting_transforms.insert(tr_ref) {
            return Ok(None);
        }
        let Some(tr_obj) = self.try_read_object(tr_ref) else {
            self.visiting_transforms.remove(&tr_ref);
            return Ok(None);
        };
        let props = tr_obj.class.properties();
        let (t, r, s) = extract_local_trs_rh(props);

        let mut node_name = format!("Transform_{}", tr_ref.1);
        let mut go_components = Vec::new();
        if let Some(go_ref) = props
            .get("m_GameObject")
            .and_then(|v| self.resolve_pptr(tr_ref.0, v))
            && let Some(go_obj) = self.try_read_object(go_ref)
        {
            if let Some(n) = go_obj.name()
                && !n.is_empty()
            {
                node_name = n;
            }
            if let Some(arr) = go_obj
                .class
                .properties()
                .get("m_Component")
                .and_then(|v| v.as_array())
            {
                for item in arr {
                    let comp_val = item
                        .as_object()
                        .and_then(|m| m.get("component").or_else(|| m.get("second")))
                        .unwrap_or(item);
                    if let Some(comp_ref) = self.resolve_pptr(go_ref.0, comp_val) {
                        go_components.push(comp_ref);
                    }
                }
            }
        }

        let node_idx = self.push_node(SceneNode {
            name: node_name,
            unity_ref: Some(AssetRef {
                file_index: tr_ref.0,
                path_id: tr_ref.1,
            }),
            parent: parent_idx,
            children: Vec::new(),
            translation: t.unwrap_or([0.0, 0.0, 0.0]),
            rotation: r.unwrap_or([0.0, 0.0, 0.0, 1.0]),
            scale: s.unwrap_or([1.0, 1.0, 1.0]),
            mesh_idx: None,
            skin_idx: None,
            is_bone: false,
        });
        self.transform_to_node.insert(tr_ref, node_idx);

        self.process_gameobject_components(node_idx, &go_components);

        if let Some(children) = props.get("m_Children").and_then(|v| v.as_array()) {
            let child_refs: Vec<UnityRef> = children
                .iter()
                .filter_map(|item| self.resolve_pptr(tr_ref.0, item))
                .collect();
            for c_ref in child_refs {
                let _ = self.build_transform_subtree(c_ref, Some(node_idx))?;
            }
        }

        self.visiting_transforms.remove(&tr_ref);
        Ok(Some(node_idx))
    }

    fn ensure_transform_chain_in_scene(&mut self, tr_ref: UnityRef) -> Option<usize> {
        if let Some(&idx) = self.transform_to_node.get(&tr_ref) {
            return Some(idx);
        }
        let tr_obj = self.try_read_object(tr_ref)?;
        let props = tr_obj.class.properties();
        let father_ref = props
            .get("m_Father")
            .and_then(|v| self.resolve_pptr(tr_ref.0, v));
        let parent_idx = if let Some(f_ref) = father_ref {
            if self.visiting_transforms.contains(&f_ref) {
                None
            } else {
                self.visiting_transforms.insert(tr_ref);
                let p = self.ensure_transform_chain_in_scene(f_ref);
                self.visiting_transforms.remove(&tr_ref);
                p
            }
        } else {
            None
        };

        let (t, r, s) = extract_local_trs_rh(props);
        let mut node_name = format!("Bone_{}", tr_ref.1);
        if let Some(go_ref) = props
            .get("m_GameObject")
            .and_then(|v| self.resolve_pptr(tr_ref.0, v))
            && let Some(go_obj) = self.try_read_object(go_ref)
            && let Some(n) = go_obj.name()
            && !n.is_empty()
        {
            node_name = n;
        }
        let node_idx = self.push_node(SceneNode {
            name: node_name,
            unity_ref: Some(AssetRef {
                file_index: tr_ref.0,
                path_id: tr_ref.1,
            }),
            parent: parent_idx,
            children: Vec::new(),
            translation: t.unwrap_or([0.0, 0.0, 0.0]),
            rotation: r.unwrap_or([0.0, 0.0, 0.0, 1.0]),
            scale: s.unwrap_or([1.0, 1.0, 1.0]),
            mesh_idx: None,
            skin_idx: None,
            is_bone: true,
        });
        self.transform_to_node.insert(tr_ref, node_idx);
        if parent_idx.is_none() {
            self.roots.push(node_idx);
        }
        Some(node_idx)
    }

    fn process_gameobject_components(&mut self, node_idx: usize, comp_refs: &[UnityRef]) {
        let mut mf_mesh: Option<UnityRef> = None;
        let mut mr_mats: Option<Vec<Option<UnityRef>>> = None;
        let mut mr_enabled = true;

        for &comp_ref in comp_refs {
            let Some(comp_obj) = self.try_read_object(comp_ref) else {
                continue;
            };
            let props = comp_obj.class.properties();
            match comp_obj.class_id() {
                33 => {
                    if let Some(m_ref) = props
                        .get("m_Mesh")
                        .and_then(|v| self.resolve_pptr(comp_ref.0, v))
                    {
                        mf_mesh = Some(m_ref);
                    }
                }
                23 => {
                    mr_enabled = props.get("m_Enabled").and_then(extract_i32_field).unwrap_or(1) != 0;
                    mr_mats = Some(self.read_material_list(comp_ref.0, props));
                }
                137 => {
                    let enabled = props.get("m_Enabled").and_then(extract_i32_field).unwrap_or(1) != 0;
                    if !enabled {
                        continue;
                    }
                    if let Some(m_ref) = props
                        .get("m_Mesh")
                        .and_then(|v| self.resolve_pptr(comp_ref.0, v))
                    {
                        let mats = self.read_material_list(comp_ref.0, props);
                        if let Ok(mesh_idx) =
                            self.get_or_build_mesh(m_ref, &mats, self.opts.include_skinning)
                        {
                            let target_node = self.attach_mesh_to_node(node_idx, mesh_idx);
                            let mesh_skin_bp = self.meshes[mesh_idx].bind_poses_row_major.clone();
                            let has_skin = self.opts.include_skinning
                                && !self.meshes[mesh_idx].joints.is_empty()
                                && !mesh_skin_bp.is_empty();
                            if has_skin {
                                let bone_refs = props
                                    .get("m_Bones")
                                    .and_then(|v| v.as_array())
                                    .map(|arr| {
                                        arr.iter()
                                            .map(|item| self.resolve_pptr(comp_ref.0, item))
                                            .collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default();
                                let root_bone_ref = props
                                    .get("m_RootBone")
                                    .and_then(|v| self.resolve_pptr(comp_ref.0, v));
                                if !bone_refs.is_empty() {
                                    self.pending_skins.push(PendingSceneSkin {
                                        mesh_node_idx: target_node,
                                        name: format!("Skin_{}", m_ref.1),
                                        bind_poses_row_major: mesh_skin_bp,
                                        bone_refs,
                                        root_bone_ref,
                                    });
                                }
                            }
                        }
                    }
                }
                95 => {
                    if let Some(av_ref) = props
                        .get("m_Avatar")
                        .and_then(|v| self.resolve_pptr(comp_ref.0, v))
                    {
                        self.collect_avatar_tos(av_ref);
                    }
                    if let Some(ctrl_ref) = props
                        .get("m_Controller")
                        .and_then(|v| self.resolve_pptr(comp_ref.0, v))
                    {
                        self.collect_clips_from_controller(ctrl_ref);
                    }
                }
                111 => {
                    if let Some(c_ref) = props
                        .get("m_Animation")
                        .and_then(|v| self.resolve_pptr(comp_ref.0, v))
                    {
                        self.collected_clips.push(c_ref);
                    }
                    if let Some(arr) = props.get("m_Animations").and_then(|v| v.as_array()) {
                        for item in arr {
                            if let Some(c_ref) = self.resolve_pptr(comp_ref.0, item) {
                                self.collected_clips.push(c_ref);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some(m_ref) = mf_mesh
            && mr_enabled
        {
            let mats = mr_mats.unwrap_or_default();
            if let Ok(mesh_idx) = self.get_or_build_mesh(m_ref, &mats, false) {
                self.attach_mesh_to_node(node_idx, mesh_idx);
            }
        }
    }

    fn attach_mesh_to_node(&mut self, node_idx: usize, mesh_idx: usize) -> usize {
        if self.nodes[node_idx].mesh_idx.is_none() {
            self.nodes[node_idx].mesh_idx = Some(mesh_idx);
            return node_idx;
        }
        let child_idx = self.nodes.len();
        self.push_node(SceneNode {
            name: format!("Renderer_{child_idx}"),
            unity_ref: None,
            parent: Some(node_idx),
            children: Vec::new(),
            translation: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
            mesh_idx: Some(mesh_idx),
            skin_idx: None,
            is_bone: false,
        })
    }

    fn read_material_list(
        &self,
        source_file: usize,
        props: &indexmap::IndexMap<String, UnityValue>,
    ) -> Vec<Option<UnityRef>> {
        props
            .get("m_Materials")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|item| self.resolve_pptr(source_file, item))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn get_or_build_mesh(
        &mut self,
        mesh_ref: UnityRef,
        mat_refs: &[Option<UnityRef>],
        want_skin: bool,
    ) -> CodecResult<usize> {
        let mut mat_indices = Vec::with_capacity(mat_refs.len());
        if self.opts.include_materials {
            for mr in mat_refs {
                mat_indices.push(mr.and_then(|r| self.get_or_create_material(r)));
            }
        } else {
            mat_indices.resize(mat_refs.len(), None);
        }

        let cache_key = (mesh_ref, mat_indices.clone());
        if let Some(&idx) = self.mesh_cache.get(&cache_key) {
            return Ok(idx);
        }

        let mesh_obj = self.try_read_object(mesh_ref).ok_or_else(|| {
            CodecError::MalformedData(format!(
                "Mesh {} missing from file #{}",
                mesh_ref.1, mesh_ref.0
            ))
        })?;
        let ver = self.unity_version_for(mesh_ref.0).to_owned();
        let decoded = decode_mesh_full(&mesh_obj, &ver, &self.workspace.streams)?;
        let base = &decoded.base;
        if base.positions.is_empty() || base.indices.is_empty() {
            return Err(CodecError::MalformedData(format!(
                "Mesh '{}' has empty geometry",
                base.name
            )));
        }

        let positions: Vec<[f32; 3]> = base.positions.iter().map(|p| [-p[0], p[1], p[2]]).collect();
        let normals: Vec<[f32; 3]> = base
            .normals
            .iter()
            .map(|n| normalize3([-n[0], n[1], n[2]]))
            .collect();
        let uvs = base.uvs.clone();

        let (joints, weights, bind_poses_row_major) = if want_skin
            && let Some(skin) = &decoded.skin
            && skin.joints.len() == positions.len()
            && skin.weights.len() == positions.len()
            && !skin.bind_poses_row_major.is_empty()
        {
            (
                skin.joints.clone(),
                skin.weights.clone(),
                skin.bind_poses_row_major.clone(),
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };

        let slices = if base.submeshes.is_empty() {
            vec![devx_plugin_api::SubMeshSlice {
                first_index: 0,
                index_count: base.indices.len() as u32,
                topology: 0,
            }]
        } else {
            base.submeshes.clone()
        };

        let mut indices = Vec::with_capacity(base.indices.len());
        let mut submeshes = Vec::with_capacity(slices.len());
        for (sub_i, slice) in slices.into_iter().enumerate() {
            let start = slice.first_index as usize;
            let end = start
                .saturating_add(slice.index_count as usize)
                .min(base.indices.len());
            if start + 3 > end {
                continue;
            }
            let raw = &base.indices[start..end];
            let tri_len = (raw.len() / 3) * 3;
            let out_start = indices.len();
            for tri in raw[..tri_len].as_chunks::<3>().0 {
                indices.push(tri[0]);
                indices.push(tri[2]);
                indices.push(tri[1]);
            }
            let mat_idx = mat_indices
                .get(sub_i)
                .copied()
                .flatten()
                .or_else(|| mat_indices.last().copied().flatten());
            submeshes.push(SceneSubMesh {
                index_start: out_start,
                index_count: tri_len,
                material_idx: mat_idx,
            });
        }

        let blend_shapes: Vec<DecodedBlendShape> = decoded
            .blend_shapes
            .iter()
            .map(|bs| DecodedBlendShape {
                name: bs.name.clone(),
                vertex_indices: bs.vertex_indices.clone(),
                position_deltas: bs
                    .position_deltas
                    .iter()
                    .map(|p| [-p[0], p[1], p[2]])
                    .collect(),
                normal_deltas: bs
                    .normal_deltas
                    .iter()
                    .map(|n| [-n[0], n[1], n[2]])
                    .collect(),
                has_normals: bs.has_normals,
                default_weight: bs.default_weight,
            })
            .collect();

        let idx = self.meshes.len();
        self.meshes.push(SceneMesh {
            name: base.name.clone(),
            positions,
            normals,
            uvs,
            indices,
            submeshes,
            joints,
            weights,
            bind_poses_row_major,
            blend_shapes,
        });
        self.mesh_cache.insert(cache_key, idx);
        Ok(idx)
    }

    fn finalize_pending_skins(&mut self) {
        let pending = std::mem::take(&mut self.pending_skins);
        for ps in pending {
            let mut joint_nodes = Vec::with_capacity(ps.bone_refs.len());
            let mut unresolved = 0usize;
            for b_opt in &ps.bone_refs {
                if let Some(b_ref) = *b_opt
                    && let Some(j_idx) = self.ensure_transform_chain_in_scene(b_ref)
                {
                    self.nodes[j_idx].is_bone = true;
                    joint_nodes.push(j_idx);
                } else {
                    unresolved += 1;
                    joint_nodes.push(ps.mesh_node_idx);
                }
            }
            if joint_nodes.is_empty() || unresolved == joint_nodes.len() {
                continue;
            }

            let root_bone_idx = ps
                .root_bone_ref
                .and_then(|r| self.ensure_transform_chain_in_scene(r))
                .inspect(|&idx| {
                    self.nodes[idx].is_bone = true;
                });

            let ibm: Vec<[f32; 16]> = (0..joint_nodes.len())
                .map(|j_slot| {
                    let row_major = ps
                        .bind_poses_row_major
                        .get(j_slot)
                        .copied()
                        .unwrap_or(IDENTITY_MAT4);
                    unity_bindpose_row_major_to_gltf_col_major(&row_major)
                })
                .collect();

            let skin_idx = self.skins.len();
            self.skins.push(SceneSkin {
                name: ps.name,
                mesh_node_idx: ps.mesh_node_idx,
                root_bone_idx,
                joint_nodes,
                inverse_bind_matrices_col_major: ibm,
            });
            self.nodes[ps.mesh_node_idx].skin_idx = Some(skin_idx);
        }
    }

    fn get_or_create_material(&mut self, mat_ref: UnityRef) -> Option<usize> {
        if let Some(&idx) = self.mat_cache.get(&mat_ref) {
            return Some(idx);
        }
        let mat_obj = self.try_read_object(mat_ref)?;
        if mat_obj.class_id() != 21 {
            return None;
        }
        let decoded: DecodedMaterial = decode_material(&mat_obj).ok()?;
        let albedo_tex = if self.opts.include_textures {
            decoded.albedo_slot().and_then(|s| {
                self.resolve_tex_pptr(mat_ref.0, s.texture_file_id, s.texture_path_id)
                    .and_then(|r| self.get_or_create_texture(r))
            })
        } else {
            None
        };
        let normal_tex = if self.opts.include_textures {
            decoded.normal_slot().and_then(|s| {
                self.resolve_tex_pptr(mat_ref.0, s.texture_file_id, s.texture_path_id)
                    .and_then(|r| self.get_or_create_texture(r))
            })
        } else {
            None
        };
        let emissive_tex = if self.opts.include_textures {
            decoded.emission_slot().and_then(|s| {
                self.resolve_tex_pptr(mat_ref.0, s.texture_file_id, s.texture_path_id)
                    .and_then(|r| self.get_or_create_texture(r))
            })
        } else {
            None
        };
        let metallic_gloss_tex = if self.opts.include_textures {
            decoded.metallic_gloss_slot().and_then(|s| {
                self.resolve_tex_pptr(mat_ref.0, s.texture_file_id, s.texture_path_id)
                    .and_then(|r| self.get_or_create_texture(r))
            })
        } else {
            None
        };

        let idx = self.materials.len();
        self.materials.push(SceneMaterial {
            name: decoded.name.clone(),
            base_color: decoded.base_color(),
            metallic: decoded.metallic_factor(),
            roughness: decoded.roughness_factor(),
            emissive: decoded.emissive_color().unwrap_or([0.0, 0.0, 0.0]),
            double_sided: decoded.double_sided(),
            alpha_mode: decoded.alpha_mode(),
            albedo_tex,
            normal_tex,
            emissive_tex,
            metallic_gloss_tex,
        });
        self.mat_cache.insert(mat_ref, idx);
        Some(idx)
    }

    fn resolve_tex_pptr(&self, source_file: usize, file_id: i32, path_id: i64) -> Option<UnityRef> {
        if path_id == 0 {
            return None;
        }
        let f = self.workspace.resolve_external_file(source_file, file_id)?;
        Some((f, path_id))
    }

    fn get_or_create_texture(&mut self, tex_ref: UnityRef) -> Option<usize> {
        if let Some(&idx) = self.tex_cache.get(&tex_ref) {
            return Some(idx);
        }
        let tex_obj = self.try_read_object(tex_ref)?;
        if tex_obj.class_id() != 28 {
            return None;
        }
        let ver = self.unity_version_for(tex_ref.0).to_owned();
        let mut decoded = decode_texture2d(
            &tex_obj,
            &ver,
            &self.workspace.streams,
            TextureDecodeOptions {
                flip_vertical: true,
                ..Default::default()
            },
        )
        .ok()?;

        if let Some(max_dim) = self.opts.max_texture_dim
            && max_dim > 0
            && (decoded.width > max_dim || decoded.height > max_dim)
            && let Some(buf) = RgbaImage::from_raw(decoded.width, decoded.height, decoded.rgba8.clone())
        {
            let dyn_img = DynamicImage::ImageRgba8(buf);
            let resized = dyn_img
                .resize(max_dim, max_dim, FilterType::Triangle)
                .to_rgba8();
            decoded.width = resized.width();
            decoded.height = resized.height();
            decoded.rgba8 = resized.into_raw();
        }

        let png_bytes = encode_png(&decoded).ok()?;
        let tex_name = tex_obj
            .name()
            .unwrap_or_else(|| format!("Texture_{}", tex_ref.1));
        let clean: String = tex_name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let base_stem = if clean.is_empty() {
            format!("tex_{}", tex_ref.1)
        } else {
            clean
        };
        let mut stem = base_stem.clone();
        let mut n = 2usize;
        while !self.used_tex_stems.insert(stem.clone()) {
            stem = format!("{base_stem}_{n}");
            n += 1;
        }

        let idx = self.textures.len();
        self.textures.push(SceneTexture {
            name: tex_name,
            file_stem: stem,
            image: Arc::new(decoded),
            png_bytes: Arc::new(png_bytes),
        });
        self.tex_cache.insert(tex_ref, idx);
        Some(idx)
    }

    fn collect_avatar_tos(&mut self, avatar_ref: UnityRef) {
        let Some(obj) = self.try_read_object(avatar_ref) else {
            return;
        };
        if let Some(arr) = obj
            .class
            .properties()
            .get("m_TOS")
            .and_then(|v| v.as_array())
        {
            for item in arr {
                if let Some(pair) = item.as_object()
                    && let Some(hash) = pair.get("first").and_then(extract_i64_field)
                    && let Some(path) = pair.get("second").and_then(|v| v.as_str())
                {
                    self.avatar_tos.insert(hash as u32, path.to_owned());
                }
            }
        }
    }

    fn collect_clips_from_controller(&mut self, ctrl_ref: UnityRef) {
        let Some(obj) = self.try_read_object(ctrl_ref) else {
            return;
        };
        let props = obj.class.properties();
        if let Some(arr) = props.get("m_AnimationClips").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(c_ref) = self.resolve_pptr(ctrl_ref.0, item) {
                    self.collected_clips.push(c_ref);
                }
            }
        }
        if let Some(base_ctrl) = props
            .get("m_Controller")
            .and_then(|v| self.resolve_pptr(ctrl_ref.0, v))
            && base_ctrl != ctrl_ref
        {
            self.collect_clips_from_controller(base_ctrl);
        }
        if let Some(overrides) = props.get("m_Clips").and_then(|v| v.as_array()) {
            for ov in overrides {
                if let Some(m) = ov.as_object()
                    && let Some(clip_val) = m.get("m_OverrideClip")
                    && let Some(c_ref) = self.resolve_pptr(ctrl_ref.0, clip_val)
                {
                    self.collected_clips.push(c_ref);
                }
            }
        }
    }

    fn collect_clip_refs(&self, roots: &[AssetRef], extra_clips: &[AssetRef]) -> Vec<UnityRef> {
        collect_fallback_clip_refs(self.workspace, roots, extra_clips, &self.collected_clips)
    }
}

fn container_sibling_indices<'a, I>(containers: I, file_index: usize) -> Vec<usize>
where
    I: Iterator<Item = Option<&'a str>> + Clone,
{
    let Some(target) = containers.clone().nth(file_index) else {
        return Vec::new();
    };
    if let Some(cn) = target {
        let mut indices: Vec<usize> = containers
            .enumerate()
            .filter(|(_, c)| *c == Some(cn))
            .map(|(idx, _)| idx)
            .collect();
        if !indices.contains(&file_index) {
            indices.push(file_index);
        }
        indices
    } else {
        vec![file_index]
    }
}

/// Returns all workspace file indices sharing the same `.ab` bundle container (`container_name`)
/// as `file_index`, or `[file_index]` when standalone.
pub fn bundle_sibling_file_indices(
    workspace: &UnityWorkspaceData,
    file_index: usize,
) -> Vec<usize> {
    container_sibling_indices(
        workspace.files.iter().map(|f| f.container_name.as_deref()),
        file_index,
    )
}

fn read_ws_object(workspace: &UnityWorkspaceData, uref: UnityRef) -> Option<UnityObject> {
    let (loaded, info) = workspace.object_info(uref.0, uref.1)?;
    read_object(loaded, info).ok()
}

fn resolve_ws_pptr(
    workspace: &UnityWorkspaceData,
    source_file: usize,
    val: &UnityValue,
) -> Option<UnityRef> {
    let obj = val.as_object()?;
    let pid = obj.get("m_PathID").and_then(extract_i64_field)?;
    if pid == 0 {
        return None;
    }
    let fid = obj.get("m_FileID").and_then(extract_i32_field).unwrap_or(0);
    let target_file = workspace.resolve_external_file(source_file, fid)?;
    Some((target_file, pid))
}

fn find_ws_transform_of_gameobject(
    workspace: &UnityWorkspaceData,
    go_ref: UnityRef,
    go_obj: &UnityObject,
) -> Option<UnityRef> {
    let arr = go_obj.class.properties().get("m_Component")?.as_array()?;
    for item in arr {
        let comp_val = item
            .as_object()
            .and_then(|m| m.get("component").or_else(|| m.get("second")))
            .unwrap_or(item);
        if let Some(comp_ref) = resolve_ws_pptr(workspace, go_ref.0, comp_val)
            && let Some((_, info)) = workspace.object_info(comp_ref.0, comp_ref.1)
            && (info.type_id == 4 || info.type_id == 224)
        {
            return Some(comp_ref);
        }
    }
    None
}

fn root_transforms_in_file(workspace: &UnityWorkspaceData, file_index: usize) -> Vec<UnityRef> {
    let Some(loaded) = workspace.files.get(file_index) else {
        return Vec::new();
    };
    let mut roots = Vec::new();
    for info in &loaded.file.objects {
        if info.type_id != 4 && info.type_id != 224 {
            continue;
        }
        if let Ok(obj) = read_object(loaded, info) {
            let father_pid = obj
                .class
                .properties()
                .get("m_Father")
                .and_then(|v| v.as_object())
                .and_then(|m| m.get("m_PathID"))
                .and_then(extract_i64_field)
                .unwrap_or(0);
            if father_pid == 0 {
                roots.push((file_index, info.path_id));
            }
        }
    }
    roots
}

fn asset_bundle_asset_pptrs(bundle_obj: &UnityObject) -> Vec<&UnityValue> {
    let mut pptrs = Vec::new();
    let props = bundle_obj.class.properties();

    if let Some(main_asset) = props.get("m_MainAsset").and_then(|v| v.as_object())
        && let Some(asset_val) = main_asset.get("asset")
    {
        pptrs.push(asset_val);
    }

    if let Some(container) = props.get("m_Container") {
        match container {
            UnityValue::Array(items) => {
                for item in items {
                    let info_val = match item {
                        UnityValue::Object(m) => m.get("second").unwrap_or(item),
                        UnityValue::Array(pair) => pair.get(1).unwrap_or(item),
                        _ => item,
                    };
                    if let Some(asset_val) = info_val.as_object().and_then(|m| m.get("asset")) {
                        pptrs.push(asset_val);
                    }
                }
            }
            UnityValue::Object(map) => {
                for info_val in map.values() {
                    if let Some(asset_val) = info_val.as_object().and_then(|m| m.get("asset")) {
                        pptrs.push(asset_val);
                    }
                }
            }
            _ => {}
        }
    }

    pptrs
}

fn is_streamed_scene_asset_bundle(bundle_obj: &UnityObject) -> bool {
    match bundle_obj
        .class
        .properties()
        .get("m_IsStreamedSceneAssetBundle")
    {
        Some(UnityValue::Bool(b)) => *b,
        Some(v) => extract_i32_field(v).unwrap_or(0) != 0,
        None => false,
    }
}

fn push_resolved_transform_target(
    workspace: &UnityWorkspaceData,
    target: UnityRef,
    seen: &mut HashSet<UnityRef>,
    out: &mut Vec<UnityRef>,
) {
    let Some(obj) = read_ws_object(workspace, target) else {
        return;
    };
    match obj.class_id() {
        4 | 224 => {
            if seen.insert(target) {
                out.push(target);
            }
        }
        1 => {
            if let Some(tr) = find_ws_transform_of_gameobject(workspace, target, &obj)
                && seen.insert(tr)
            {
                out.push(tr);
            }
        }
        _ => {}
    }
}

/// Resolves arbitrary hierarchy roots (`Transform`, `RectTransform`, `GameObject`,
/// `AssetBundle`, or component with `m_GameObject`) into deduplicated initial root
/// `(file_index, path_id)` transform refs.
///
/// For `AssetBundle` (`class_id == 142`): resolves explicit `m_MainAsset`/`m_Container`
/// targets first, and for streamed-scene bundles (`m_IsStreamedSceneAssetBundle != 0`)
/// or bundles without resolved transform pointers scans all sibling `SerializedFile`s
/// sharing the same `.ab` container for root `Transform`s (`m_Father == 0`).
pub fn resolve_scene_root_transforms(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
) -> Vec<UnityRef> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for r in roots {
        let uref = (r.file_index, r.path_id);
        let Some(obj) = read_ws_object(workspace, uref) else {
            continue;
        };
        match obj.class_id() {
            4 | 224 => {
                if seen.insert(uref) {
                    out.push(uref);
                }
            }
            1 => {
                if let Some(tr) = find_ws_transform_of_gameobject(workspace, uref, &obj)
                    && seen.insert(tr)
                {
                    out.push(tr);
                }
            }
            142 => {
                let before_len = out.len();
                for pptr_val in asset_bundle_asset_pptrs(&obj) {
                    if let Some(target) = resolve_ws_pptr(workspace, r.file_index, pptr_val) {
                        push_resolved_transform_target(workspace, target, &mut seen, &mut out);
                    }
                }
                if out.len() == before_len || is_streamed_scene_asset_bundle(&obj) {
                    for sibling_idx in bundle_sibling_file_indices(workspace, r.file_index) {
                        for tr in root_transforms_in_file(workspace, sibling_idx) {
                            if seen.insert(tr) {
                                out.push(tr);
                            }
                        }
                    }
                }
            }
            _ => {
                if let Some(go_ref) = obj
                    .class
                    .properties()
                    .get("m_GameObject")
                    .and_then(|v| resolve_ws_pptr(workspace, uref.0, v))
                    && let Some(go_obj) = read_ws_object(workspace, go_ref)
                    && let Some(tr) = find_ws_transform_of_gameobject(workspace, go_ref, &go_obj)
                    && seen.insert(tr)
                {
                    out.push(tr);
                }
            }
        }
    }
    out
}

/// Resolves standalone `Mesh` (`class_id == 43`) roots from explicit refs or
/// mesh-only `AssetBundle` (`class_id == 142`) sibling files when no `Transform` exists.
pub fn resolve_standalone_mesh_roots(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
) -> Vec<AssetRef> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for r in roots {
        let uref = (r.file_index, r.path_id);
        let Some(obj) = read_ws_object(workspace, uref) else {
            continue;
        };
        if obj.class_id() == 43 {
            if seen.insert(uref) {
                out.push(*r);
            }
        } else if obj.class_id() == 142 {
            let before_len = out.len();
            for pptr_val in asset_bundle_asset_pptrs(&obj) {
                if let Some(target) = resolve_ws_pptr(workspace, r.file_index, pptr_val)
                    && let Some((_, info)) = workspace.object_info(target.0, target.1)
                    && info.type_id == 43
                    && seen.insert(target)
                {
                    out.push(AssetRef {
                        file_index: target.0,
                        path_id: target.1,
                    });
                }
            }
            if out.len() == before_len {
                for sibling_idx in bundle_sibling_file_indices(workspace, r.file_index) {
                    if let Some(loaded) = workspace.files.get(sibling_idx) {
                        for info in &loaded.file.objects {
                            if info.type_id == 43 && seen.insert((sibling_idx, info.path_id)) {
                                out.push(AssetRef {
                                    file_index: sibling_idx,
                                    path_id: info.path_id,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// Collects explicit `extra_clips`, controller-referenced `collected_clips`, or fallback
/// `AnimationClip` (`class_id == 74`) assets from root files and their `.ab` bundle siblings.
pub fn collect_fallback_clip_refs(
    workspace: &UnityWorkspaceData,
    roots: &[AssetRef],
    extra_clips: &[AssetRef],
    collected_clips: &[UnityRef],
) -> Vec<UnityRef> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for r in extra_clips {
        let u = (r.file_index, r.path_id);
        if seen.insert(u) {
            out.push(u);
        }
    }
    for &u in collected_clips {
        if seen.insert(u) {
            out.push(u);
        }
    }
    if out.is_empty() {
        let mut file_indices = BTreeSet::new();
        for r in roots {
            for sibling_idx in bundle_sibling_file_indices(workspace, r.file_index) {
                file_indices.insert(sibling_idx);
            }
        }
        for f_idx in file_indices {
            if let Some(loaded) = workspace.files.get(f_idx) {
                for info in &loaded.file.objects {
                    if info.type_id == 74 {
                        let u = (f_idx, info.path_id);
                        if seen.insert(u) {
                            out.push(u);
                        }
                    }
                }
            }
        }
    }
    out
}

/// Normalizes a `[x, y, z]` vector cleanly.
pub fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-6 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 1.0, 0.0]
    }
}

/// Normalizes a quaternion `[x, y, z, w]`.
pub fn normalize_quat(q: [f32; 4]) -> [f32; 4] {
    let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if len > 1e-6 {
        [q[0] / len, q[1] / len, q[2] / len, q[3] / len]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// Builds a column-major 4x4 matrix from local translation, unit quaternion `[x, y, z, w]`, and scale.
pub fn mat4_from_trs(t: [f32; 3], q: [f32; 4], s: [f32; 3]) -> [f32; 16] {
    let [x, y, z, w] = normalize_quat(q);
    let xx = x * x;
    let yy = y * y;
    let zz = z * z;
    let xy = x * y;
    let xz = x * z;
    let yz = y * z;
    let wx = w * x;
    let wy = w * y;
    let wz = w * z;

    [
        (1.0 - 2.0 * (yy + zz)) * s[0],
        (2.0 * (xy + wz)) * s[0],
        (2.0 * (xz - wy)) * s[0],
        0.0,
        (2.0 * (xy - wz)) * s[1],
        (1.0 - 2.0 * (xx + zz)) * s[1],
        (2.0 * (yz + wx)) * s[1],
        0.0,
        (2.0 * (xz + wy)) * s[2],
        (2.0 * (yz - wx)) * s[2],
        (1.0 - 2.0 * (xx + yy)) * s[2],
        0.0,
        t[0],
        t[1],
        t[2],
        1.0,
    ]
}

/// Multiplies two column-major 4x4 matrices (`a * b`).
pub fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for c in 0..4 {
        for r in 0..4 {
            out[c * 4 + r] = a[r] * b[c * 4]
                + a[4 + r] * b[c * 4 + 1]
                + a[8 + r] * b[c * 4 + 2]
                + a[12 + r] * b[c * 4 + 3];
        }
    }
    out
}

/// Transforms a 3D point by a column-major 4x4 matrix.
pub fn mat4_transform_point(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

/// Transforms and renormalizes a 3D direction/normal by the linear 3x3 upper block of `m`.
pub fn mat4_transform_normal(m: &[f32; 16], n: [f32; 3]) -> [f32; 3] {
    normalize3([
        m[0] * n[0] + m[4] * n[1] + m[8] * n[2],
        m[1] * n[0] + m[5] * n[1] + m[9] * n[2],
        m[2] * n[0] + m[6] * n[1] + m[10] * n[2],
    ])
}

/// Inverts a general 4x4 column-major matrix via cofactor expansion.
pub fn mat4_invert(m: &[f32; 16]) -> Option<[f32; 16]> {
    let mut inv = [0.0f32; 16];
    inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15]
        + m[9] * m[7] * m[14] + m[13] * m[6] * m[11] - m[13] * m[7] * m[10];
    inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15]
        - m[8] * m[7] * m[14] - m[12] * m[6] * m[11] + m[12] * m[7] * m[10];
    inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15]
        + m[8] * m[7] * m[13] + m[12] * m[5] * m[11] - m[12] * m[7] * m[9];
    inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14]
        - m[8] * m[6] * m[13] - m[12] * m[5] * m[10] + m[12] * m[6] * m[9];
    inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15]
        - m[9] * m[3] * m[14] - m[13] * m[2] * m[11] + m[13] * m[3] * m[10];
    inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15]
        + m[8] * m[3] * m[14] + m[12] * m[2] * m[11] - m[12] * m[3] * m[10];
    inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15]
        - m[8] * m[3] * m[13] - m[12] * m[1] * m[11] + m[12] * m[3] * m[9];
    inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14]
        + m[8] * m[2] * m[13] + m[12] * m[1] * m[10] - m[12] * m[2] * m[9];
    inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15]
        + m[5] * m[3] * m[14] + m[13] * m[2] * m[7] - m[13] * m[3] * m[6];
    inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15]
        - m[4] * m[3] * m[14] - m[12] * m[2] * m[7] + m[12] * m[3] * m[6];
    inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15]
        + m[4] * m[3] * m[13] + m[12] * m[1] * m[7] - m[12] * m[3] * m[5];
    inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14]
        - m[4] * m[2] * m[13] - m[12] * m[1] * m[6] + m[12] * m[2] * m[5];
    inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11]
        - m[5] * m[3] * m[10] - m[9] * m[2] * m[7] + m[9] * m[3] * m[6];
    inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11]
        + m[4] * m[3] * m[10] + m[8] * m[2] * m[7] - m[8] * m[3] * m[6];
    inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11]
        - m[4] * m[3] * m[9] - m[8] * m[1] * m[7] + m[8] * m[3] * m[5];
    inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10]
        + m[4] * m[2] * m[9] + m[8] * m[1] * m[6] - m[8] * m[2] * m[5];

    let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
    if det.abs() <= 1e-10 || !det.is_finite() {
        return None;
    }
    let inv_det = 1.0 / det;
    for v in &mut inv {
        *v *= inv_det;
    }
    Some(inv)
}

/// Converts a unit quaternion `[x, y, z, w]` to extrinsic XYZ Euler angles in degrees
/// (`R = Rz(z) * Ry(y) * Rx(x)`, matching FBX `eEulerXYZ`).
pub fn quat_to_euler_xyz_deg(q: [f32; 4]) -> [f64; 3] {
    let [x, y, z, w] = normalize_quat(q);
    let (x, y, z, w) = (x as f64, y as f64, z as f64, w as f64);

    // Rotation matrix elements for R = Rz * Ry * Rx:
    let m00 = 1.0 - 2.0 * (y * y + z * z);
    let m10 = 2.0 * (x * y + w * z);
    let m20 = 2.0 * (x * z - w * y);
    let m21 = 2.0 * (y * z + w * x);
    let m22 = 1.0 - 2.0 * (x * x + y * y);
    let m01 = 2.0 * (x * y - w * z);
    let m11 = 1.0 - 2.0 * (x * x + z * z);

    let sy = (-m20).clamp(-1.0, 1.0);
    let ry = sy.asin();
    let (rx, rz) = if m20.abs() < 0.999_999 {
        (m21.atan2(m22), m10.atan2(m00))
    } else {
        ((-m01).atan2(m11), 0.0)
    };
    [rx.to_degrees(), ry.to_degrees(), rz.to_degrees()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mat4_invert_roundtrips_trs_and_lbs_skinning() {
        let trs = mat4_from_trs([1.5, -2.0, 3.25], [0.0, 0.3826834, 0.0, 0.9238795], [1.0, 1.0, 1.0]);
        let inv = mat4_invert(&trs).expect("invert");
        let prod = mat4_mul(&trs, &inv);
        for c in 0..4 {
            for r in 0..4 {
                let expected = if r == c { 1.0 } else { 0.0 };
                assert!((prod[c * 4 + r] - expected).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn euler_xyz_deg_matches_pure_axis_rotations() {
        let s = std::f32::consts::FRAC_1_SQRT_2;
        // +90 deg around Y: q = [0, sin(45), 0, cos(45)]
        let e = quat_to_euler_xyz_deg([0.0, s, 0.0, s]);
        assert!(e[0].abs() < 1e-4);
        assert!((e[1] - 90.0).abs() < 1e-4);
        assert!(e[2].abs() < 1e-4);
    }

    #[test]
    fn evaluates_lbs_animation_on_rigged_scene() {
        let mut doc = SceneDocument {
            name: "rig".into(),
            roots: vec![0],
            nodes: vec![
                SceneNode {
                    name: "Root".into(),
                    unity_ref: None,
                    parent: None,
                    children: vec![1, 2],
                    translation: [0.0, 0.0, 0.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                    scale: [1.0, 1.0, 1.0],
                    mesh_idx: None,
                    skin_idx: None,
                    is_bone: false,
                },
                SceneNode {
                    name: "MeshNode".into(),
                    unity_ref: None,
                    parent: Some(0),
                    children: vec![],
                    translation: [0.0, 0.0, 0.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                    scale: [1.0, 1.0, 1.0],
                    mesh_idx: Some(0),
                    skin_idx: Some(0),
                    is_bone: false,
                },
                SceneNode {
                    name: "Bone0".into(),
                    unity_ref: None,
                    parent: Some(0),
                    children: vec![],
                    translation: [0.0, 0.0, 0.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                    scale: [1.0, 1.0, 1.0],
                    mesh_idx: None,
                    skin_idx: None,
                    is_bone: true,
                },
            ],
            meshes: vec![SceneMesh {
                name: "M".into(),
                positions: vec![[0.0, 1.0, 0.0]],
                normals: vec![[0.0, 0.0, 1.0]],
                uvs: vec![[0.0, 0.0]],
                indices: vec![],
                submeshes: vec![],
                joints: vec![[0, 0, 0, 0]],
                weights: vec![[1.0, 0.0, 0.0, 0.0]],
                bind_poses_row_major: vec![],
                blend_shapes: vec![DecodedBlendShape {
                    name: "Tall".into(),
                    vertex_indices: vec![0],
                    position_deltas: vec![[0.0, 2.0, 0.0]],
                    normal_deltas: vec![[0.0, 0.0, 0.0]],
                    has_normals: false,
                    default_weight: 0.0,
                }],
            }],
            materials: vec![],
            textures: vec![],
            skins: vec![SceneSkin {
                name: "S".into(),
                mesh_node_idx: 1,
                root_bone_idx: Some(2),
                joint_nodes: vec![2],
                inverse_bind_matrices_col_major: vec![IDENTITY_MAT4],
            }],
            animations: vec![SceneAnimClip {
                name: "Move".into(),
                duration_secs: 1.0,
                sample_rate: 30.0,
                tracks: vec![SceneAnimTrack {
                    node_idx: 2,
                    property: TrackProperty::Translation,
                    times: vec![0.0, 1.0],
                    values: vec![0.0, 0.0, 0.0, 2.0, 0.0, 0.0],
                }],
            }],
            avatar_tos: HashMap::new(),
        };

        let pose_half = evaluate_scene_pose(&doc, Some(0), 0.5);
        let deformed = deform_node_mesh_world(&doc, 1, &pose_half).expect("deform");
        assert!((deformed.positions[0][0] - 1.0).abs() < 1e-5);
        assert!((deformed.positions[0][1] - 1.0).abs() < 1e-5);
        assert!(doc.meshes[0].positions[0][0].abs() < 1e-5);

        let morphed =
            deform_node_mesh_world_with_weights(&doc, 1, &pose_half, Some(&[0.5])).expect("morph");
        assert!((morphed.positions[0][1] - 2.0).abs() < 1e-5);

        let pose_zero = evaluate_scene_pose(&doc, Some(0), 0.0);
        let pose_one = evaluate_scene_pose(&doc, Some(0), 1.0);
        let blended = blend_scene_poses(&doc, &pose_zero, &pose_one, 0.25);
        assert!((blended.world_matrices[2][12] - 0.5).abs() < 1e-5);
        let _ = &mut doc;
    }

    #[test]
    fn bundle_sibling_file_indices_groups_by_container_name() {
        let containers = [
            Some("assets/other.ab"),
            Some("assets/scene/battle_map1.ab"),
            Some("assets/scene/battle_map1.ab"),
            Some("assets/scene/battle_map1.ab"),
            None,
        ];

        assert_eq!(container_sibling_indices(containers.iter().copied(), 0), vec![0]);
        assert_eq!(container_sibling_indices(containers.iter().copied(), 1), vec![1, 2, 3]);
        assert_eq!(container_sibling_indices(containers.iter().copied(), 2), vec![1, 2, 3]);
        assert_eq!(container_sibling_indices(containers.iter().copied(), 4), vec![4]);
        assert!(container_sibling_indices(containers.iter().copied(), 99).is_empty());
    }

    #[test]
    fn asset_bundle_extracts_streamed_scene_flag_and_container_pptrs() {
        use unity_asset_binary::asset::types::ObjectInfo;
        use unity_asset_core::UnityClass;

        let make_pptr = |fid: i64, pid: i64| {
            let mut m = UnityClass::new(0, "PPtr<Object>".into(), "0".into());
            m.set("m_FileID".into(), UnityValue::Integer(fid));
            m.set("m_PathID".into(), UnityValue::Integer(pid));
            UnityValue::Object(m.properties().clone())
        };

        // 1. Streamed Scene AssetBundle (`m_IsStreamedSceneAssetBundle: 1`, `m_Container` has Null PPtrs).
        let mut streamed_cls = UnityClass::new(142, "AssetBundle".into(), "2".into());
        streamed_cls.set("m_IsStreamedSceneAssetBundle".into(), UnityValue::Integer(1));
        let mut main_asset = UnityClass::new(0, "AssetInfo".into(), "0".into());
        main_asset.set("asset".into(), make_pptr(0, 0));
        streamed_cls.set("m_MainAsset".into(), UnityValue::Object(main_asset.properties().clone()));

        let mut pair_obj = UnityClass::new(0, "pair".into(), "0".into());
        pair_obj.set("first".into(), UnityValue::String("Assets/Scenes/battle.unity".into()));
        let mut second_info = UnityClass::new(0, "AssetInfo".into(), "0".into());
        second_info.set("asset".into(), make_pptr(0, 0));
        pair_obj.set("second".into(), UnityValue::Object(second_info.properties().clone()));
        streamed_cls.set(
            "m_Container".into(),
            UnityValue::Array(vec![UnityValue::Object(pair_obj.properties().clone())]),
        );

        let streamed_obj =
            UnityObject::from_info_and_class(ObjectInfo::new(2, 0, 0, 142, 0), streamed_cls);
        assert!(is_streamed_scene_asset_bundle(&streamed_obj));
        let pptrs = asset_bundle_asset_pptrs(&streamed_obj);
        assert_eq!(pptrs.len(), 2);

        // 2. Non-streamed Prefab AssetBundle with valid GameObject PPtr in m_Container.
        let mut prefab_cls = UnityClass::new(142, "AssetBundle".into(), "1".into());
        prefab_cls.set("m_IsStreamedSceneAssetBundle".into(), UnityValue::Bool(false));
        let mut prefab_pair = UnityClass::new(0, "pair".into(), "0".into());
        prefab_pair.set("first".into(), UnityValue::String("Assets/Prefabs/Hero.prefab".into()));
        let mut prefab_info = UnityClass::new(0, "AssetInfo".into(), "0".into());
        prefab_info.set("asset".into(), make_pptr(0, 42));
        prefab_pair.set("second".into(), UnityValue::Object(prefab_info.properties().clone()));
        prefab_cls.set(
            "m_Container".into(),
            UnityValue::Array(vec![UnityValue::Object(prefab_pair.properties().clone())]),
        );

        let prefab_obj =
            UnityObject::from_info_and_class(ObjectInfo::new(1, 0, 0, 142, 0), prefab_cls);
        assert!(!is_streamed_scene_asset_bundle(&prefab_obj));
        let pptrs2 = asset_bundle_asset_pptrs(&prefab_obj);
        assert_eq!(pptrs2.len(), 1);
        let pid = pptrs2[0]
            .as_object()
            .and_then(|m| m.get("m_PathID"))
            .and_then(extract_i64_field);
        assert_eq!(pid, Some(42));
    }
}
