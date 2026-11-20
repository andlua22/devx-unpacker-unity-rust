//! Pure-Rust Unity `Mesh` (`class_id == 43`) vertex/index stream unpacker and OBJ/STL/GLB exporters.
//!
//! Unlike `unity-asset-decode`'s placeholder OBJ emitter, this module decodes actual vertex channel
//! formats (`Float32`, `Float16`, `UNorm8/16`, `SNorm8/16`) across single or multi-stream vertex buffers
//! plus 16-bit/32-bit triangle, quad, and triangle-strip index buffers.

use crate::{CodecError, CodecResult};
use devx_plugin_api::{DecodedMesh, SubMeshSlice};
use devx_unity_io::{ResourceStreamStore, extract_bytes_field, extract_i64_field};
use gltf_json as json;
use json::validation::Checked;
use serde_json::json;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use unity_asset_binary::object::UnityObject;
use unity_asset_core::UnityValue;
use unity_asset_decode::mesh::{ChannelInfo, Mesh, MeshParser};
use unity_asset_decode::unity_version::UnityVersion;

/// Optional skeleton bindpose and per-vertex skin weights extracted alongside a `Mesh`.
#[derive(Debug, Clone, Default)]
pub struct DecodedMeshSkin {
    /// Per-vertex up-to-4 joint indices referencing `SkinnedMeshRenderer.m_Bones`.
    pub joints: Vec<[u16; 4]>,
    /// Per-vertex normalized skin weights summing to `1.0`.
    pub weights: Vec<[f32; 4]>,
    /// Raw Unity row-major `m_BindPose` matrices (`[e00, e01, ..., e33]`).
    pub bind_poses_row_major: Vec<[f32; 16]>,
    /// Unity bone path CRC32 hashes (`m_BoneNameHashes`).
    pub bone_name_hashes: Vec<u32>,
}

/// Decoded Unity `Mesh.m_Shapes` morph target channel (sparse vertex/normal deltas in Unity LH coordinates).
#[derive(Debug, Clone, Default)]
pub struct DecodedBlendShape {
    pub name: String,
    pub vertex_indices: Vec<u32>,
    pub position_deltas: Vec<[f32; 3]>,
    pub normal_deltas: Vec<[f32; 3]>,
    pub has_normals: bool,
    pub default_weight: f32,
}

/// Complete mesh decode result combining geometry, optional skinning data, and blendshapes.
#[derive(Debug, Clone)]
pub struct DecodedMeshFull {
    pub base: DecodedMesh,
    pub skin: Option<DecodedMeshSkin>,
    pub blend_shapes: Vec<DecodedBlendShape>,
}

/// Decodes a Unity `Mesh` (`class_id == 43`) into normalized triangles, positions, normals, and UVs.
pub fn decode_mesh(
    obj: &UnityObject,
    unity_version: &str,
    streams: &ResourceStreamStore,
) -> CodecResult<DecodedMesh> {
    decode_mesh_full(obj, unity_version, streams).map(|f| f.base)
}

/// Decodes a Unity `Mesh` (`class_id == 43`) including `m_BindPose` and per-vertex bone weights/indices.
pub fn decode_mesh_full(
    obj: &UnityObject,
    unity_version: &str,
    streams: &ResourceStreamStore,
) -> CodecResult<DecodedMeshFull> {
    let version = UnityVersion::parse_version(unity_version).unwrap_or_default();
    let parser = MeshParser::new(version);
    let mut parsed = parser
        .parse_from_unity_object(obj)
        .map_err(|e| CodecError::MalformedData(format!("Mesh parse failed: {e}")))?
        .mesh;

    let props = obj.class.properties();
    if let Some(idx_fmt) = props.get("m_IndexFormat").and_then(|v| v.as_i64()) {
        parsed.index_format = idx_fmt as i32;
    }

    // Resolve streamed vertex/index buffer from `.resS` if present.
    let explicit_streams = extract_legacy_streams(props);
    let mut vertex_bytes = parsed.vertex_data.data_size.clone();
    let mut index_bytes = parsed.index_buffer.clone();

    if let Some(stream_info) = &parsed.stream_data
        && !stream_info.path.is_empty()
        && stream_info.size > 0
        && let Some(slice) =
            streams.read_slice(&stream_info.path, stream_info.offset, stream_info.size)
    {
        if vertex_bytes.is_empty() && !index_bytes.is_empty() {
            vertex_bytes = slice.to_vec();
        } else if vertex_bytes.is_empty() && index_bytes.is_empty() {
            // In older/combined streamed meshes, estimate vertex stream size from channels * vcount.
            let layouts = compute_stream_layouts(
                &parsed.vertex_data.channels,
                parsed.vertex_data.vertex_count as usize,
                &explicit_streams,
            );
            let expected_vb: usize = layouts
                .iter()
                .map(|l| l.byte_offset + l.stride * parsed.vertex_data.vertex_count as usize)
                .max()
                .unwrap_or(0);
            if expected_vb > 0 && expected_vb <= slice.len() {
                vertex_bytes = slice[..expected_vb].to_vec();
                index_bytes = slice[expected_vb..].to_vec();
            } else {
                vertex_bytes = slice.to_vec();
            }
        }
    }

    // Also pull `m_VertexData.m_Data` (Unity 5/2017 used `m_Data` before renaming to `m_DataSize`).
    if vertex_bytes.is_empty()
        && let Some(vd) = props.get("m_VertexData").and_then(|v| v.as_object())
        && let Some(data_val) = vd.get("m_Data").or_else(|| vd.get("m_DataSize"))
        && let Some(b) = extract_bytes_field(data_val)
    {
        vertex_bytes = b;
    }

    let vcount = parsed.vertex_data.vertex_count as usize;
    let is_legacy_channels = parsed.vertex_data.channels.len() <= 8;

    let (pos_slot, norm_slot, uv0_slot) = if is_legacy_channels {
        // Unity <= 2017: 0=Vertex, 1=Normal, 2=Color, 3=UV0, 4=UV1, 5=Tangent
        (0usize, Some(1usize), Some(3usize))
    } else {
        // Unity 2018+: 0=Position, 1=Normal, 2=Tangent, 3=Color, 4=TexCoord0
        (0usize, Some(1usize), Some(4usize))
    };

    let layouts = compute_stream_layouts(&parsed.vertex_data.channels, vcount, &explicit_streams);

    let mut positions = read_vec3_channel(
        &parsed.vertex_data.channels,
        pos_slot,
        vcount,
        &layouts,
        &vertex_bytes,
    );

    if positions.is_empty() && vcount == 0 {
        // Fall back to compressed mesh quantized positions if present.
        if let Some((comp_pos, comp_indices)) = try_decode_compressed_mesh(props) {
            return Ok(DecodedMeshFull {
                base: finalize_decoded_mesh(
                    parsed.name,
                    comp_pos,
                    Vec::new(),
                    Vec::new(),
                    comp_indices,
                    Vec::new(),
                ),
                skin: None,
                blend_shapes: Vec::new(),
            });
        }
    }

    if positions.is_empty() && vcount > 0 {
        positions.resize(vcount, [0.0, 0.0, 0.0]);
    }

    let normals = norm_slot
        .map(|s| read_vec3_channel(&parsed.vertex_data.channels, s, vcount, &layouts, &vertex_bytes))
        .unwrap_or_default();

    let uvs = uv0_slot
        .map(|s| read_vec2_channel(&parsed.vertex_data.channels, s, vcount, &layouts, &vertex_bytes))
        .unwrap_or_default();

    let skin = extract_mesh_skin(
        props,
        &parsed.vertex_data.channels,
        is_legacy_channels,
        vcount,
        &layouts,
        &vertex_bytes,
    );

    let is_u32_indices = parsed.index_format == 1;
    let (indices, submeshes) = decode_submeshes_and_indices(
        &parsed,
        &index_bytes,
        is_u32_indices,
        positions.len(),
    );

    if positions.is_empty() {
        return Err(CodecError::MalformedData(format!(
            "Mesh '{}' decoded 0 vertices",
            parsed.name
        )));
    }

    let blend_shapes = extract_mesh_blend_shapes(props, positions.len());

    Ok(DecodedMeshFull {
        base: finalize_decoded_mesh(
            parsed.name,
            positions,
            normals,
            uvs,
            indices,
            submeshes,
        ),
        skin,
        blend_shapes,
    })
}

fn unity_f32(val: &UnityValue) -> Option<f32> {
    val.as_f64()
        .or_else(|| extract_i64_field(val).map(|i| i as f64))
        .map(|f| if f.is_finite() { f as f32 } else { 0.0 })
}

fn parse_vec3_field(val: &UnityValue) -> Option<[f32; 3]> {
    let o = val.as_object()?;
    Some([
        o.get("x").and_then(unity_f32).unwrap_or(0.0),
        o.get("y").and_then(unity_f32).unwrap_or(0.0),
        o.get("z").and_then(unity_f32).unwrap_or(0.0),
    ])
}

/// Extracts `Mesh.m_Shapes` morph target channels into sparse vertex & normal delta lists.
pub fn extract_mesh_blend_shapes(
    props: &indexmap::IndexMap<String, UnityValue>,
    vertex_count: usize,
) -> Vec<DecodedBlendShape> {
    let Some(m_shapes) = props.get("m_Shapes").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let Some(raw_verts) = m_shapes.get("vertices").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    if raw_verts.is_empty() || vertex_count == 0 {
        return Vec::new();
    }

    struct RawBsVert {
        pos: [f32; 3],
        norm: [f32; 3],
        index: u32,
    }
    let verts: Vec<RawBsVert> = raw_verts
        .iter()
        .filter_map(|item| {
            let o = item.as_object()?;
            let idx = o.get("index").and_then(extract_i64_field).unwrap_or(-1);
            if idx < 0 || (idx as usize) >= vertex_count {
                return None;
            }
            Some(RawBsVert {
                pos: o.get("vertex").and_then(parse_vec3_field).unwrap_or([0.0; 3]),
                norm: o.get("normal").and_then(parse_vec3_field).unwrap_or([0.0; 3]),
                index: idx as u32,
            })
        })
        .collect();
    if verts.is_empty() {
        return Vec::new();
    }

    struct RawBsShape {
        first: usize,
        count: usize,
        has_normals: bool,
    }
    let shapes: Vec<RawBsShape> = m_shapes
        .get("shapes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let o = item.as_object()?;
                    let first = o.get("firstVertex").and_then(extract_i64_field).unwrap_or(0).max(0) as usize;
                    let count = o.get("vertexCount").and_then(extract_i64_field).unwrap_or(0).max(0) as usize;
                    let has_normals = o.get("hasNormals").and_then(UnityValue::as_bool).unwrap_or(true);
                    Some(RawBsShape {
                        first,
                        count,
                        has_normals,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    if let Some(channels) = m_shapes.get("channels").and_then(|v| v.as_array())
        && !channels.is_empty()
    {
        for (ch_idx, ch) in channels.iter().enumerate() {
            let Some(co) = ch.as_object() else {
                continue;
            };
            let name = co
                .get("name")
                .and_then(UnityValue::as_str)
                .map(str::to_owned)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("BlendShape_{ch_idx}"));
            let frame_idx = co.get("frameIndex").and_then(extract_i64_field).unwrap_or(ch_idx as i64).max(0) as usize;
            let frame_count = co.get("frameCount").and_then(extract_i64_field).unwrap_or(1).max(1) as usize;
            let target_shape_idx = frame_idx.saturating_add(frame_count - 1);
            let Some(sh) = shapes.get(target_shape_idx) else {
                continue;
            };
            let start = sh.first.min(verts.len());
            let end = start.saturating_add(sh.count).min(verts.len());
            if start >= end {
                continue;
            }
            let slice = &verts[start..end];
            out.push(DecodedBlendShape {
                name,
                vertex_indices: slice.iter().map(|v| v.index).collect(),
                position_deltas: slice.iter().map(|v| v.pos).collect(),
                normal_deltas: slice.iter().map(|v| v.norm).collect(),
                has_normals: sh.has_normals,
                default_weight: 0.0,
            });
        }
    } else {
        for (si, sh) in shapes.iter().enumerate() {
            let start = sh.first.min(verts.len());
            let end = start.saturating_add(sh.count).min(verts.len());
            if start >= end {
                continue;
            }
            let slice = &verts[start..end];
            out.push(DecodedBlendShape {
                name: format!("BlendShape_{si}"),
                vertex_indices: slice.iter().map(|v| v.index).collect(),
                position_deltas: slice.iter().map(|v| v.pos).collect(),
                normal_deltas: slice.iter().map(|v| v.norm).collect(),
                has_normals: sh.has_normals,
                default_weight: 0.0,
            });
        }
    }
    out
}

/// Applies normalized blendshape weights (`0.0..=1.0`) onto base vertex positions and normals.
pub fn apply_blend_shapes(
    base_positions: &[[f32; 3]],
    base_normals: &[[f32; 3]],
    shapes: &[DecodedBlendShape],
    weights: &[f32],
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let mut positions = base_positions.to_vec();
    let mut normals = base_normals.to_vec();
    if shapes.is_empty() {
        return (positions, normals);
    }

    let has_norms = normals.len() == positions.len();
    let mut norm_touched = false;

    for (idx, shape) in shapes.iter().enumerate() {
        let w = weights.get(idx).copied().unwrap_or(shape.default_weight);
        if w.abs() <= 1e-5 {
            continue;
        }
        for k in 0..shape.vertex_indices.len().min(shape.position_deltas.len()) {
            let vi = shape.vertex_indices[k] as usize;
            if vi >= positions.len() {
                continue;
            }
            let dp = shape.position_deltas[k];
            positions[vi][0] += dp[0] * w;
            positions[vi][1] += dp[1] * w;
            positions[vi][2] += dp[2] * w;
            if has_norms && shape.has_normals && let Some(dn) = shape.normal_deltas.get(k) {
                normals[vi][0] += dn[0] * w;
                normals[vi][1] += dn[1] * w;
                normals[vi][2] += dn[2] * w;
                norm_touched = true;
            }
        }
    }

    if norm_touched {
        for n in &mut normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len > 1e-6 {
                n[0] /= len;
                n[1] /= len;
                n[2] /= len;
            } else {
                *n = [0.0, 1.0, 0.0];
            }
        }
    }

    (positions, normals)
}

pub(crate) fn parse_matrix4x4_row_major(val: &UnityValue) -> Option<[f32; 16]> {
    let obj = val.as_object()?;
    let mut m = [0.0f32; 16];
    let keys = [
        "e00", "e01", "e02", "e03", "e10", "e11", "e12", "e13", "e20", "e21", "e22", "e23", "e30",
        "e31", "e32", "e33",
    ];
    for (idx, k) in keys.iter().enumerate() {
        m[idx] = obj.get(*k)?.as_f64()? as f32;
    }
    Some(m)
}

fn extract_mesh_skin(
    props: &indexmap::IndexMap<String, UnityValue>,
    channels: &[ChannelInfo],
    is_legacy_channels: bool,
    vcount: usize,
    layouts: &[StreamLayout],
    vertex_bytes: &[u8],
) -> Option<DecodedMeshSkin> {
    let bind_poses_row_major: Vec<[f32; 16]> = props
        .get("m_BindPose")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(parse_matrix4x4_row_major)
                .collect()
        })
        .unwrap_or_default();

    let bone_name_hashes: Vec<u32> = props
        .get("m_BoneNameHashes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_i64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default();

    let (mut weights, mut joints) = if !is_legacy_channels && channels.len() >= 14 {
        let w = read_vec4_weight_channel(channels, 12, vcount, layouts, vertex_bytes);
        let j = read_vec4_joint_channel(channels, 13, vcount, layouts, vertex_bytes);
        (w, j)
    } else {
        (Vec::new(), Vec::new())
    };

    // Unity omits the weight stream for optimized one-influence meshes. The
    // remaining scalar bone index carries an implicit unit weight.
    if !is_legacy_channels
        && channels.get(12).is_some_and(|ch| ch.dimension & 0x0F == 0)
        && channels.get(13).is_some_and(|ch| ch.dimension & 0x0F == 1)
        && weights.is_empty()
        && joints.len() == vcount
        && !bind_poses_row_major.is_empty()
        && joints.iter().all(|j| usize::from(j[0]) < bind_poses_row_major.len())
    {
        weights.resize(vcount, [1.0, 0.0, 0.0, 0.0]);
    }

    if (weights.len() != vcount || joints.len() != vcount)
        && let Some(skin_arr) = props.get("m_Skin").and_then(|v| v.as_array())
        && !skin_arr.is_empty()
    {
        weights.clear();
        joints.clear();
        weights.reserve(skin_arr.len());
        joints.reserve(skin_arr.len());
        for entry in skin_arr {
            if let Some(obj) = entry.as_object() {
                let w = [
                    obj.get("weight[0]").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                    obj.get("weight[1]").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                    obj.get("weight[2]").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                    obj.get("weight[3]").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                ];
                let j = [
                    obj.get("boneIndex[0]").and_then(|v| v.as_i64()).unwrap_or(0).clamp(0, u16::MAX as i64) as u16,
                    obj.get("boneIndex[1]").and_then(|v| v.as_i64()).unwrap_or(0).clamp(0, u16::MAX as i64) as u16,
                    obj.get("boneIndex[2]").and_then(|v| v.as_i64()).unwrap_or(0).clamp(0, u16::MAX as i64) as u16,
                    obj.get("boneIndex[3]").and_then(|v| v.as_i64()).unwrap_or(0).clamp(0, u16::MAX as i64) as u16,
                ];
                weights.push(w);
                joints.push(j);
            }
        }
    }

    if (weights.len() != vcount || joints.len() != vcount)
        && let Some(vbw) = props
            .get("m_VariableBoneCountWeights")
            .and_then(|v| v.as_object())
        && let Some(bpv_val) = vbw.get("m_BonesPerVertex")
        && let Some(bpv) = extract_bytes_field(bpv_val)
        && !bpv.is_empty()
        && let Some(w_arr) = vbw.get("m_Weights").and_then(|v| v.as_array())
    {
        weights.clear();
        joints.clear();
        let mut cursor = 0usize;
        for &count in &bpv {
            let count = count as usize;
            let mut pairs: Vec<(f32, u16)> = Vec::with_capacity(count);
            for item in w_arr.iter().skip(cursor).take(count) {
                if let Some(obj) = item.as_object() {
                    let w = obj.get("weight").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                    let b = obj
                        .get("boneIndex")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0)
                        .clamp(0, u16::MAX as i64) as u16;
                    pairs.push((w, b));
                }
            }
            cursor = cursor.saturating_add(count);
            pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
            let mut w4 = [0.0f32; 4];
            let mut j4 = [0u16; 4];
            for (slot, (w, j)) in pairs.into_iter().take(4).enumerate() {
                w4[slot] = w;
                j4[slot] = j;
            }
            weights.push(w4);
            joints.push(j4);
        }
    }

    let has_skin_weights = weights.len() == vcount && joints.len() == vcount && vcount > 0;
    if !has_skin_weights {
        weights.clear();
        joints.clear();
    } else {
        normalize_skin_weights(&mut weights, &mut joints);
    }

    if bind_poses_row_major.is_empty() && joints.is_empty() {
        return None;
    }

    Some(DecodedMeshSkin {
        joints,
        weights,
        bind_poses_row_major,
        bone_name_hashes,
    })
}

fn read_vec4_weight_channel(
    channels: &[ChannelInfo],
    slot: usize,
    vcount: usize,
    layouts: &[StreamLayout],
    data: &[u8],
) -> Vec<[f32; 4]> {
    let Some(ch) = channels.get(slot) else {
        return Vec::new();
    };
    let dim = (ch.dimension & 0x0F) as usize;
    if dim == 0 || vcount == 0 {
        return Vec::new();
    }
    let Some(layout) = layouts.get(ch.stream as usize) else {
        return Vec::new();
    };
    if layout.stride == 0 {
        return Vec::new();
    }
    let csize = format_component_bytes(ch.format);
    let mut out = Vec::with_capacity(vcount);
    for v in 0..vcount {
        let base = layout.byte_offset + v * layout.stride + ch.offset as usize;
        if base + dim * csize > data.len() {
            break;
        }
        let mut w = [0.0f32; 4];
        for (i, dst) in w.iter_mut().enumerate().take(dim.min(4)) {
            *dst = read_component_f32(ch.format, &data[base + i * csize..]).unwrap_or(0.0);
        }
        out.push(w);
    }
    out
}

fn read_component_u16(format: u8, bytes: &[u8]) -> Option<u16> {
    match format {
        0 => Some((f32::from_le_bytes(bytes.get(..4)?.try_into().ok()?).round()).clamp(0.0, u16::MAX as f32) as u16),
        2 | 6 => Some(*bytes.first()? as u16),
        3 | 7 => Some((*bytes.first()? as i8).max(0) as u16),
        4 | 8 => Some(u16::from_le_bytes(bytes.get(..2)?.try_into().ok()?)),
        5 | 9 => Some(i16::from_le_bytes(bytes.get(..2)?.try_into().ok()?).max(0) as u16),
        10 => Some(u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?).min(u16::MAX as u32) as u16),
        11 => Some(i32::from_le_bytes(bytes.get(..4)?.try_into().ok()?).clamp(0, u16::MAX as i32) as u16),
        _ => None,
    }
}

fn read_vec4_joint_channel(
    channels: &[ChannelInfo],
    slot: usize,
    vcount: usize,
    layouts: &[StreamLayout],
    data: &[u8],
) -> Vec<[u16; 4]> {
    let Some(ch) = channels.get(slot) else {
        return Vec::new();
    };
    let dim = (ch.dimension & 0x0F) as usize;
    if dim == 0 || vcount == 0 {
        return Vec::new();
    }
    let Some(layout) = layouts.get(ch.stream as usize) else {
        return Vec::new();
    };
    if layout.stride == 0 {
        return Vec::new();
    }
    let csize = format_component_bytes(ch.format);
    let mut out = Vec::with_capacity(vcount);
    for v in 0..vcount {
        let base = layout.byte_offset + v * layout.stride + ch.offset as usize;
        if base + dim * csize > data.len() {
            break;
        }
        let mut j = [0u16; 4];
        for (i, dst) in j.iter_mut().enumerate().take(dim.min(4)) {
            *dst = read_component_u16(ch.format, &data[base + i * csize..]).unwrap_or(0);
        }
        out.push(j);
    }
    out
}

pub(crate) fn normalize_skin_weights(weights: &mut [[f32; 4]], joints: &mut [[u16; 4]]) {
    for (w, j) in weights.iter_mut().zip(joints.iter_mut()) {
        for slot in 0..4 {
            if !w[slot].is_finite() || w[slot] <= 0.0 {
                w[slot] = 0.0;
                j[slot] = 0;
            }
        }
        let sum: f32 = w.iter().sum();
        if sum > 1e-6 {
            for val in w.iter_mut() {
                *val /= sum;
            }
        } else {
            *w = [1.0, 0.0, 0.0, 0.0];
            *j = [0, 0, 0, 0];
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct StreamLayout {
    stride: usize,
    byte_offset: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct LegacyStreamInfo {
    offset: usize,
    stride: usize,
}

fn extract_legacy_streams(props: &indexmap::IndexMap<String, UnityValue>) -> Vec<LegacyStreamInfo> {
    let Some(vd) = props.get("m_VertexData").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let Some(arr) = vd.get("m_Streams").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            Some(LegacyStreamInfo {
                offset: obj.get("offset").and_then(|v| v.as_i64()).unwrap_or(0).max(0) as usize,
                stride: obj.get("stride").and_then(|v| v.as_i64()).unwrap_or(0).max(0) as usize,
            })
        })
        .collect()
}

fn compute_stream_layouts(
    channels: &[ChannelInfo],
    vcount: usize,
    legacy: &[LegacyStreamInfo],
) -> Vec<StreamLayout> {
    let max_stream = channels
        .iter()
        .map(|c| c.stream as usize)
        .max()
        .unwrap_or(0);
    let count = (max_stream + 1).max(legacy.len());
    let mut layouts = vec![StreamLayout::default(); count];

    for (idx, leg) in legacy.iter().enumerate() {
        layouts[idx].stride = leg.stride;
        layouts[idx].byte_offset = leg.offset;
    }

    for ch in channels {
        let dim = (ch.dimension & 0x0F) as usize;
        if dim == 0 {
            continue;
        }
        let s = ch.stream as usize;
        if s >= layouts.len() {
            continue;
        }
        let end = ch.offset as usize + dim * format_component_bytes(ch.format);
        if end > layouts[s].stride {
            layouts[s].stride = end;
        }
    }

    if legacy.is_empty() {
        let mut cursor = 0usize;
        for layout in &mut layouts {
            layout.byte_offset = cursor;
            let raw_bytes = layout.stride.saturating_mul(vcount);
            // Unity aligns each stream inside `m_DataSize` to 16 bytes (safe 4-byte align minimum).
            let aligned = (raw_bytes + 15) & !15;
            cursor = cursor.saturating_add(aligned);
        }
    }
    layouts
}

fn format_component_bytes(format: u8) -> usize {
    match format {
        0 | 10 | 11 => 4,       // Float32, UInt32, SInt32
        1 | 4 | 5 | 8 | 9 => 2, // Float16, UNorm16, SNorm16, UInt16, SInt16
        2 | 3 | 6 | 7 => 1,     // UNorm8, SNorm8, UInt8, SInt8
        _ => 4,
    }
}

fn read_component_f32(format: u8, bytes: &[u8]) -> Option<f32> {
    match format {
        0 => Some(f32::from_le_bytes(bytes.get(..4)?.try_into().ok()?)),
        1 => Some(f16_to_f32(u16::from_le_bytes(bytes.get(..2)?.try_into().ok()?))),
        2 => Some(*bytes.first()? as f32 / 255.0),
        3 => Some((*bytes.first()? as i8 as f32 / 127.0).max(-1.0)),
        4 => Some(u16::from_le_bytes(bytes.get(..2)?.try_into().ok()?) as f32 / 65535.0),
        5 => Some(
            (i16::from_le_bytes(bytes.get(..2)?.try_into().ok()?) as f32 / 32767.0).max(-1.0),
        ),
        6 => Some(*bytes.first()? as f32),
        7 => Some(*bytes.first()? as i8 as f32),
        8 => Some(u16::from_le_bytes(bytes.get(..2)?.try_into().ok()?) as f32),
        9 => Some(i16::from_le_bytes(bytes.get(..2)?.try_into().ok()?) as f32),
        10 => Some(u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?) as f32),
        11 => Some(i32::from_le_bytes(bytes.get(..4)?.try_into().ok()?) as f32),
        _ => None,
    }
}

fn read_vec3_channel(
    channels: &[ChannelInfo],
    slot: usize,
    vcount: usize,
    layouts: &[StreamLayout],
    data: &[u8],
) -> Vec<[f32; 3]> {
    let Some(ch) = channels.get(slot) else {
        return Vec::new();
    };
    let dim = (ch.dimension & 0x0F) as usize;
    if dim == 0 || vcount == 0 {
        return Vec::new();
    }
    let Some(layout) = layouts.get(ch.stream as usize) else {
        return Vec::new();
    };
    if layout.stride == 0 {
        return Vec::new();
    }
    let csize = format_component_bytes(ch.format);
    let mut out = Vec::with_capacity(vcount);
    for v in 0..vcount {
        let base = layout.byte_offset + v * layout.stride + ch.offset as usize;
        if base + dim * csize > data.len() {
            break;
        }
        let x = read_component_f32(ch.format, &data[base..]).unwrap_or(0.0);
        let y = if dim >= 2 {
            read_component_f32(ch.format, &data[base + csize..]).unwrap_or(0.0)
        } else {
            0.0
        };
        let z = if dim >= 3 {
            read_component_f32(ch.format, &data[base + csize * 2..]).unwrap_or(0.0)
        } else {
            0.0
        };
        out.push([x, y, z]);
    }
    out
}

fn read_vec2_channel(
    channels: &[ChannelInfo],
    slot: usize,
    vcount: usize,
    layouts: &[StreamLayout],
    data: &[u8],
) -> Vec<[f32; 2]> {
    let Some(ch) = channels.get(slot) else {
        return Vec::new();
    };
    let dim = (ch.dimension & 0x0F) as usize;
    if dim == 0 || vcount == 0 {
        return Vec::new();
    }
    let Some(layout) = layouts.get(ch.stream as usize) else {
        return Vec::new();
    };
    if layout.stride == 0 {
        return Vec::new();
    }
    let csize = format_component_bytes(ch.format);
    let mut out = Vec::with_capacity(vcount);
    for v in 0..vcount {
        let base = layout.byte_offset + v * layout.stride + ch.offset as usize;
        if base + dim * csize > data.len() {
            break;
        }
        let u = read_component_f32(ch.format, &data[base..]).unwrap_or(0.0);
        let v_coord = if dim >= 2 {
            read_component_f32(ch.format, &data[base + csize..]).unwrap_or(0.0)
        } else {
            0.0
        };
        out.push([u, v_coord]);
    }
    out
}

fn decode_submeshes_and_indices(
    mesh: &Mesh,
    index_bytes: &[u8],
    is_u32: bool,
    vertex_count: usize,
) -> (Vec<u32>, Vec<SubMeshSlice>) {
    let read_idx = |byte_off: usize| -> Option<u32> {
        if is_u32 {
            let b: [u8; 4] = index_bytes.get(byte_off..byte_off + 4)?.try_into().ok()?;
            Some(u32::from_le_bytes(b))
        } else {
            let b: [u8; 2] = index_bytes.get(byte_off..byte_off + 2)?.try_into().ok()?;
            Some(u16::from_le_bytes(b) as u32)
        }
    };
    let idx_stride = if is_u32 { 4usize } else { 2usize };

    let mut all_indices = Vec::new();
    let mut submeshes = Vec::new();

    if mesh.sub_meshes.is_empty() {
        let count = index_bytes.len() / idx_stride;
        let tri_count = count / 3;
        for t in 0..tri_count {
            let a = read_idx((t * 3) * idx_stride).unwrap_or(0);
            let b = read_idx((t * 3 + 1) * idx_stride).unwrap_or(0);
            let c = read_idx((t * 3 + 2) * idx_stride).unwrap_or(0);
            if (a as usize) < vertex_count
                && (b as usize) < vertex_count
                && (c as usize) < vertex_count
            {
                all_indices.extend_from_slice(&[a, b, c]);
            }
        }
        if !all_indices.is_empty() {
            submeshes.push(SubMeshSlice {
                first_index: 0,
                index_count: all_indices.len() as u32,
                topology: 0,
            });
        }
        return (all_indices, submeshes);
    }

    for sub in &mesh.sub_meshes {
        let first_tri_index = all_indices.len() as u32;
        let base_byte = sub.first_byte as usize;
        let count = sub.index_count as usize;
        let raw_indices: Vec<u32> = (0..count)
            .filter_map(|i| read_idx(base_byte + i * idx_stride))
            .map(|i| i + sub.base_vertex)
            .collect();

        match sub.topology {
            0 => {
                // Triangles
                for tri in raw_indices.as_chunks::<3>().0 {
                    if tri.iter().all(|&i| (i as usize) < vertex_count) {
                        all_indices.extend_from_slice(tri);
                    }
                }
            }
            1 => {
                // TriangleStrip
                for (i, w) in raw_indices.windows(3).enumerate() {
                    let (a, b, c) = if i % 2 == 0 {
                        (w[0], w[1], w[2])
                    } else {
                        (w[1], w[0], w[2])
                    };
                    if a != b
                        && b != c
                        && a != c
                        && (a as usize) < vertex_count
                        && (b as usize) < vertex_count
                        && (c as usize) < vertex_count
                    {
                        all_indices.extend_from_slice(&[a, b, c]);
                    }
                }
            }
            2 => {
                // Quads -> 2 triangles
                for quad in raw_indices.as_chunks::<4>().0 {
                    if quad.iter().all(|&i| (i as usize) < vertex_count) {
                        all_indices.extend_from_slice(&[
                            quad[0], quad[1], quad[2], quad[0], quad[2], quad[3],
                        ]);
                    }
                }
            }
            _ => {}
        }

        let produced = all_indices.len() as u32 - first_tri_index;
        submeshes.push(SubMeshSlice {
            first_index: first_tri_index,
            index_count: produced,
            topology: 0,
        });
    }

    (all_indices, submeshes)
}

fn try_decode_compressed_mesh(
    props: &indexmap::IndexMap<String, UnityValue>,
) -> Option<(Vec<[f32; 3]>, Vec<u32>)> {
    let cm = props.get("m_CompressedMesh")?.as_object()?;
    let verts = unpack_packed_float_vector(cm.get("m_Vertices")?)?;
    let tris = unpack_packed_int_vector(cm.get("m_Triangles")?)?;
    if verts.len() < 3 || tris.len() < 3 {
        return None;
    }
    let positions: Vec<[f32; 3]> = verts.as_chunks::<3>().0.to_vec();
    Some((positions, tris))
}

fn unpack_packed_float_vector(val: &UnityValue) -> Option<Vec<f32>> {
    let obj = val.as_object()?;
    let num_items = obj.get("m_NumItems")?.as_i64()? as usize;
    let range = obj.get("m_Range")?.as_f64()? as f32;
    let start = obj.get("m_Start")?.as_f64()? as f32;
    let bit_size = obj.get("m_BitSize")?.as_i64()? as u8;
    let data = extract_bytes_field(obj.get("m_Data")?)?;
    let ints = unpack_bit_stream(&data, num_items, bit_size);
    let max_val = ((1u64 << bit_size) - 1).max(1) as f32;
    Some(
        ints.into_iter()
            .map(|q| start + (q as f32 / max_val) * range)
            .collect(),
    )
}

fn unpack_packed_int_vector(val: &UnityValue) -> Option<Vec<u32>> {
    let obj = val.as_object()?;
    let num_items = obj.get("m_NumItems")?.as_i64()? as usize;
    let bit_size = obj.get("m_BitSize")?.as_i64()? as u8;
    let data = extract_bytes_field(obj.get("m_Data")?)?;
    Some(unpack_bit_stream(&data, num_items, bit_size))
}

fn unpack_bit_stream(data: &[u8], count: usize, bit_size: u8) -> Vec<u32> {
    if bit_size == 0 || bit_size > 32 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(count);
    let mut bit_pos = 0usize;
    let bits_per_item = bit_size as usize;
    for _ in 0..count {
        let mut acc = 0u32;
        for b in 0..bits_per_item {
            let byte_idx = (bit_pos + b) / 8;
            let bit_in_byte = (bit_pos + b) % 8;
            if let Some(&byte_val) = data.get(byte_idx) {
                let bit = ((byte_val >> bit_in_byte) & 1) as u32;
                acc |= bit << b;
            }
        }
        out.push(acc);
        bit_pos += bits_per_item;
    }
    out
}

fn finalize_decoded_mesh(
    name: String,
    positions: Vec<[f32; 3]>,
    mut normals: Vec<[f32; 3]>,
    mut uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
    mut submeshes: Vec<SubMeshSlice>,
) -> DecodedMesh {
    let mut bounds_min = [f32::INFINITY; 3];
    let mut bounds_max = [f32::NEG_INFINITY; 3];
    for p in &positions {
        for axis in 0..3 {
            bounds_min[axis] = bounds_min[axis].min(p[axis]);
            bounds_max[axis] = bounds_max[axis].max(p[axis]);
        }
    }
    if positions.is_empty() {
        bounds_min = [0.0; 3];
        bounds_max = [0.0; 3];
    }

    if normals.len() != positions.len() {
        normals = compute_smooth_normals(&positions, &indices);
    }
    if uvs.len() != positions.len() {
        uvs.resize(positions.len(), [0.0, 0.0]);
    }
    if submeshes.is_empty() && !indices.is_empty() {
        submeshes.push(SubMeshSlice {
            first_index: 0,
            index_count: indices.len() as u32,
            topology: 0,
        });
    }

    DecodedMesh {
        name,
        positions,
        normals,
        uvs,
        indices,
        submeshes,
        bounds_min,
        bounds_max,
    }
}

fn compute_smooth_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut acc = vec![[0.0f32; 3]; positions.len()];
    for tri in indices.as_chunks::<3>().0 {
        let (a, b, c) = (
            positions[tri[0] as usize],
            positions[tri[1] as usize],
            positions[tri[2] as usize],
        );
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        for &idx in tri {
            acc[idx as usize][0] += n[0];
            acc[idx as usize][1] += n[1];
            acc[idx as usize][2] += n[2];
        }
    }
    for n in &mut acc {
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len > 1e-6 {
            n[0] /= len;
            n[1] /= len;
            n[2] /= len;
        } else {
            *n = [0.0, 1.0, 0.0];
        }
    }
    acc
}

/// Converts IEEE 754 binary16 bits to `f32`.
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits & 0x8000) as u32) << 16;
    let exp = ((bits & 0x7C00) >> 10) as u32;
    let mant = (bits & 0x03FF) as u32;
    let out_bits = if exp == 0 {
        if mant == 0 {
            sign
        } else {
            let mut m = mant;
            let mut e = 127i32 - 15 + 1;
            while (m & 0x0400) == 0 {
                m <<= 1;
                e -= 1;
            }
            m &= 0x03FF;
            sign | ((e as u32) << 23) | (m << 13)
        }
    } else if exp == 0x1F {
        sign | 0x7F80_0000 | (mant << 13)
    } else {
        sign | ((exp + 127 - 15) << 23) | (mant << 13)
    };
    f32::from_bits(out_bits)
}

/// Exports a [`DecodedMesh`] to Wavefront `.obj` text.
pub fn export_obj(mesh: &DecodedMesh) -> String {
    export_obj_internal(mesh, None, &[])
}

/// Exports a [`DecodedMesh`] to Wavefront `.obj` text with an external `.mtl` library reference
/// (`mtllib <mtl_filename>`) and `usemtl <mat_name>` per submesh.
pub fn export_obj_with_mtl(
    mesh: &DecodedMesh,
    mtl_filename: &str,
    submesh_materials: &[String],
) -> String {
    export_obj_internal(mesh, Some(mtl_filename), submesh_materials)
}

fn export_obj_internal(
    mesh: &DecodedMesh,
    mtl_filename: Option<&str>,
    submesh_materials: &[String],
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Exported by devx-codecs (DevX Unity Rust)");
    if let Some(mtl) = mtl_filename {
        let _ = writeln!(out, "mtllib {mtl}");
    }
    let _ = writeln!(out, "o {}", sanitize_obj_name(&mesh.name));

    for &[x, y, z] in &mesh.positions {
        let _ = writeln!(out, "v {x:.6} {y:.6} {z:.6}");
    }
    for &[u, v] in &mesh.uvs {
        let _ = writeln!(out, "vt {u:.6} {v:.6}");
    }
    for &[nx, ny, nz] in &mesh.normals {
        let _ = writeln!(out, "vn {nx:.6} {ny:.6} {nz:.6}");
    }

    let has_vt = mesh.uvs.len() == mesh.positions.len();
    let has_vn = mesh.normals.len() == mesh.positions.len();

    for (sub_idx, sub) in mesh.submeshes.iter().enumerate() {
        let _ = writeln!(out, "g submesh_{sub_idx}");
        if let Some(mat_name) = submesh_materials.get(sub_idx).or_else(|| submesh_materials.last()) {
            let _ = writeln!(out, "usemtl {}", sanitize_obj_name(mat_name));
        }
        let start = sub.first_index as usize;
        let end = (start + sub.index_count as usize).min(mesh.indices.len());
        for tri in mesh.indices[start..end].as_chunks::<3>().0 {
            let (i0, i1, i2) = (tri[0] + 1, tri[1] + 1, tri[2] + 1);
            match (has_vt, has_vn) {
                (true, true) => {
                    let _ = writeln!(out, "f {i0}/{i0}/{i0} {i1}/{i1}/{i1} {i2}/{i2}/{i2}");
                }
                (false, true) => {
                    let _ = writeln!(out, "f {i0}//{i0} {i1}//{i1} {i2}//{i2}");
                }
                (true, false) => {
                    let _ = writeln!(out, "f {i0}/{i0} {i1}/{i1} {i2}/{i2}");
                }
                (false, false) => {
                    let _ = writeln!(out, "f {i0} {i1} {i2}");
                }
            }
        }
    }
    out
}

/// Formats one Wavefront `.mtl` material entry block.
pub fn format_mtl_material(
    name: &str,
    diffuse_rgb: [f32; 3],
    opacity: f32,
    specular: f32,
    diffuse_tex_rel: Option<&str>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "newmtl {}", sanitize_obj_name(name));
    let _ = writeln!(
        out,
        "Kd {:.4} {:.4} {:.4}",
        diffuse_rgb[0].clamp(0.0, 1.0),
        diffuse_rgb[1].clamp(0.0, 1.0),
        diffuse_rgb[2].clamp(0.0, 1.0)
    );
    let _ = writeln!(out, "Ka 0.0500 0.0500 0.0500");
    let _ = writeln!(
        out,
        "Ks {:.4} {:.4} {:.4}",
        specular.clamp(0.0, 1.0),
        specular.clamp(0.0, 1.0),
        specular.clamp(0.0, 1.0)
    );
    let _ = writeln!(out, "d {:.4}", opacity.clamp(0.0, 1.0));
    let _ = writeln!(out, "illum 2");
    if let Some(tex) = diffuse_tex_rel {
        let _ = writeln!(out, "map_Kd {tex}");
    }
    let _ = writeln!(out);
    out
}

/// Exports a [`DecodedMesh`] as ASCII Stanford `.ply` with normals and UVs.
pub fn export_ply(mesh: &DecodedMesh) -> String {
    let has_norm = mesh.normals.len() == mesh.positions.len();
    let has_uv = mesh.uvs.len() == mesh.positions.len();
    let tri_count = mesh.indices.len() / 3;

    let mut out = String::new();
    out.push_str("ply\n");
    out.push_str("format ascii 1.0\n");
    out.push_str("comment Exported by devx-codecs (DevX Unity Rust)\n");
    let _ = writeln!(out, "element vertex {}", mesh.positions.len());
    out.push_str("property float x\nproperty float y\nproperty float z\n");
    if has_norm {
        out.push_str("property float nx\nproperty float ny\nproperty float nz\n");
    }
    if has_uv {
        out.push_str("property float s\nproperty float t\n");
    }
    let _ = writeln!(out, "element face {tri_count}");
    out.push_str("property list uchar int vertex_indices\n");
    out.push_str("end_header\n");

    for (idx, p) in mesh.positions.iter().enumerate() {
        let _ = write!(out, "{:.6} {:.6} {:.6}", p[0], p[1], p[2]);
        if has_norm {
            let n = mesh.normals[idx];
            let _ = write!(out, " {:.6} {:.6} {:.6}", n[0], n[1], n[2]);
        }
        if has_uv {
            let uv = mesh.uvs[idx];
            let _ = write!(out, " {:.6} {:.6}", uv[0], uv[1]);
        }
        out.push('\n');
    }
    for tri in mesh.indices.as_chunks::<3>().0 {
        let _ = writeln!(out, "3 {} {} {}", tri[0], tri[1], tri[2]);
    }
    out
}

/// Sanitized OBJ/MTL identifier name helper.
pub fn sanitize_obj_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if clean.is_empty() {
        "mesh".to_owned()
    } else {
        clean
    }
}

/// Exports a [`DecodedMesh`] as a binary little-endian `.stl` file.
pub fn export_stl_binary(mesh: &DecodedMesh) -> Vec<u8> {
    let tri_count = mesh.indices.len() / 3;
    let mut out = Vec::with_capacity(84 + tri_count * 50);
    let mut header = [0u8; 80];
    let tag = b"Exported by DevX Unity Rust";
    header[..tag.len()].copy_from_slice(tag);
    out.extend_from_slice(&header);
    out.extend_from_slice(&(tri_count as u32).to_le_bytes());

    for tri in mesh.indices.as_chunks::<3>().0 {
        let a = mesh.positions[tri[0] as usize];
        let b = mesh.positions[tri[1] as usize];
        let c = mesh.positions[tri[2] as usize];
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let mut n = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len > 1e-6 {
            n[0] /= len;
            n[1] /= len;
            n[2] /= len;
        }
        for v in [n, a, b, c] {
            for f in v {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    out
}

/// Exports a [`DecodedMesh`] as a standalone glTF 2.0 binary (`.glb`) container.
pub fn export_glb(mesh: &DecodedMesh) -> CodecResult<Vec<u8>> {
    if mesh.positions.is_empty() || mesh.indices.is_empty() {
        return Err(CodecError::MalformedData(
            "cannot export empty mesh to GLB".to_owned(),
        ));
    }

    let mut bin = Vec::new();

    // 1. POSITION
    let pos_off = bin.len();
    for p in &mesh.positions {
        for f in p {
            bin.extend_from_slice(&f.to_le_bytes());
        }
    }
    let pos_len = bin.len() - pos_off;

    // 2. NORMAL
    let norm_off = bin.len();
    for n in &mesh.normals {
        for f in n {
            bin.extend_from_slice(&f.to_le_bytes());
        }
    }
    let norm_len = bin.len() - norm_off;

    // 3. TEXCOORD_0
    let uv_off = bin.len();
    for uv in &mesh.uvs {
        for f in uv {
            bin.extend_from_slice(&f.to_le_bytes());
        }
    }
    let uv_len = bin.len() - uv_off;

    // 4. INDICES (u32)
    let idx_off = bin.len();
    for &i in &mesh.indices {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    let idx_len = bin.len() - idx_off;

    while bin.len() % 4 != 0 {
        bin.push(0);
    }

    let mut root = json::Root {
        asset: json::Asset {
            generator: Some("devx-codecs (DevX Unity Rust)".to_owned()),
            version: "2.0".to_owned(),
            ..Default::default()
        },
        ..Default::default()
    };

    let buffer_idx = json::Index::push(
        &mut root.buffers,
        json::Buffer {
            byte_length: json::validation::USize64::from(bin.len()),
            uri: None,
            name: None,
            extensions: None,
            extras: Default::default(),
        },
    );

    let push_view = |views: &mut Vec<json::buffer::View>,
                     offset: usize,
                     len: usize,
                     target: json::buffer::Target| {
        json::Index::push(
            views,
            json::buffer::View {
                buffer: buffer_idx,
                byte_length: json::validation::USize64::from(len),
                byte_offset: Some(json::validation::USize64::from(offset)),
                byte_stride: None,
                name: None,
                target: Some(Checked::Valid(target)),
                extensions: None,
                extras: Default::default(),
            },
        )
    };

    let pos_view = push_view(
        &mut root.buffer_views,
        pos_off,
        pos_len,
        json::buffer::Target::ArrayBuffer,
    );
    let norm_view = push_view(
        &mut root.buffer_views,
        norm_off,
        norm_len,
        json::buffer::Target::ArrayBuffer,
    );
    let uv_view = push_view(
        &mut root.buffer_views,
        uv_off,
        uv_len,
        json::buffer::Target::ArrayBuffer,
    );
    let idx_view = push_view(
        &mut root.buffer_views,
        idx_off,
        idx_len,
        json::buffer::Target::ElementArrayBuffer,
    );

    let pos_acc = json::Index::push(
        &mut root.accessors,
        json::Accessor {
            buffer_view: Some(pos_view),
            byte_offset: None,
            count: json::validation::USize64::from(mesh.positions.len()),
            component_type: Checked::Valid(json::accessor::GenericComponentType(
                json::accessor::ComponentType::F32,
            )),
            extensions: None,
            extras: Default::default(),
            type_: Checked::Valid(json::accessor::Type::Vec3),
            min: Some(json!(mesh.bounds_min)),
            max: Some(json!(mesh.bounds_max)),
            normalized: false,
            name: None,
            sparse: None,
        },
    );
    let norm_acc = json::Index::push(
        &mut root.accessors,
        json::Accessor {
            buffer_view: Some(norm_view),
            byte_offset: None,
            count: json::validation::USize64::from(mesh.normals.len()),
            component_type: Checked::Valid(json::accessor::GenericComponentType(
                json::accessor::ComponentType::F32,
            )),
            extensions: None,
            extras: Default::default(),
            type_: Checked::Valid(json::accessor::Type::Vec3),
            min: None,
            max: None,
            normalized: false,
            name: None,
            sparse: None,
        },
    );
    let uv_acc = json::Index::push(
        &mut root.accessors,
        json::Accessor {
            buffer_view: Some(uv_view),
            byte_offset: None,
            count: json::validation::USize64::from(mesh.uvs.len()),
            component_type: Checked::Valid(json::accessor::GenericComponentType(
                json::accessor::ComponentType::F32,
            )),
            extensions: None,
            extras: Default::default(),
            type_: Checked::Valid(json::accessor::Type::Vec2),
            min: None,
            max: None,
            normalized: false,
            name: None,
            sparse: None,
        },
    );
    let idx_acc = json::Index::push(
        &mut root.accessors,
        json::Accessor {
            buffer_view: Some(idx_view),
            byte_offset: None,
            count: json::validation::USize64::from(mesh.indices.len()),
            component_type: Checked::Valid(json::accessor::GenericComponentType(
                json::accessor::ComponentType::U32,
            )),
            extensions: None,
            extras: Default::default(),
            type_: Checked::Valid(json::accessor::Type::Scalar),
            min: None,
            max: None,
            normalized: false,
            name: None,
            sparse: None,
        },
    );

    let mut attributes = BTreeMap::new();
    attributes.insert(Checked::Valid(json::mesh::Semantic::Positions), pos_acc);
    attributes.insert(Checked::Valid(json::mesh::Semantic::Normals), norm_acc);
    attributes.insert(Checked::Valid(json::mesh::Semantic::TexCoords(0)), uv_acc);

    let mesh_idx = json::Index::push(
        &mut root.meshes,
        json::Mesh {
            name: Some(mesh.name.clone()),
            extensions: None,
            extras: Default::default(),
            primitives: vec![json::mesh::Primitive {
                attributes,
                extensions: None,
                extras: Default::default(),
                indices: Some(idx_acc),
                material: None,
                mode: Checked::Valid(json::mesh::Mode::Triangles),
                targets: None,
            }],
            weights: None,
        },
    );
    let node_idx = json::Index::push(
        &mut root.nodes,
        json::Node {
            mesh: Some(mesh_idx),
            name: Some(mesh.name.clone()),
            ..Default::default()
        },
    );
    let scene_idx = json::Index::push(
        &mut root.scenes,
        json::Scene {
            name: None,
            extensions: None,
            extras: Default::default(),
            nodes: vec![node_idx],
        },
    );
    root.scene = Some(scene_idx);
    pack_glb(&root, bin)
}

/// Packs a `gltf_json::Root` and a single binary buffer into a valid GLB 2.0 byte container.
pub(crate) fn pack_glb(root: &json::Root, mut bin: Vec<u8>) -> CodecResult<Vec<u8>> {
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }

    let mut json_bytes = serde_json::to_vec(root)
        .map_err(|e| CodecError::MalformedData(format!("GLB JSON encode failed: {e}")))?;
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' ');
    }

    let total_len = 12 + 8 + json_bytes.len() + 8 + bin.len();
    let mut glb = Vec::with_capacity(total_len);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total_len as u32).to_le_bytes());

    glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes()); // "JSON"
    glb.extend_from_slice(&json_bytes);

    glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x004E_4942u32.to_le_bytes()); // "BIN\0"
    glb.extend_from_slice(&bin);

    Ok(glb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_decodes_known_values() {
        assert!((f16_to_f32(0x3C00) - 1.0).abs() < 1e-6);
        assert!((f16_to_f32(0xC000) + 2.0).abs() < 1e-6);
        assert_eq!(f16_to_f32(0x0000), 0.0);
    }

    #[test]
    fn exports_obj_stl_and_glb() {
        let mut vb = Vec::new();
        for p in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for f in p {
                vb.extend_from_slice(&f.to_le_bytes());
            }
        }
        let channels = vec![ChannelInfo {
            stream: 0,
            offset: 0,
            format: 0,
            dimension: 3,
        }];
        let layouts = compute_stream_layouts(&channels, 3, &[]);
        let pos = read_vec3_channel(&channels, 0, 3, &layouts, &vb);
        assert_eq!(pos.len(), 3);
        let mesh = finalize_decoded_mesh(
            "tri".into(),
            pos,
            Vec::new(),
            Vec::new(),
            vec![0, 1, 2],
            vec![SubMeshSlice {
                first_index: 0,
                index_count: 3,
                topology: 0,
            }],
        );
        let obj = export_obj(&mesh);
        assert!(obj.contains("v 1.000000 0.000000 0.000000"));
        assert!(obj.contains("f 1/1/1 2/2/2 3/3/3"));

        let stl = export_stl_binary(&mesh);
        assert_eq!(stl.len(), 84 + 50);

        let glb = export_glb(&mesh).expect("glb export");
        assert!(glb.starts_with(b"glTF"));
        assert_eq!(glb.len() % 4, 0);

        let ply = export_ply(&mesh);
        assert!(ply.starts_with("ply\nformat ascii 1.0\n"));
        assert!(ply.contains("element vertex 3"));
        assert!(ply.contains("3 0 1 2"));

        let obj_mtl = export_obj_with_mtl(&mesh, "hero.mtl", &["HeroMat".to_owned()]);
        assert!(obj_mtl.contains("mtllib hero.mtl"));
        assert!(obj_mtl.contains("usemtl HeroMat"));
        let mtl = format_mtl_material("HeroMat", [0.8, 0.2, 0.1], 1.0, 0.3, Some("tex.png"));
        assert!(mtl.contains("newmtl HeroMat"));
        assert!(mtl.contains("map_Kd tex.png"));
    }

    #[test]
    fn normalizes_skin_weights_and_handles_zero_sum() {
        let mut weights = vec![[2.0, 2.0, -0.5, f32::NAN], [0.0, 0.0, 0.0, 0.0]];
        let mut joints = vec![[3, 7, 9, 11], [5, 0, 0, 0]];
        normalize_skin_weights(&mut weights, &mut joints);
        assert!((weights[0][0] - 0.5).abs() < 1e-6);
        assert!((weights[0][1] - 0.5).abs() < 1e-6);
        assert_eq!(weights[0][2], 0.0);
        assert_eq!(joints[0], [3, 7, 0, 0]);
        assert_eq!(weights[1], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(joints[1], [0, 0, 0, 0]);
    }

    #[test]
    fn decodes_optimized_one_influence_skin_and_rejects_incomplete_indices() {
        let mut matrix = indexmap::IndexMap::new();
        for row in 0..4 {
            for col in 0..4 {
                matrix.insert(format!("e{row}{col}"), UnityValue::Float(if row == col { 1.0 } else { 0.0 }));
            }
        }
        let mut props = indexmap::IndexMap::new();
        props.insert("m_BindPose".into(), UnityValue::Array(vec![
            UnityValue::Object(matrix.clone()), UnityValue::Object(matrix),
        ]));
        let mut channels = vec![ChannelInfo { stream: 0, offset: 0, format: 0, dimension: 0 }; 14];
        channels[13] = ChannelInfo { stream: 0, offset: 0, format: 10, dimension: 1 };
        let layouts = compute_stream_layouts(&channels, 3, &[]);
        let bytes: Vec<u8> = [0u32, 1, 0].into_iter().flat_map(u32::to_le_bytes).collect();
        let skin = extract_mesh_skin(&props, &channels, false, 3, &layouts, &bytes).unwrap();
        assert_eq!(skin.joints, [[0, 0, 0, 0], [1, 0, 0, 0], [0, 0, 0, 0]]);
        assert_eq!(skin.weights, [[1.0, 0.0, 0.0, 0.0]; 3]);
        let truncated = extract_mesh_skin(&props, &channels, false, 3, &layouts, &bytes[..8]).unwrap();
        assert!(truncated.joints.is_empty());
        assert!(truncated.weights.is_empty());
        channels[13].dimension = 2;
        let ambiguous = extract_mesh_skin(&props, &channels, false, 3, &layouts, &bytes).unwrap();
        assert!(ambiguous.weights.is_empty());
    }

    #[test]
    fn decodes_and_applies_unity_m_shapes_blend_shapes() {
        let mut props = indexmap::IndexMap::new();
        let mut vtx_obj = indexmap::IndexMap::new();
        let mut dp = indexmap::IndexMap::new();
        dp.insert("x".into(), UnityValue::Float(0.0));
        dp.insert("y".into(), UnityValue::Float(0.4));
        dp.insert("z".into(), UnityValue::Float(0.0));
        let mut dn = indexmap::IndexMap::new();
        dn.insert("x".into(), UnityValue::Float(1.0));
        dn.insert("y".into(), UnityValue::Float(0.0));
        dn.insert("z".into(), UnityValue::Float(0.0));
        vtx_obj.insert("vertex".into(), UnityValue::Object(dp));
        vtx_obj.insert("normal".into(), UnityValue::Object(dn));
        vtx_obj.insert("index".into(), UnityValue::Integer(1));

        let mut shape_obj = indexmap::IndexMap::new();
        shape_obj.insert("firstVertex".into(), UnityValue::Integer(0));
        shape_obj.insert("vertexCount".into(), UnityValue::Integer(1));
        shape_obj.insert("hasNormals".into(), UnityValue::Bool(true));

        let mut ch_obj = indexmap::IndexMap::new();
        ch_obj.insert("name".into(), UnityValue::String("Smile".into()));
        ch_obj.insert("frameIndex".into(), UnityValue::Integer(0));
        ch_obj.insert("frameCount".into(), UnityValue::Integer(1));

        let mut m_shapes = indexmap::IndexMap::new();
        m_shapes.insert("vertices".into(), UnityValue::Array(vec![UnityValue::Object(vtx_obj)]));
        m_shapes.insert("shapes".into(), UnityValue::Array(vec![UnityValue::Object(shape_obj)]));
        m_shapes.insert("channels".into(), UnityValue::Array(vec![UnityValue::Object(ch_obj)]));
        props.insert("m_Shapes".into(), UnityValue::Object(m_shapes));

        let extracted = extract_mesh_blend_shapes(&props, 3);
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0].name, "Smile");
        assert_eq!(extracted[0].vertex_indices, vec![1]);

        let base_pos = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let base_nrm = [[0.0, 0.0, 1.0]; 3];
        let (pos, nrm) = apply_blend_shapes(&base_pos, &base_nrm, &extracted, &[0.5]);
        assert!((pos[1][1] - 0.2).abs() < 1e-6);
        assert!(nrm[1][0] > 0.3);
    }
}
