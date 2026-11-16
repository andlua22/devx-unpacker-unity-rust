//! Clean-room COLLADA 1.4.1 (`.dae`) exporter supporting both single meshes and
//! complete rigged, textured, and keyframe-animated Unity scenes.

use crate::animation::TrackProperty;
use crate::scene_extract::{
    IDENTITY_MAT4, SceneDocument, SceneMesh, mat4_from_trs, normalize3,
};
use crate::{CodecError, CodecResult};
use devx_plugin_api::DecodedMesh;
use std::collections::HashMap;
use std::fmt::Write;

/// Exports a single [`DecodedMesh`] as a self-contained COLLADA 1.4.1 XML document.
pub fn export_mesh_to_dae(mesh: &DecodedMesh) -> CodecResult<String> {
    if mesh.positions.is_empty() || mesh.indices.is_empty() {
        return Err(CodecError::MalformedData(
            "cannot export empty mesh to COLLADA".to_owned(),
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
                .map(|n| normalize3([-n[0], n[1], n[2]]))
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
    let (dae_xml, _) = export_scene_to_dae(&scene, "textures")?;
    Ok(dae_xml)
}

/// Relative texture path inside a COLLADA export bundle and its encoded PNG bytes.
pub type ColladaTextureEntry = (String, Vec<u8>);

/// Exports a complete right-handed [`SceneDocument`] to COLLADA 1.4.1 XML plus
/// relative PNG texture files (`("<tex_dir>/<file_stem>.png", png_bytes)`).
pub fn export_scene_to_dae(
    scene: &SceneDocument,
    tex_dir: &str,
) -> CodecResult<(String, Vec<ColladaTextureEntry>)> {
    if scene.nodes.is_empty() {
        return Err(CodecError::MalformedData(
            "cannot export empty scene to COLLADA".to_owned(),
        ));
    }

    let mut out = String::with_capacity(16 * 1024);
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<COLLADA xmlns=\"http://www.collada.org/2005/11/COLLADASchema\" version=\"1.4.1\">\n");
    out.push_str("  <asset>\n");
    out.push_str("    <contributor><authoring_tool>devx-codecs (DevX Unity Rust)</authoring_tool></contributor>\n");
    out.push_str("    <unit name=\"meter\" meter=\"1\"/>\n");
    out.push_str("    <up_axis>Y_UP</up_axis>\n");
    out.push_str("  </asset>\n");

    let mut tex_files = Vec::with_capacity(scene.textures.len());
    if !scene.textures.is_empty() {
        out.push_str("  <library_images>\n");
        for (ti, tex) in scene.textures.iter().enumerate() {
            let rel = if tex_dir.is_empty() {
                format!("{}.png", tex.file_stem)
            } else {
                format!("{}/{}.png", tex_dir.trim_end_matches('/'), tex.file_stem)
            };
            let _ = writeln!(
                out,
                "    <image id=\"img_{ti}\" name=\"{}\"><init_from>{}</init_from></image>",
                escape_xml(&tex.name),
                escape_xml(&rel)
            );
            tex_files.push((rel, tex.png_bytes.as_ref().clone()));
        }
        out.push_str("  </library_images>\n");
    }

    if !scene.materials.is_empty() {
        out.push_str("  <library_effects>\n");
        for (mi, mat) in scene.materials.iter().enumerate() {
            let _ = writeln!(out, "    <effect id=\"eff_{mi}\">");
            out.push_str("      <profile_COMMON>\n");
            if let Some(ti) = mat.albedo_tex {
                let _ = writeln!(
                    out,
                    "        <newparam sid=\"surf_{ti}\"><surface type=\"2D\"><init_from>img_{ti}</init_from></surface></newparam>"
                );
                let _ = writeln!(
                    out,
                    "        <newparam sid=\"samp_{ti}\"><sampler2D><source>surf_{ti}</source></sampler2D></newparam>"
                );
            }
            out.push_str("        <technique sid=\"common\"><phong>\n");
            let _ = writeln!(
                out,
                "          <emission><color>{:.4} {:.4} {:.4} 1.0</color></emission>",
                mat.emissive[0], mat.emissive[1], mat.emissive[2]
            );
            if let Some(ti) = mat.albedo_tex {
                let _ = writeln!(
                    out,
                    "          <diffuse><texture texture=\"samp_{ti}\" texcoord=\"UVMap\"/></diffuse>"
                );
            } else {
                let _ = writeln!(
                    out,
                    "          <diffuse><color>{:.4} {:.4} {:.4} {:.4}</color></diffuse>",
                    mat.base_color[0], mat.base_color[1], mat.base_color[2], mat.base_color[3]
                );
            }
            let spec = (1.0 - mat.roughness).clamp(0.05, 0.8);
            let _ = writeln!(
                out,
                "          <specular><color>{spec:.4} {spec:.4} {spec:.4} 1.0</color></specular>"
            );
            out.push_str("          <shininess><float>24.0</float></shininess>\n");
            out.push_str("        </phong></technique>\n");
            out.push_str("      </profile_COMMON>\n");
            out.push_str("    </effect>\n");
        }
        out.push_str("  </library_effects>\n");

        out.push_str("  <library_materials>\n");
        for (mi, mat) in scene.materials.iter().enumerate() {
            let _ = writeln!(
                out,
                "    <material id=\"mat_{mi}\" name=\"{}\"><instance_effect url=\"#eff_{mi}\"/></material>",
                escape_xml(&mat.name)
            );
        }
        out.push_str("  </library_materials>\n");
    }

    // Geometries
    out.push_str("  <library_geometries>\n");
    for (gi, mesh) in scene.meshes.iter().enumerate() {
        write_dae_geometry(&mut out, gi, mesh, scene.materials.len());
    }
    out.push_str("  </library_geometries>\n");

    // Skin Controllers
    if !scene.skins.is_empty() {
        out.push_str("  <library_controllers>\n");
        for (si, skin) in scene.skins.iter().enumerate() {
            let Some(mesh_idx) = scene.nodes.get(skin.mesh_node_idx).and_then(|n| n.mesh_idx) else {
                continue;
            };
            let Some(mesh) = scene.meshes.get(mesh_idx) else {
                continue;
            };
            write_dae_skin(&mut out, si, mesh_idx, mesh, skin, &scene.nodes);
        }
        out.push_str("  </library_controllers>\n");
    }

    // Animations
    if !scene.animations.is_empty() {
        out.push_str("  <library_animations>\n");
        for (ci, clip) in scene.animations.iter().enumerate() {
            for (ti, track) in clip.tracks.iter().enumerate() {
                if track.times.is_empty() || track.node_idx >= scene.nodes.len() {
                    continue;
                }
                write_dae_anim_track(&mut out, ci, ti, &scene.nodes[track.node_idx], track);
            }
        }
        out.push_str("  </library_animations>\n");
    }

    // Visual Scene
    out.push_str("  <library_visual_scenes>\n");
    let _ = writeln!(out, "    <visual_scene id=\"Scene\" name=\"{}\">", escape_xml(&scene.name));
    let mut visited = vec![false; scene.nodes.len()];
    for &r_idx in &scene.roots {
        write_dae_node(&mut out, scene, r_idx, 3, &mut visited);
    }
    out.push_str("    </visual_scene>\n");
    out.push_str("  </library_visual_scenes>\n");
    out.push_str("  <scene><instance_visual_scene url=\"#Scene\"/></scene>\n");
    out.push_str("</COLLADA>\n");

    Ok((out, tex_files))
}

fn write_dae_geometry(out: &mut String, gi: usize, mesh: &SceneMesh, mat_count: usize) {
    let _ = writeln!(
        out,
        "    <geometry id=\"mesh_{gi}\" name=\"{}\">",
        escape_xml(&mesh.name)
    );
    out.push_str("      <mesh>\n");

    // Positions
    let _ = writeln!(out, "        <source id=\"mesh_{gi}-positions\">");
    let _ = write!(
        out,
        "          <float_array id=\"mesh_{gi}-positions-array\" count=\"{}\">",
        mesh.positions.len() * 3
    );
    for (i, p) in mesh.positions.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{:.6} {:.6} {:.6}", p[0], p[1], p[2]);
    }
    out.push_str("</float_array>\n");
    let _ = writeln!(
        out,
        "          <technique_common><accessor source=\"#mesh_{gi}-positions-array\" count=\"{}\" stride=\"3\"><param name=\"X\" type=\"float\"/><param name=\"Y\" type=\"float\"/><param name=\"Z\" type=\"float\"/></accessor></technique_common>",
        mesh.positions.len()
    );
    out.push_str("        </source>\n");

    // Normals
    let has_norm = mesh.normals.len() == mesh.positions.len();
    if has_norm {
        let _ = writeln!(out, "        <source id=\"mesh_{gi}-normals\">");
        let _ = write!(
            out,
            "          <float_array id=\"mesh_{gi}-normals-array\" count=\"{}\">",
            mesh.normals.len() * 3
        );
        for (i, n) in mesh.normals.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            let _ = write!(out, "{:.6} {:.6} {:.6}", n[0], n[1], n[2]);
        }
        out.push_str("</float_array>\n");
        let _ = writeln!(
            out,
            "          <technique_common><accessor source=\"#mesh_{gi}-normals-array\" count=\"{}\" stride=\"3\"><param name=\"X\" type=\"float\"/><param name=\"Y\" type=\"float\"/><param name=\"Z\" type=\"float\"/></accessor></technique_common>",
            mesh.normals.len()
        );
        out.push_str("        </source>\n");
    }

    // UVs
    let has_uv = mesh.uvs.len() == mesh.positions.len();
    if has_uv {
        let _ = writeln!(out, "        <source id=\"mesh_{gi}-uvs\">");
        let _ = write!(
            out,
            "          <float_array id=\"mesh_{gi}-uvs-array\" count=\"{}\">",
            mesh.uvs.len() * 2
        );
        for (i, uv) in mesh.uvs.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            let _ = write!(out, "{:.6} {:.6}", uv[0], uv[1]);
        }
        out.push_str("</float_array>\n");
        let _ = writeln!(
            out,
            "          <technique_common><accessor source=\"#mesh_{gi}-uvs-array\" count=\"{}\" stride=\"2\"><param name=\"S\" type=\"float\"/><param name=\"T\" type=\"float\"/></accessor></technique_common>",
            mesh.uvs.len()
        );
        out.push_str("        </source>\n");
    }

    let _ = writeln!(
        out,
        "        <vertices id=\"mesh_{gi}-vertices\"><input semantic=\"POSITION\" source=\"#mesh_{gi}-positions\"/></vertices>"
    );

    let submeshes: Vec<crate::scene_extract::SceneSubMesh> = if mesh.submeshes.is_empty() {
        vec![crate::scene_extract::SceneSubMesh {
            index_start: 0,
            index_count: mesh.indices.len(),
            material_idx: None,
        }]
    } else {
        mesh.submeshes.clone()
    };

    for sub in submeshes {
        let start = sub.index_start;
        let end = (start + sub.index_count).min(mesh.indices.len());
        let tri_count = (end.saturating_sub(start)) / 3;
        if tri_count == 0 {
            continue;
        }
        let mat_attr = sub
            .material_idx
            .filter(|&mi| mi < mat_count)
            .map_or_else(String::new, |mi| format!(" material=\"mat_{mi}\""));
        let _ = writeln!(out, "        <triangles{mat_attr} count=\"{tri_count}\">");
        let _ = writeln!(
            out,
            "          <input semantic=\"VERTEX\" source=\"#mesh_{gi}-vertices\" offset=\"0\"/>"
        );
        if has_norm {
            let _ = writeln!(
                out,
                "          <input semantic=\"NORMAL\" source=\"#mesh_{gi}-normals\" offset=\"0\"/>"
            );
        }
        if has_uv {
            let _ = writeln!(
                out,
                "          <input semantic=\"TEXCOORD\" source=\"#mesh_{gi}-uvs\" offset=\"0\" set=\"0\"/>"
            );
        }
        out.push_str("          <p>");
        for (i, idx) in mesh.indices[start..start + tri_count * 3].iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            let _ = write!(out, "{idx}");
        }
        out.push_str("</p>\n        </triangles>\n");
    }

    out.push_str("      </mesh>\n    </geometry>\n");
}

fn write_dae_skin(
    out: &mut String,
    si: usize,
    mesh_idx: usize,
    mesh: &SceneMesh,
    skin: &crate::scene_extract::SceneSkin,
    nodes: &[crate::scene_extract::SceneNode],
) {
    let j_count = skin.joint_nodes.len();
    let _ = writeln!(
        out,
        "    <controller id=\"skin_{si}\" name=\"{}\">",
        escape_xml(&skin.name)
    );
    let _ = writeln!(out, "      <skin source=\"#mesh_{mesh_idx}\">");
    out.push_str("        <bind_shape_matrix>1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 1</bind_shape_matrix>\n");

    // Joint names (sid matches node sid="node_<idx>")
    let _ = writeln!(out, "        <source id=\"skin_{si}-joints\">");
    let _ = write!(
        out,
        "          <Name_array id=\"skin_{si}-joints-array\" count=\"{j_count}\">"
    );
    for (i, &jn) in skin.joint_nodes.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let sid = dae_sid(nodes.get(jn).map_or("bone", |n| n.name.as_str()), jn);
        out.push_str(&sid);
    }
    out.push_str("</Name_array>\n");
    let _ = writeln!(
        out,
        "          <technique_common><accessor source=\"#skin_{si}-joints-array\" count=\"{j_count}\" stride=\"1\"><param name=\"JOINT\" type=\"name\"/></accessor></technique_common>"
    );
    out.push_str("        </source>\n");

    // Inverse bind matrices (COLLADA stores row-major 4x4!)
    let _ = writeln!(out, "        <source id=\"skin_{si}-bind_poses\">");
    let _ = write!(
        out,
        "          <float_array id=\"skin_{si}-bind_poses-array\" count=\"{}\">",
        j_count * 16
    );
    for j in 0..j_count {
        let cm = skin
            .inverse_bind_matrices_col_major
            .get(j)
            .copied()
            .unwrap_or(IDENTITY_MAT4);
        // Transpose col-major -> COLLADA row-major
        let rm = [
            cm[0], cm[4], cm[8], cm[12],
            cm[1], cm[5], cm[9], cm[13],
            cm[2], cm[6], cm[10], cm[14],
            cm[3], cm[7], cm[11], cm[15],
        ];
        if j > 0 {
            out.push(' ');
        }
        for (k, v) in rm.iter().enumerate() {
            if k > 0 {
                out.push(' ');
            }
            let _ = write!(out, "{v:.6}");
        }
    }
    out.push_str("</float_array>\n");
    let _ = writeln!(
        out,
        "          <technique_common><accessor source=\"#skin_{si}-bind_poses-array\" count=\"{j_count}\" stride=\"16\"><param name=\"TRANSFORM\" type=\"float4x4\"/></accessor></technique_common>"
    );
    out.push_str("        </source>\n");

    // Vertex weights
    let mut flat_weights = Vec::with_capacity(mesh.positions.len() * 2);
    let mut vcounts = Vec::with_capacity(mesh.positions.len());
    let mut pairs = Vec::with_capacity(mesh.positions.len() * 4);
    for v in 0..mesh.positions.len() {
        let js = mesh.joints.get(v).copied().unwrap_or([0; 4]);
        let ws = mesh.weights.get(v).copied().unwrap_or([1.0, 0.0, 0.0, 0.0]);
        let mut count = 0usize;
        for k in 0..4 {
            if ws[k] > 1e-6 && (js[k] as usize) < j_count {
                let w_idx = flat_weights.len();
                flat_weights.push(ws[k]);
                pairs.push((js[k] as usize, w_idx));
                count += 1;
            }
        }
        if count == 0 {
            let w_idx = flat_weights.len();
            flat_weights.push(1.0);
            pairs.push((0, w_idx));
            count = 1;
        }
        vcounts.push(count);
    }

    let _ = writeln!(out, "        <source id=\"skin_{si}-weights\">");
    let _ = write!(
        out,
        "          <float_array id=\"skin_{si}-weights-array\" count=\"{}\">",
        flat_weights.len()
    );
    for (i, w) in flat_weights.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{w:.6}");
    }
    out.push_str("</float_array>\n");
    let _ = writeln!(
        out,
        "          <technique_common><accessor source=\"#skin_{si}-weights-array\" count=\"{}\" stride=\"1\"><param name=\"WEIGHT\" type=\"float\"/></accessor></technique_common>",
        flat_weights.len()
    );
    out.push_str("        </source>\n");

    let _ = writeln!(
        out,
        "        <joints><input semantic=\"JOINT\" source=\"#skin_{si}-joints\"/><input semantic=\"INV_BIND_MATRIX\" source=\"#skin_{si}-bind_poses\"/></joints>"
    );
    let _ = writeln!(
        out,
        "        <vertex_weights count=\"{}\">",
        mesh.positions.len()
    );
    let _ = writeln!(
        out,
        "          <input semantic=\"JOINT\" source=\"#skin_{si}-joints\" offset=\"0\"/>"
    );
    let _ = writeln!(
        out,
        "          <input semantic=\"WEIGHT\" source=\"#skin_{si}-weights\" offset=\"1\"/>"
    );
    out.push_str("          <vcount>");
    for (i, c) in vcounts.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{c}");
    }
    out.push_str("</vcount>\n          <v>");
    for (i, (ji, wi)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{ji} {wi}");
    }
    out.push_str("</v>\n        </vertex_weights>\n      </skin>\n    </controller>\n");
}

fn write_dae_anim_track(
    out: &mut String,
    ci: usize,
    ti: usize,
    node: &crate::scene_extract::SceneNode,
    track: &crate::scene_extract::SceneAnimTrack,
) {
    let base_id = format!("anim_{ci}_{ti}");
    let target_id = format!("node_{}", track.node_idx);
    let key_count = track.times.len();

    let _ = writeln!(out, "    <animation id=\"{base_id}\">");
    let _ = writeln!(out, "      <source id=\"{base_id}-input\">");
    let _ = write!(
        out,
        "        <float_array id=\"{base_id}-input-array\" count=\"{key_count}\">"
    );
    for (i, t) in track.times.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{t:.5}");
    }
    out.push_str("</float_array>\n");
    let _ = writeln!(
        out,
        "        <technique_common><accessor source=\"#{base_id}-input-array\" count=\"{key_count}\" stride=\"1\"><param name=\"TIME\" type=\"float\"/></accessor></technique_common>"
    );
    out.push_str("      </source>\n");

    let _ = writeln!(out, "      <source id=\"{base_id}-output\">");
    let _ = write!(
        out,
        "        <float_array id=\"{base_id}-output-array\" count=\"{}\">",
        key_count * 16
    );
    for k in 0..key_count {
        let m_col = match track.property {
            TrackProperty::Translation => mat4_from_trs(
                [track.values[k * 3], track.values[k * 3 + 1], track.values[k * 3 + 2]],
                node.rotation,
                node.scale,
            ),
            TrackProperty::Rotation => mat4_from_trs(
                node.translation,
                [
                    track.values[k * 4],
                    track.values[k * 4 + 1],
                    track.values[k * 4 + 2],
                    track.values[k * 4 + 3],
                ],
                node.scale,
            ),
            TrackProperty::Scale => mat4_from_trs(
                node.translation,
                node.rotation,
                [track.values[k * 3], track.values[k * 3 + 1], track.values[k * 3 + 2]],
            ),
        };
        if k > 0 {
            out.push(' ');
        }
        let rm = [
            m_col[0], m_col[4], m_col[8], m_col[12],
            m_col[1], m_col[5], m_col[9], m_col[13],
            m_col[2], m_col[6], m_col[10], m_col[14],
            m_col[3], m_col[7], m_col[11], m_col[15],
        ];
        for (j, v) in rm.iter().enumerate() {
            if j > 0 {
                out.push(' ');
            }
            let _ = write!(out, "{v:.5}");
        }
    }
    out.push_str("</float_array>\n");
    let _ = writeln!(
        out,
        "        <technique_common><accessor source=\"#{base_id}-output-array\" count=\"{key_count}\" stride=\"16\"><param name=\"TRANSFORM\" type=\"float4x4\"/></accessor></technique_common>"
    );
    out.push_str("      </source>\n");

    let _ = writeln!(out, "      <source id=\"{base_id}-interp\">");
    let _ = write!(
        out,
        "        <Name_array id=\"{base_id}-interp-array\" count=\"{key_count}\">"
    );
    for k in 0..key_count {
        if k > 0 {
            out.push(' ');
        }
        out.push_str("LINEAR");
    }
    out.push_str("</Name_array>\n");
    let _ = writeln!(
        out,
        "        <technique_common><accessor source=\"#{base_id}-interp-array\" count=\"{key_count}\" stride=\"1\"><param name=\"INTERPOLATION\" type=\"name\"/></accessor></technique_common>"
    );
    out.push_str("      </source>\n");

    let _ = writeln!(
        out,
        "      <sampler id=\"{base_id}-sampler\"><input semantic=\"INPUT\" source=\"#{base_id}-input\"/><input semantic=\"OUTPUT\" source=\"#{base_id}-output\"/><input semantic=\"INTERPOLATION\" source=\"#{base_id}-interp\"/></sampler>"
    );
    let _ = writeln!(
        out,
        "      <channel source=\"#{base_id}-sampler\" target=\"{target_id}/transform\"/>"
    );
    out.push_str("    </animation>\n");
}

fn write_dae_node(
    out: &mut String,
    scene: &SceneDocument,
    idx: usize,
    indent: usize,
    visited: &mut [bool],
) {
    if idx >= scene.nodes.len() || visited[idx] {
        return;
    }
    visited[idx] = true;
    let node = &scene.nodes[idx];
    let pad = "  ".repeat(indent);
    let sid = dae_sid(&node.name, idx);
    let ty_attr = if node.is_bone {
        " type=\"JOINT\""
    } else {
        " type=\"NODE\""
    };

    let _ = writeln!(
        out,
        "{pad}<node id=\"node_{idx}\" name=\"{}\" sid=\"{sid}\"{ty_attr}>",
        escape_xml(&node.name)
    );

    let m = mat4_from_trs(node.translation, node.rotation, node.scale);
    let rm = [
        m[0], m[4], m[8], m[12],
        m[1], m[5], m[9], m[13],
        m[2], m[6], m[10], m[14],
        m[3], m[7], m[11], m[15],
    ];
    let _ = writeln!(
        out,
        "{pad}  <matrix sid=\"transform\">{:.6} {:.6} {:.6} {:.6} {:.6} {:.6} {:.6} {:.6} {:.6} {:.6} {:.6} {:.6} {:.6} {:.6} {:.6} {:.6}</matrix>",
        rm[0], rm[1], rm[2], rm[3], rm[4], rm[5], rm[6], rm[7], rm[8], rm[9], rm[10], rm[11],
        rm[12], rm[13], rm[14], rm[15]
    );

    if let Some(si) = node.skin_idx
        && let Some(skin) = scene.skins.get(si)
    {
        let mesh_idx = node.mesh_idx.unwrap_or(0);
        let root_id = skin
            .root_bone_idx
            .or_else(|| skin.joint_nodes.first().copied())
            .map_or_else(|| format!("#node_{idx}"), |r| format!("#node_{r}"));
        let _ = writeln!(out, "{pad}  <instance_controller url=\"#skin_{si}\">");
        let _ = writeln!(out, "{pad}    <skeleton>{root_id}</skeleton>");
        write_bind_material_block(out, &scene.meshes[mesh_idx], scene.materials.len(), indent + 2);
        let _ = writeln!(out, "{pad}  </instance_controller>");
    } else if let Some(mi) = node.mesh_idx {
        let _ = writeln!(out, "{pad}  <instance_geometry url=\"#mesh_{mi}\">");
        write_bind_material_block(out, &scene.meshes[mi], scene.materials.len(), indent + 2);
        let _ = writeln!(out, "{pad}  </instance_geometry>");
    }

    for &c_idx in &node.children {
        write_dae_node(out, scene, c_idx, indent + 1, visited);
    }

    let _ = writeln!(out, "{pad}</node>");
}

fn write_bind_material_block(out: &mut String, mesh: &SceneMesh, mat_count: usize, indent: usize) {
    let mut mats = Vec::new();
    for s in &mesh.submeshes {
        if let Some(mi) = s.material_idx
            && mi < mat_count
            && !mats.contains(&mi)
        {
            mats.push(mi);
        }
    }
    if mats.is_empty() {
        return;
    }
    let pad = "  ".repeat(indent);
    let _ = writeln!(out, "{pad}<bind_material><technique_common>");
    for mi in mats {
        let _ = writeln!(
            out,
            "{pad}  <instance_material symbol=\"mat_{mi}\" target=\"#mat_{mi}\"><bind_vertex_input semantic=\"UVMap\" input_semantic=\"TEXCOORD\" input_set=\"0\"/></instance_material>"
        );
    }
    let _ = writeln!(out, "{pad}</technique_common></bind_material>");
}

fn dae_sid(name: &str, idx: usize) -> String {
    let clean: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if clean.is_empty() {
        format!("node_{idx}")
    } else {
        format!("{clean}_{idx}")
    }
}

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use devx_plugin_api::{DecodedMesh, SubMeshSlice};

    #[test]
    fn exports_well_formed_collada_xml() {
        let mesh = DecodedMesh {
            name: "Quad&Tri".into(),
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: Vec::new(),
            uvs: Vec::new(),
            indices: vec![0, 1, 2],
            submeshes: vec![SubMeshSlice {
                first_index: 0,
                index_count: 3,
                topology: 0,
            }],
            bounds_min: [0.0, 0.0, 0.0],
            bounds_max: [1.0, 1.0, 0.0],
        };
        let dae = export_mesh_to_dae(&mesh).expect("dae");
        assert!(dae.contains("<COLLADA "));
        assert!(dae.contains("</COLLADA>"));
        assert!(dae.contains("Quad&amp;Tri"));
        assert!(dae.contains("<triangles count=\"1\">"));
        assert!(dae.contains("<instance_geometry url=\"#mesh_0\">"));
    }
}
