//! Pure-Rust Binary FBX 7.4 (`Kaydara FBX Binary`) exporter for both single meshes
//! and complete rigged, textured, and animated scene hierarchies.

use crate::animation::TrackProperty;
use crate::scene_extract::{
    IDENTITY_MAT4, SceneDocument, SceneMesh, mat4_invert, quat_to_euler_xyz_deg,
};
use crate::{CodecError, CodecResult};
use devx_plugin_api::DecodedMesh;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::collections::HashMap;
use std::io::Write;

const FBX_MAGIC: &[u8; 23] = b"Kaydara FBX Binary  \x00\x1a\x00";
const FBX_VERSION: u32 = 7400;
const FBX_KTIME_ONE_SECOND: i64 = 46_186_158_000;

/// Exports a single [`DecodedMesh`] (geometry, normals, UVs, submeshes) to Binary FBX 7.4 bytes.
pub fn export_mesh_to_fbx(mesh: &DecodedMesh) -> CodecResult<Vec<u8>> {
    if mesh.positions.is_empty() || mesh.indices.is_empty() {
        return Err(CodecError::MalformedData(
            "cannot export empty mesh to FBX".to_owned(),
        ));
    }

    let scene = SceneDocument {
        name: mesh.name.clone(),
        roots: vec![0],
        nodes: vec![crate::scene_extract::SceneNode {
            name: mesh.name.clone(),
            unity_ref: None,
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
            name: mesh.name.clone(),
            positions: mesh.positions.iter().map(|p| [-p[0], p[1], p[2]]).collect(),
            normals: mesh
                .normals
                .iter()
                .map(|n| crate::scene_extract::normalize3([-n[0], n[1], n[2]]))
                .collect(),
            uvs: mesh.uvs.clone(),
            indices: {
                let mut flipped = Vec::with_capacity(mesh.indices.len());
                for tri in mesh.indices.as_chunks::<3>().0 {
                    flipped.push(tri[0]);
                    flipped.push(tri[2]);
                    flipped.push(tri[1]);
                }
                flipped
            },
            submeshes: mesh
                .submeshes
                .iter()
                .map(|s| crate::scene_extract::SceneSubMesh {
                    index_start: s.first_index as usize,
                    index_count: s.index_count as usize,
                    material_idx: None,
                })
                .collect(),
            joints: Vec::new(),
            weights: Vec::new(),
            bind_poses_row_major: Vec::new(),
            blend_shapes: Vec::new(),
        }],
        materials: Vec::new(),
        textures: Vec::new(),
        skins: Vec::new(),
        animations: Vec::new(),
        avatar_tos: HashMap::new(),
    };
    export_scene_to_fbx(&scene)
}

/// Exports a full extracted right-handed [`SceneDocument`] (hierarchy nodes, materials,
/// embedded PNG textures, skeleton skins, and animation stacks) to Binary FBX 7.4 bytes.
pub fn export_scene_to_fbx(scene: &SceneDocument) -> CodecResult<Vec<u8>> {
    if scene.nodes.is_empty() {
        return Err(CodecError::MalformedData(
            "cannot export empty scene to FBX".to_owned(),
        ));
    }

    let mut uid_alloc = UidGen(100_000_000);
    let doc_uid = uid_alloc.next();

    let mut objects = FbxRec::container("Objects");
    let mut conns = FbxRec::container("Connections");
    let mut takes = FbxRec::container("Takes");

    let mut counts = DefCounts::default();

    // 1. Videos & Textures
    let mut tex_uids = Vec::with_capacity(scene.textures.len());
    for tex in &scene.textures {
        let video_uid = uid_alloc.next();
        let tex_uid = uid_alloc.next();
        let rel_path = format!("textures/{}.png", tex.file_stem);

        objects.push(
            FbxRec::node("Video")
                .prop_i64(video_uid)
                .prop_str(&format!("{}\x00\x01Video", tex.name))
                .prop_str("Clip")
                .child(FbxRec::node("Type").prop_str("Clip"))
                .child(FbxRec::node("UseMipMap").prop_i32(0))
                .child(FbxRec::node("Filename").prop_str(&rel_path))
                .child(FbxRec::node("RelativeFilename").prop_str(&rel_path))
                .child(FbxRec::node("Content").prop_raw(&tex.png_bytes)),
        );

        let mut tex_props = FbxRec::container("Properties70");
        tex_props.push(fbx_prop_bool("UseMaterial", true));
        tex_props.push(fbx_prop_kstring("UVSet", "map1"));
        objects.push(
            FbxRec::node("Texture")
                .prop_i64(tex_uid)
                .prop_str(&format!("{}\x00\x01Texture", tex.name))
                .prop_str("")
                .child(FbxRec::node("Type").prop_str("TextureVideoClip"))
                .child(FbxRec::node("Version").prop_i32(202))
                .child(FbxRec::node("TextureName").prop_str(&format!("{}\x00\x01Texture", tex.name)))
                .child(tex_props)
                .child(FbxRec::node("Media").prop_str(&format!("{}\x00\x01Video", tex.name)))
                .child(FbxRec::node("FileName").prop_str(&rel_path))
                .child(FbxRec::node("RelativeFilename").prop_str(&rel_path)),
        );

        conns.push(fbx_conn_oo(video_uid, tex_uid));
        counts.video += 1;
        counts.texture += 1;
        tex_uids.push(tex_uid);
    }

    // 2. Materials
    let mut mat_uids = Vec::with_capacity(scene.materials.len());
    for mat in &scene.materials {
        let mat_uid = uid_alloc.next();
        let mut p70 = FbxRec::container("Properties70");
        p70.push(fbx_prop_color_rgb(
            "DiffuseColor",
            [mat.base_color[0] as f64, mat.base_color[1] as f64, mat.base_color[2] as f64],
        ));
        p70.push(fbx_prop_color_rgb(
            "EmissiveColor",
            [mat.emissive[0] as f64, mat.emissive[1] as f64, mat.emissive[2] as f64],
        ));
        p70.push(fbx_prop_f64("TransparencyFactor", (1.0 - mat.base_color[3]).clamp(0.0, 1.0) as f64));
        p70.push(fbx_prop_f64("Opacity", mat.base_color[3].clamp(0.0, 1.0) as f64));
        let shininess = ((1.0 - mat.roughness).clamp(0.0, 1.0) * 64.0) as f64;
        p70.push(fbx_prop_f64("ShininessExponent", shininess));

        objects.push(
            FbxRec::node("Material")
                .prop_i64(mat_uid)
                .prop_str(&format!("{}\x00\x01Material", mat.name))
                .prop_str("")
                .child(FbxRec::node("Version").prop_i32(102))
                .child(FbxRec::node("ShadingModel").prop_str("Phong"))
                .child(FbxRec::node("MultiLayer").prop_i32(0))
                .child(p70),
        );

        if let Some(ti) = mat.albedo_tex.and_then(|i| tex_uids.get(i).copied()) {
            conns.push(fbx_conn_op(ti, mat_uid, "DiffuseColor"));
        }
        if let Some(ti) = mat.normal_tex.and_then(|i| tex_uids.get(i).copied()) {
            conns.push(fbx_conn_op(ti, mat_uid, "NormalMap"));
        }
        if let Some(ti) = mat.emissive_tex.and_then(|i| tex_uids.get(i).copied()) {
            conns.push(fbx_conn_op(ti, mat_uid, "EmissiveColor"));
        }

        counts.material += 1;
        mat_uids.push(mat_uid);
    }

    // 3. Geometries (one per SceneMesh) + optional BlendShape deformers/channels/shapes
    let mut geo_uids = Vec::with_capacity(scene.meshes.len());
    for mesh in &scene.meshes {
        let geo_uid = uid_alloc.next();
        objects.push(build_fbx_geometry_node(geo_uid, mesh));
        counts.geometry += 1;
        geo_uids.push(geo_uid);

        if !mesh.blend_shapes.is_empty() {
            let bs_uid = uid_alloc.next();
            objects.push(
                FbxRec::node("Deformer")
                    .prop_i64(bs_uid)
                    .prop_str(&format!("{}_BlendShape\x00\x01Deformer", mesh.name))
                    .prop_str("BlendShape")
                    .child(FbxRec::node("Version").prop_i32(100)),
            );
            conns.push(fbx_conn_oo(bs_uid, geo_uid));
            counts.deformer += 1;

            for bs in &mesh.blend_shapes {
                if bs.vertex_indices.is_empty() {
                    continue;
                }
                let ch_uid = uid_alloc.next();
                let shape_uid = uid_alloc.next();
                let pct = (bs.default_weight.clamp(0.0, 1.0) * 100.0) as f64;
                let mut ch_p70 = FbxRec::container("Properties70");
                ch_p70.push(fbx_prop_f64("DeformPercent", pct));
                objects.push(
                    FbxRec::node("Deformer")
                        .prop_i64(ch_uid)
                        .prop_str(&format!("{}\x00\x01SubDeformer", bs.name))
                        .prop_str("BlendShapeChannel")
                        .child(FbxRec::node("Version").prop_i32(100))
                        .child(ch_p70)
                        .child(FbxRec::node("FullWeights").prop_f64_arr(&[100.0])),
                );
                conns.push(fbx_conn_oo(ch_uid, bs_uid));
                counts.deformer += 1;

                let mut idxs = Vec::with_capacity(bs.vertex_indices.len());
                let mut verts = Vec::with_capacity(bs.vertex_indices.len() * 3);
                let mut norms = Vec::with_capacity(bs.vertex_indices.len() * 3);
                for k in 0..bs.vertex_indices.len().min(bs.position_deltas.len()) {
                    idxs.push(bs.vertex_indices[k] as i32);
                    let dp = bs.position_deltas[k];
                    verts.extend_from_slice(&[dp[0] as f64, dp[1] as f64, dp[2] as f64]);
                    let dn = bs.normal_deltas.get(k).copied().unwrap_or([0.0; 3]);
                    norms.extend_from_slice(&[dn[0] as f64, dn[1] as f64, dn[2] as f64]);
                }
                objects.push(
                    FbxRec::node("Geometry")
                        .prop_i64(shape_uid)
                        .prop_str(&format!("{}\x00\x01Geometry", bs.name))
                        .prop_str("Shape")
                        .child(FbxRec::node("Version").prop_i32(100))
                        .child(FbxRec::node("Indexes").prop_i32_arr(&idxs))
                        .child(FbxRec::node("Vertices").prop_f64_arr(&verts))
                        .child(FbxRec::node("Normals").prop_f64_arr(&norms)),
                );
                conns.push(fbx_conn_oo(shape_uid, ch_uid));
                counts.geometry += 1;
            }
        }
    }

    // 4. Models + NodeAttributes
    let mut model_uids = Vec::with_capacity(scene.nodes.len());
    for node in &scene.nodes {
        let m_uid = uid_alloc.next();
        model_uids.push(m_uid);

        let subclass = if node.mesh_idx.is_some() {
            "Mesh"
        } else if node.is_bone {
            "LimbNode"
        } else {
            "Null"
        };

        let euler = quat_to_euler_xyz_deg(node.rotation);
        let mut p70 = FbxRec::container("Properties70");
        p70.push(fbx_prop_enum("RotationOrder", 0)); // XYZ
        p70.push(fbx_prop_bool("RotationActive", true));
        p70.push(fbx_prop_enum("InheritType", 1)); // RrSs
        p70.push(fbx_prop_vec3d(
            "Lcl Translation",
            "Lcl Translation",
            [node.translation[0] as f64, node.translation[1] as f64, node.translation[2] as f64],
        ));
        p70.push(fbx_prop_vec3d("Lcl Rotation", "Lcl Rotation", euler));
        p70.push(fbx_prop_vec3d(
            "Lcl Scaling",
            "Lcl Scaling",
            [node.scale[0] as f64, node.scale[1] as f64, node.scale[2] as f64],
        ));

        let model_rec = FbxRec::node("Model")
            .prop_i64(m_uid)
            .prop_str(&format!("{}\x00\x01Model", node.name))
            .prop_str(subclass)
            .child(FbxRec::node("Version").prop_i32(232))
            .child(p70)
            .child(FbxRec::node("Shading").prop_bool(true))
            .child(FbxRec::node("Culling").prop_str("CullingOff"));

        if subclass != "Mesh" {
            let attr_uid = uid_alloc.next();
            let mut attr = FbxRec::node("NodeAttribute")
                .prop_i64(attr_uid)
                .prop_str(&format!("{}\x00\x01NodeAttribute", node.name))
                .prop_str(subclass);
            if node.is_bone {
                attr = attr.child(FbxRec::node("TypeFlags").prop_str("Skeleton"));
            } else {
                attr = attr.child(FbxRec::node("TypeFlags").prop_str("Null"));
            }
            objects.push(attr);
            conns.push(fbx_conn_oo(attr_uid, m_uid));
            counts.node_attribute += 1;
        }

        objects.push(model_rec);
        counts.model += 1;

        if let Some(m_idx) = node.mesh_idx
            && let Some(&g_uid) = geo_uids.get(m_idx)
        {
            conns.push(fbx_conn_oo(g_uid, m_uid));
            // Attach distinct materials referenced by this mesh's submeshes in slot order.
            let mut attached_mats = Vec::new();
            for sub in &scene.meshes[m_idx].submeshes {
                if let Some(mat_idx) = sub.material_idx
                    && !attached_mats.contains(&mat_idx)
                    && let Some(&mat_uid) = mat_uids.get(mat_idx)
                {
                    attached_mats.push(mat_idx);
                    conns.push(fbx_conn_oo(mat_uid, m_uid));
                }
            }
        }
    }

    // Node parent/child Connections
    for (idx, node) in scene.nodes.iter().enumerate() {
        let child_uid = model_uids[idx];
        let parent_uid = node.parent.and_then(|p| model_uids.get(p).copied()).unwrap_or(0);
        conns.push(fbx_conn_oo(child_uid, parent_uid));
    }

    // 5. Skins & Bone Clusters
    for skin in &scene.skins {
        let Some(node) = scene.nodes.get(skin.mesh_node_idx) else {
            continue;
        };
        let Some(mesh_idx) = node.mesh_idx else {
            continue;
        };
        let Some(mesh) = scene.meshes.get(mesh_idx) else {
            continue;
        };
        let Some(&geo_uid) = geo_uids.get(mesh_idx) else {
            continue;
        };
        if mesh.joints.len() != mesh.positions.len() || mesh.weights.len() != mesh.positions.len() {
            continue;
        }

        let skin_uid = uid_alloc.next();
        objects.push(
            FbxRec::node("Deformer")
                .prop_i64(skin_uid)
                .prop_str(&format!("{}\x00\x01Deformer", skin.name))
                .prop_str("Skin")
                .child(FbxRec::node("Version").prop_i32(101))
                .child(FbxRec::node("Link_DeformAcuracy").prop_f64(50.0)),
        );
        conns.push(fbx_conn_oo(skin_uid, geo_uid));
        counts.deformer += 1;

        // Collect per-joint vertex index + weight lists.
        let joint_count = skin.joint_nodes.len();
        let mut per_joint_idx: Vec<Vec<i32>> = vec![Vec::new(); joint_count];
        let mut per_joint_w: Vec<Vec<f64>> = vec![Vec::new(); joint_count];

        for v in 0..mesh.positions.len() {
            let js = mesh.joints[v];
            let ws = mesh.weights[v];
            for k in 0..4 {
                let w = ws[k] as f64;
                let ji = js[k] as usize;
                if w > 1e-6 && ji < joint_count {
                    per_joint_idx[ji].push(v as i32);
                    per_joint_w[ji].push(w);
                }
            }
        }

        let id64: Vec<f64> = IDENTITY_MAT4.iter().map(|&f| f as f64).collect();
        for (j_slot, &bone_node_idx) in skin.joint_nodes.iter().enumerate() {
            let Some(&bone_uid) = model_uids.get(bone_node_idx) else {
                continue;
            };
            let cluster_uid = uid_alloc.next();
            let bone_name = scene
                .nodes
                .get(bone_node_idx)
                .map_or("Bone", |n| n.name.as_str());
            let ibm = skin
                .inverse_bind_matrices_col_major
                .get(j_slot)
                .copied()
                .unwrap_or(IDENTITY_MAT4);
            let tlink_mat = mat4_invert(&ibm).unwrap_or(IDENTITY_MAT4);
            let tlink64: Vec<f64> = tlink_mat.iter().map(|&f| f as f64).collect();

            let mut cluster = FbxRec::node("Deformer")
                .prop_i64(cluster_uid)
                .prop_str(&format!("{bone_name}_Cluster\x00\x01Deformer"))
                .prop_str("Cluster")
                .child(FbxRec::node("Version").prop_i32(100))
                .child(FbxRec::node("UserData").prop_str("").prop_str(""));

            if !per_joint_idx[j_slot].is_empty() {
                cluster = cluster
                    .child(FbxRec::node("Indexes").prop_i32_arr(&per_joint_idx[j_slot]))
                    .child(FbxRec::node("Weights").prop_f64_arr(&per_joint_w[j_slot]));
            }
            cluster = cluster
                .child(FbxRec::node("Transform").prop_f64_arr(&id64))
                .child(FbxRec::node("TransformLink").prop_f64_arr(&tlink64));

            objects.push(cluster);
            conns.push(fbx_conn_oo(cluster_uid, skin_uid));
            conns.push(fbx_conn_oo(bone_uid, cluster_uid));
            counts.deformer += 1;
        }
    }

    // 6. Animation Stacks / Layers / CurveNodes / Curves
    let mut current_take_name: Option<String> = None;
    for clip in &scene.animations {
        if clip.tracks.is_empty() {
            continue;
        }
        if current_take_name.is_none() {
            current_take_name = Some(clip.name.clone());
        }
        let stack_uid = uid_alloc.next();
        let layer_uid = uid_alloc.next();
        let stop_kt = (clip.duration_secs.max(1e-3) as f64 * FBX_KTIME_ONE_SECOND as f64) as i64;

        let mut stack_p70 = FbxRec::container("Properties70");
        stack_p70.push(fbx_prop_ktime("LocalStop", stop_kt));
        stack_p70.push(fbx_prop_ktime("ReferenceStop", stop_kt));

        objects.push(
            FbxRec::node("AnimationStack")
                .prop_i64(stack_uid)
                .prop_str(&format!("{}\x00\x01AnimStack", clip.name))
                .prop_str("")
                .child(stack_p70),
        );
        objects.push(
            FbxRec::node("AnimationLayer")
                .prop_i64(layer_uid)
                .prop_str("BaseLayer\x00\x01AnimLayer")
                .prop_str(""),
        );
        conns.push(fbx_conn_oo(layer_uid, stack_uid));
        counts.anim_stack += 1;
        counts.anim_layer += 1;

        takes.push(
            FbxRec::node("Take")
                .prop_str(&clip.name)
                .child(FbxRec::node("FileName").prop_str(&format!("{}.tak", clip.name)))
                .child(FbxRec::node("LocalTime").prop_i64(0).prop_i64(stop_kt))
                .child(FbxRec::node("ReferenceTime").prop_i64(0).prop_i64(stop_kt)),
        );

        for track in &clip.tracks {
            let Some(&target_model_uid) = model_uids.get(track.node_idx) else {
                continue;
            };
            if track.times.is_empty() {
                continue;
            }
            let (cn_name, prop_attr, defaults) = match track.property {
                TrackProperty::Translation => ("T", "Lcl Translation", [0.0f64, 0.0, 0.0]),
                TrackProperty::Rotation => ("R", "Lcl Rotation", [0.0f64, 0.0, 0.0]),
                TrackProperty::Scale => ("S", "Lcl Scaling", [1.0f64, 1.0, 1.0]),
            };

            let cn_uid = uid_alloc.next();
            let mut cn_p70 = FbxRec::container("Properties70");
            cn_p70.push(fbx_prop_number("d|X", defaults[0]));
            cn_p70.push(fbx_prop_number("d|Y", defaults[1]));
            cn_p70.push(fbx_prop_number("d|Z", defaults[2]));
            objects.push(
                FbxRec::node("AnimationCurveNode")
                    .prop_i64(cn_uid)
                    .prop_str(&format!("{cn_name}\x00\x01AnimCurveNode"))
                    .prop_str("")
                    .child(cn_p70),
            );
            conns.push(fbx_conn_oo(cn_uid, layer_uid));
            conns.push(fbx_conn_op(cn_uid, target_model_uid, prop_attr));
            counts.anim_curve_node += 1;

            let ktimes: Vec<i64> = track
                .times
                .iter()
                .map(|&t| (t.max(0.0) as f64 * FBX_KTIME_ONE_SECOND as f64) as i64)
                .collect();
            let mut axis_vals: [Vec<f32>; 3] = [
                Vec::with_capacity(track.times.len()),
                Vec::with_capacity(track.times.len()),
                Vec::with_capacity(track.times.len()),
            ];

            match track.property {
                TrackProperty::Translation | TrackProperty::Scale => {
                    for k in 0..track.times.len() {
                        axis_vals[0].push(track.values[k * 3]);
                        axis_vals[1].push(track.values[k * 3 + 1]);
                        axis_vals[2].push(track.values[k * 3 + 2]);
                    }
                }
                TrackProperty::Rotation => {
                    let mut prev_deg: Option<[f64; 3]> = None;
                    for k in 0..track.times.len() {
                        let q = [
                            track.values[k * 4],
                            track.values[k * 4 + 1],
                            track.values[k * 4 + 2],
                            track.values[k * 4 + 3],
                        ];
                        let mut deg = quat_to_euler_xyz_deg(q);
                        if let Some(prev) = prev_deg {
                            for axis in 0..3 {
                                let turns = ((prev[axis] - deg[axis]) / 360.0).round();
                                deg[axis] += turns * 360.0;
                            }
                        }
                        prev_deg = Some(deg);
                        axis_vals[0].push(deg[0] as f32);
                        axis_vals[1].push(deg[1] as f32);
                        axis_vals[2].push(deg[2] as f32);
                    }
                }
            }

            for (axis_idx, axis_ch) in ["d|X", "d|Y", "d|Z"].into_iter().enumerate() {
                let c_uid = uid_alloc.next();
                let key_count = ktimes.len();
                let attr_flags = vec![24836i32; key_count];
                let attr_data = vec![0.0f32; key_count * 4];
                let attr_ref_count = vec![key_count as i32];

                objects.push(
                    FbxRec::node("AnimationCurve")
                        .prop_i64(c_uid)
                        .prop_str("\x00\x01AnimCurve")
                        .prop_str("")
                        .child(FbxRec::node("Default").prop_f64(0.0))
                        .child(FbxRec::node("KeyVer").prop_i32(4008))
                        .child(FbxRec::node("KeyTime").prop_i64_arr(&ktimes))
                        .child(FbxRec::node("KeyValueFloat").prop_f32_arr(&axis_vals[axis_idx]))
                        .child(FbxRec::node("KeyAttrFlags").prop_i32_arr(&attr_flags))
                        .child(FbxRec::node("KeyAttrDataFloat").prop_f32_arr(&attr_data))
                        .child(FbxRec::node("KeyAttrRefCount").prop_i32_arr(&attr_ref_count)),
                );
                conns.push(fbx_conn_op(c_uid, cn_uid, axis_ch));
                counts.anim_curve += 1;
            }
        }
    }

    let mut takes_root = FbxRec::container("Takes");
    takes_root.push(FbxRec::node("Current").prop_str(current_take_name.as_deref().unwrap_or("")));
    for t in takes.children {
        takes_root.push(t);
    }

    let top_nodes = vec![
        build_fbx_header_extension(),
        build_global_settings(),
        build_documents(doc_uid, &scene.name),
        FbxRec::container("References"),
        build_definitions(&counts),
        objects,
        conns,
        takes_root,
    ];

    let mut out = Vec::with_capacity(4096);
    out.extend_from_slice(FBX_MAGIC);
    out.extend_from_slice(&FBX_VERSION.to_le_bytes());
    for rec in &top_nodes {
        write_fbx_record(&mut out, rec)?;
    }
    out.extend_from_slice(&[0u8; 13]);
    Ok(out)
}

fn build_fbx_geometry_node(geo_uid: i64, mesh: &SceneMesh) -> FbxRec {
    let mut flat_verts = Vec::with_capacity(mesh.positions.len() * 3);
    for p in &mesh.positions {
        flat_verts.push(p[0] as f64);
        flat_verts.push(p[1] as f64);
        flat_verts.push(p[2] as f64);
    }

    let mut poly_indices = Vec::with_capacity(mesh.indices.len());
    for tri in mesh.indices.as_chunks::<3>().0 {
        poly_indices.push(tri[0] as i32);
        poly_indices.push(tri[1] as i32);
        poly_indices.push(-(tri[2] as i32) - 1);
    }

    let mut rec = FbxRec::node("Geometry")
        .prop_i64(geo_uid)
        .prop_str(&format!("{}\x00\x01Geometry", mesh.name))
        .prop_str("Mesh")
        .child(FbxRec::node("GeometryVersion").prop_i32(124))
        .child(FbxRec::node("Vertices").prop_f64_arr(&flat_verts))
        .child(FbxRec::node("PolygonVertexIndex").prop_i32_arr(&poly_indices));

    let mut layer = FbxRec::node("Layer")
        .prop_i32(0)
        .child(FbxRec::node("Version").prop_i32(100));

    if mesh.normals.len() == mesh.positions.len() {
        let mut norms = Vec::with_capacity(mesh.normals.len() * 3);
        for n in &mesh.normals {
            norms.push(n[0] as f64);
            norms.push(n[1] as f64);
            norms.push(n[2] as f64);
        }
        rec = rec.child(
            FbxRec::node("LayerElementNormal")
                .prop_i32(0)
                .child(FbxRec::node("Version").prop_i32(101))
                .child(FbxRec::node("Name").prop_str(""))
                .child(FbxRec::node("MappingInformationType").prop_str("ByVertice"))
                .child(FbxRec::node("ReferenceInformationType").prop_str("Direct"))
                .child(FbxRec::node("Normals").prop_f64_arr(&norms)),
        );
        layer = layer.child(
            FbxRec::node("LayerElement")
                .child(FbxRec::node("Type").prop_str("LayerElementNormal"))
                .child(FbxRec::node("TypedIndex").prop_i32(0)),
        );
    }

    if mesh.uvs.len() == mesh.positions.len() {
        let mut flat_uvs = Vec::with_capacity(mesh.uvs.len() * 2);
        for uv in &mesh.uvs {
            flat_uvs.push(uv[0] as f64);
            flat_uvs.push(uv[1] as f64);
        }
        let tri_total = (mesh.indices.len() / 3) * 3;
        let uv_indices: Vec<i32> = mesh.indices[..tri_total].iter().map(|&i| i as i32).collect();

        rec = rec.child(
            FbxRec::node("LayerElementUV")
                .prop_i32(0)
                .child(FbxRec::node("Version").prop_i32(101))
                .child(FbxRec::node("Name").prop_str("map1"))
                .child(FbxRec::node("MappingInformationType").prop_str("ByPolygonVertex"))
                .child(FbxRec::node("ReferenceInformationType").prop_str("IndexToDirect"))
                .child(FbxRec::node("UV").prop_f64_arr(&flat_uvs))
                .child(FbxRec::node("UVIndex").prop_i32_arr(&uv_indices)),
        );
        layer = layer.child(
            FbxRec::node("LayerElement")
                .child(FbxRec::node("Type").prop_str("LayerElementUV"))
                .child(FbxRec::node("TypedIndex").prop_i32(0)),
        );
    }

    if !mesh.submeshes.is_empty() {
        let mut unique_mats: Vec<usize> = Vec::new();
        for s in &mesh.submeshes {
            if let Some(mi) = s.material_idx
                && !unique_mats.contains(&mi)
            {
                unique_mats.push(mi);
            }
        }
        let tri_total = mesh.indices.len() / 3;
        let mut mat_per_poly = vec![0i32; tri_total];
        for s in &mesh.submeshes {
            let slot = s
                .material_idx
                .and_then(|mi| unique_mats.iter().position(|&m| m == mi))
                .unwrap_or(0) as i32;
            let first_poly = s.index_start / 3;
            let count_poly = s.index_count / 3;
            let end = (first_poly + count_poly).min(tri_total);
            for slot_entry in &mut mat_per_poly[first_poly..end] {
                *slot_entry = slot;
            }
        }
        rec = rec.child(
            FbxRec::node("LayerElementMaterial")
                .prop_i32(0)
                .child(FbxRec::node("Version").prop_i32(101))
                .child(FbxRec::node("Name").prop_str(""))
                .child(FbxRec::node("MappingInformationType").prop_str("ByPolygon"))
                .child(FbxRec::node("ReferenceInformationType").prop_str("IndexToDirect"))
                .child(FbxRec::node("Materials").prop_i32_arr(&mat_per_poly)),
        );
        layer = layer.child(
            FbxRec::node("LayerElement")
                .child(FbxRec::node("Type").prop_str("LayerElementMaterial"))
                .child(FbxRec::node("TypedIndex").prop_i32(0)),
        );
    }

    rec.child(layer)
}

#[derive(Default)]
struct DefCounts {
    model: i32,
    geometry: i32,
    material: i32,
    texture: i32,
    video: i32,
    node_attribute: i32,
    deformer: i32,
    anim_stack: i32,
    anim_layer: i32,
    anim_curve_node: i32,
    anim_curve: i32,
}

fn build_definitions(c: &DefCounts) -> FbxRec {
    let mut defs = FbxRec::node("Definitions")
        .child(FbxRec::node("Version").prop_i32(100));

    let total = 1
        + c.model
        + c.geometry
        + c.material
        + c.texture
        + c.video
        + c.node_attribute
        + c.deformer
        + c.anim_stack
        + c.anim_layer
        + c.anim_curve_node
        + c.anim_curve;
    defs = defs.child(FbxRec::node("Count").prop_i32(total));
    defs = defs.child(
        FbxRec::node("ObjectType")
            .prop_str("GlobalSettings")
            .child(FbxRec::node("Count").prop_i32(1)),
    );

    let pairs = [
        ("Model", c.model),
        ("Geometry", c.geometry),
        ("Material", c.material),
        ("Texture", c.texture),
        ("Video", c.video),
        ("NodeAttribute", c.node_attribute),
        ("Deformer", c.deformer),
        ("AnimationStack", c.anim_stack),
        ("AnimationLayer", c.anim_layer),
        ("AnimationCurveNode", c.anim_curve_node),
        ("AnimationCurve", c.anim_curve),
    ];
    for (ty, n) in pairs {
        if n > 0 {
            defs = defs.child(
                FbxRec::node("ObjectType")
                    .prop_str(ty)
                    .child(FbxRec::node("Count").prop_i32(n)),
            );
        }
    }
    defs
}

fn build_fbx_header_extension() -> FbxRec {
    FbxRec::node("FBXHeaderExtension")
        .child(FbxRec::node("FBXHeaderVersion").prop_i32(1003))
        .child(FbxRec::node("FBXVersion").prop_i32(FBX_VERSION as i32))
        .child(FbxRec::node("EncryptionType").prop_i32(0))
        .child(
            FbxRec::node("CreationTimeStamp")
                .child(FbxRec::node("Version").prop_i32(1000))
                .child(FbxRec::node("Year").prop_i32(2026))
                .child(FbxRec::node("Month").prop_i32(1))
                .child(FbxRec::node("Day").prop_i32(1))
                .child(FbxRec::node("Hour").prop_i32(0))
                .child(FbxRec::node("Minute").prop_i32(0))
                .child(FbxRec::node("Second").prop_i32(0))
                .child(FbxRec::node("Millisecond").prop_i32(0)),
        )
        .child(FbxRec::node("Creator").prop_str("devx-codecs (DevX Unity Rust)"))
}

fn build_global_settings() -> FbxRec {
    let mut p70 = FbxRec::container("Properties70");
    for (k, v) in [
        ("UpAxis", 1),
        ("UpAxisSign", 1),
        ("FrontAxis", 2),
        ("FrontAxisSign", 1),
        ("CoordAxis", 0),
        ("CoordAxisSign", 1),
        ("OriginalUpAxis", 1),
        ("OriginalUpAxisSign", 1),
    ] {
        p70.push(FbxRec::node("P").prop_str(k).prop_str("int").prop_str("Integer").prop_str("").prop_i32(v));
    }
    p70.push(fbx_prop_f64("UnitScaleFactor", 100.0));
    p70.push(fbx_prop_f64("OriginalUnitScaleFactor", 100.0));
    FbxRec::node("GlobalSettings")
        .child(FbxRec::node("Version").prop_i32(1000))
        .child(p70)
}

fn build_documents(doc_uid: i64, name: &str) -> FbxRec {
    let mut p70 = FbxRec::container("Properties70");
    p70.push(
        FbxRec::node("P")
            .prop_str("SourceObject")
            .prop_str("object")
            .prop_str("")
            .prop_str(""),
    );
    p70.push(
        FbxRec::node("P")
            .prop_str("ActiveAnimStackName")
            .prop_str("KString")
            .prop_str("")
            .prop_str("")
            .prop_str(""),
    );
    FbxRec::node("Documents")
        .child(FbxRec::node("Count").prop_i32(1))
        .child(
            FbxRec::node("Document")
                .prop_i64(doc_uid)
                .prop_str(name)
                .prop_str("Scene")
                .child(p70)
                .child(FbxRec::node("RootNode").prop_i64(0)),
        )
}

struct UidGen(i64);
impl UidGen {
    fn next(&mut self) -> i64 {
        let v = self.0;
        self.0 += 1;
        v
    }
}

fn fbx_conn_oo(src: i64, dst: i64) -> FbxRec {
    FbxRec::node("C").prop_str("OO").prop_i64(src).prop_i64(dst)
}

fn fbx_conn_op(src: i64, dst: i64, prop: &str) -> FbxRec {
    FbxRec::node("C")
        .prop_str("OP")
        .prop_i64(src)
        .prop_i64(dst)
        .prop_str(prop)
}

fn fbx_prop_bool(name: &str, v: bool) -> FbxRec {
    FbxRec::node("P")
        .prop_str(name)
        .prop_str("bool")
        .prop_str("")
        .prop_str("")
        .prop_i32(i32::from(v))
}

fn fbx_prop_kstring(name: &str, v: &str) -> FbxRec {
    FbxRec::node("P")
        .prop_str(name)
        .prop_str("KString")
        .prop_str("")
        .prop_str("")
        .prop_str(v)
}

fn fbx_prop_enum(name: &str, v: i32) -> FbxRec {
    FbxRec::node("P")
        .prop_str(name)
        .prop_str("enum")
        .prop_str("")
        .prop_str("")
        .prop_i32(v)
}

fn fbx_prop_f64(name: &str, v: f64) -> FbxRec {
    FbxRec::node("P")
        .prop_str(name)
        .prop_str("double")
        .prop_str("Number")
        .prop_str("")
        .prop_f64(v)
}

fn fbx_prop_number(name: &str, v: f64) -> FbxRec {
    FbxRec::node("P")
        .prop_str(name)
        .prop_str("Number")
        .prop_str("")
        .prop_str("A")
        .prop_f64(v)
}

fn fbx_prop_ktime(name: &str, kt: i64) -> FbxRec {
    FbxRec::node("P")
        .prop_str(name)
        .prop_str("KTime")
        .prop_str("Time")
        .prop_str("")
        .prop_i64(kt)
}

fn fbx_prop_color_rgb(name: &str, rgb: [f64; 3]) -> FbxRec {
    FbxRec::node("P")
        .prop_str(name)
        .prop_str("Color")
        .prop_str("")
        .prop_str("A")
        .prop_f64(rgb[0])
        .prop_f64(rgb[1])
        .prop_f64(rgb[2])
}

fn fbx_prop_vec3d(name: &str, ty: &str, v: [f64; 3]) -> FbxRec {
    FbxRec::node("P")
        .prop_str(name)
        .prop_str(ty)
        .prop_str("")
        .prop_str("A")
        .prop_f64(v[0])
        .prop_f64(v[1])
        .prop_f64(v[2])
}

enum FbxProp {
    Bool(bool),
    I32(i32),
    F64(f64),
    I64(i64),
    Str(Vec<u8>),
    Raw(Vec<u8>),
    I32Arr(Vec<i32>),
    I64Arr(Vec<i64>),
    F32Arr(Vec<f32>),
    F64Arr(Vec<f64>),
}

struct FbxRec {
    name: String,
    props: Vec<FbxProp>,
    children: Vec<FbxRec>,
    has_block_list: bool,
}

impl FbxRec {
    fn node(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            props: Vec::new(),
            children: Vec::new(),
            has_block_list: false,
        }
    }

    fn container(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            props: Vec::new(),
            children: Vec::new(),
            has_block_list: true,
        }
    }

    fn push(&mut self, child: FbxRec) {
        self.has_block_list = true;
        self.children.push(child);
    }

    fn child(mut self, child: FbxRec) -> Self {
        self.has_block_list = true;
        self.children.push(child);
        self
    }

    fn prop_bool(mut self, v: bool) -> Self {
        self.props.push(FbxProp::Bool(v));
        self
    }
    fn prop_i32(mut self, v: i32) -> Self {
        self.props.push(FbxProp::I32(v));
        self
    }
    fn prop_f64(mut self, v: f64) -> Self {
        self.props.push(FbxProp::F64(v));
        self
    }
    fn prop_i64(mut self, v: i64) -> Self {
        self.props.push(FbxProp::I64(v));
        self
    }
    fn prop_str(mut self, s: &str) -> Self {
        self.props.push(FbxProp::Str(s.as_bytes().to_vec()));
        self
    }
    fn prop_raw(mut self, bytes: &[u8]) -> Self {
        self.props.push(FbxProp::Raw(bytes.to_vec()));
        self
    }
    fn prop_i32_arr(mut self, arr: &[i32]) -> Self {
        self.props.push(FbxProp::I32Arr(arr.to_vec()));
        self
    }
    fn prop_i64_arr(mut self, arr: &[i64]) -> Self {
        self.props.push(FbxProp::I64Arr(arr.to_vec()));
        self
    }
    fn prop_f32_arr(mut self, arr: &[f32]) -> Self {
        self.props.push(FbxProp::F32Arr(arr.to_vec()));
        self
    }
    fn prop_f64_arr(mut self, arr: &[f64]) -> Self {
        self.props.push(FbxProp::F64Arr(arr.to_vec()));
        self
    }
}

fn write_fbx_record(out: &mut Vec<u8>, rec: &FbxRec) -> CodecResult<()> {
    let header_pos = out.len();
    out.extend_from_slice(&0u32.to_le_bytes()); // end_offset placeholder
    out.extend_from_slice(&(rec.props.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // property_list_len placeholder
    let name_bytes = rec.name.as_bytes();
    if name_bytes.len() > 255 {
        return Err(CodecError::MalformedData(format!(
            "FBX record name too long: {}",
            rec.name
        )));
    }
    out.push(name_bytes.len() as u8);
    out.extend_from_slice(name_bytes);

    let props_start = out.len();
    for p in &rec.props {
        write_fbx_prop(out, p)?;
    }
    let prop_len = (out.len() - props_start) as u32;

    if rec.has_block_list || !rec.children.is_empty() {
        for c in &rec.children {
            write_fbx_record(out, c)?;
        }
        out.extend_from_slice(&[0u8; 13]);
    }

    let end_offset = u32::try_from(out.len())
        .map_err(|_| CodecError::MalformedData("FBX binary offset exceeded 4 GiB".to_owned()))?;
    out[header_pos..header_pos + 4].copy_from_slice(&end_offset.to_le_bytes());
    out[header_pos + 8..header_pos + 12].copy_from_slice(&prop_len.to_le_bytes());
    Ok(())
}

fn write_fbx_prop(out: &mut Vec<u8>, prop: &FbxProp) -> CodecResult<()> {
    match prop {
        FbxProp::Bool(v) => {
            out.push(b'C');
            out.push(u8::from(*v));
        }
        FbxProp::I32(v) => {
            out.push(b'I');
            out.extend_from_slice(&v.to_le_bytes());
        }
        FbxProp::F64(v) => {
            out.push(b'D');
            out.extend_from_slice(&v.to_le_bytes());
        }
        FbxProp::I64(v) => {
            out.push(b'L');
            out.extend_from_slice(&v.to_le_bytes());
        }
        FbxProp::Str(bytes) => {
            out.push(b'S');
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        FbxProp::Raw(bytes) => {
            out.push(b'R');
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        FbxProp::I32Arr(arr) => {
            let mut raw = Vec::with_capacity(arr.len() * 4);
            for v in arr {
                raw.extend_from_slice(&v.to_le_bytes());
            }
            write_fbx_array(out, b'i', arr.len(), &raw)?;
        }
        FbxProp::I64Arr(arr) => {
            let mut raw = Vec::with_capacity(arr.len() * 8);
            for v in arr {
                raw.extend_from_slice(&v.to_le_bytes());
            }
            write_fbx_array(out, b'l', arr.len(), &raw)?;
        }
        FbxProp::F32Arr(arr) => {
            let mut raw = Vec::with_capacity(arr.len() * 4);
            for v in arr {
                raw.extend_from_slice(&v.to_le_bytes());
            }
            write_fbx_array(out, b'f', arr.len(), &raw)?;
        }
        FbxProp::F64Arr(arr) => {
            let mut raw = Vec::with_capacity(arr.len() * 8);
            for v in arr {
                raw.extend_from_slice(&v.to_le_bytes());
            }
            write_fbx_array(out, b'd', arr.len(), &raw)?;
        }
    }
    Ok(())
}

fn write_fbx_array(out: &mut Vec<u8>, tag: u8, count: usize, raw: &[u8]) -> CodecResult<()> {
    out.push(tag);
    out.extend_from_slice(&(count as u32).to_le_bytes());
    if raw.len() > 128 {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(raw)?;
        let compressed = enc.finish()?;
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&compressed);
    } else {
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        out.extend_from_slice(raw);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_extract::{SceneAnimClip, SceneAnimTrack, SceneNode, SceneSkin, SceneSubMesh};

    fn walk_fbx_records(bytes: &[u8], mut pos: usize, limit: usize, names: &mut Vec<String>) -> usize {
        while pos + 13 <= limit {
            let end_offset = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
            let num_props = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap());
            let prop_len = u32::from_le_bytes(bytes[pos + 8..pos + 12].try_into().unwrap()) as usize;
            let name_len = bytes[pos + 12] as usize;
            if end_offset == 0 && num_props == 0 && prop_len == 0 && name_len == 0 {
                return pos + 13;
            }
            let name = String::from_utf8_lossy(&bytes[pos + 13..pos + 13 + name_len]).into_owned();
            names.push(name);
            let child_start = pos + 13 + name_len + prop_len;
            assert!(end_offset <= limit, "record overflows limit: {end_offset} > {limit}");
            if child_start < end_offset {
                let end = walk_fbx_records(bytes, child_start, end_offset, names);
                assert_eq!(end, end_offset);
            }
            pos = end_offset;
        }
        pos
    }

    #[test]
    fn exports_valid_binary_fbx_record_chain_with_skin_and_animation() {
        let mut scene = SceneDocument {
            name: "HeroRig".into(),
            roots: vec![0],
            nodes: vec![
                SceneNode {
                    name: "Hero".into(),
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
                    name: "BodyMesh".into(),
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
                    name: "Spine".into(),
                    unity_ref: None,
                    parent: Some(0),
                    children: vec![],
                    translation: [0.0, 1.0, 0.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                    scale: [1.0, 1.0, 1.0],
                    mesh_idx: None,
                    skin_idx: None,
                    is_bone: true,
                },
            ],
            meshes: vec![SceneMesh {
                name: "Body".into(),
                positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                normals: vec![[0.0, 0.0, 1.0]; 3],
                uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                indices: vec![0, 1, 2],
                submeshes: vec![SceneSubMesh {
                    index_start: 0,
                    index_count: 3,
                    material_idx: None,
                }],
                joints: vec![[0, 0, 0, 0]; 3],
                weights: vec![[1.0, 0.0, 0.0, 0.0]; 3],
                bind_poses_row_major: vec![],
                blend_shapes: vec![crate::mesh::DecodedBlendShape {
                    name: "BrowUp".into(),
                    vertex_indices: vec![2],
                    position_deltas: vec![[0.0, 0.25, 0.0]],
                    normal_deltas: vec![[0.0, 0.0, 0.0]],
                    has_normals: true,
                    default_weight: 0.0,
                }],
            }],
            materials: vec![],
            textures: vec![],
            skins: vec![SceneSkin {
                name: "BodySkin".into(),
                mesh_node_idx: 1,
                root_bone_idx: Some(2),
                joint_nodes: vec![2],
                inverse_bind_matrices_col_major: vec![IDENTITY_MAT4],
            }],
            animations: vec![SceneAnimClip {
                name: "Idle".into(),
                duration_secs: 0.5,
                sample_rate: 30.0,
                tracks: vec![SceneAnimTrack {
                    node_idx: 2,
                    property: TrackProperty::Rotation,
                    times: vec![0.0, 0.5],
                    values: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.3826834, 0.0, 0.9238795],
                }],
            }],
            avatar_tos: HashMap::new(),
        };
        let bytes = export_scene_to_fbx(&scene).expect("fbx export");
        assert!(bytes.starts_with(FBX_MAGIC));
        let ver = u32::from_le_bytes(bytes[23..27].try_into().unwrap());
        assert_eq!(ver, 7400);

        let mut names = Vec::new();
        let end = walk_fbx_records(&bytes, 27, bytes.len(), &mut names);
        assert_eq!(end, bytes.len());
        for expected in [
            "FBXHeaderExtension",
            "GlobalSettings",
            "Definitions",
            "Objects",
            "Geometry",
            "Model",
            "Deformer",
            "AnimationStack",
            "AnimationLayer",
            "AnimationCurveNode",
            "AnimationCurve",
            "Connections",
            "Takes",
        ] {
            assert!(names.iter().any(|n| n == expected), "missing FBX node {expected}");
        }
        let _ = &mut scene;
    }
}
