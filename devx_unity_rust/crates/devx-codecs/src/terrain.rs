//! Pure-Rust decoder and 3D mesh + hypsometric preview generator for Unity
//! `TerrainData` (`class_id == 156`).

use crate::{CodecError, CodecResult};
use devx_plugin_api::{DecodedImage, DecodedMesh, SubMeshSlice};
use devx_unity_io::{UnityObject, UnityValue, extract_i32_field};

/// Summary of one splat / `TerrainLayer` slot configured on a `TerrainData`.
#[derive(Debug, Clone)]
pub struct TerrainLayerSummary {
    pub index: usize,
    pub label: String,
    pub tile_size: [f32; 2],
    pub tile_offset: [f32; 2],
}

/// Decoded metadata and heightfield payload from a Unity `TerrainData` (`class_id == 156`).
#[derive(Debug, Clone)]
pub struct DecodedTerrainData {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub size: [f32; 3],
    /// Normalized elevation values in `[0.0, 1.0]` stored in row-major (`y * width + x`) order.
    pub heights: Vec<f32>,
    pub min_height: f32,
    pub max_height: f32,
    pub hole_count: usize,
    pub layers: Vec<TerrainLayerSummary>,
    pub alpha_texture_count: usize,
    pub detail_prototype_count: usize,
    pub tree_prototype_count: usize,
    pub tree_instance_count: usize,
}

impl DecodedTerrainData {
    /// Generates a bounded triangle grid [`DecodedMesh`] centered at XZ origin and a
    /// matching elevation/slope-shaded [`DecodedImage`] texture for 3D viewport rendering.
    pub fn build_preview_mesh_and_albedo(&self, max_grid: u32) -> (DecodedMesh, DecodedImage) {
        let cap = max_grid.max(4) as usize;
        let w = (self.width as usize).max(2);
        let h = (self.height as usize).max(2);
        let step_x = (w / cap).max(1);
        let step_z = (h / cap).max(1);

        let mut xs: Vec<usize> = (0..w).step_by(step_x).collect();
        if *xs.last().unwrap_or(&0) != w - 1 {
            xs.push(w - 1);
        }
        let mut zs: Vec<usize> = (0..h).step_by(step_z).collect();
        if *zs.last().unwrap_or(&0) != h - 1 {
            zs.push(h - 1);
        }

        let gw = xs.len();
        let gz = zs.len();
        let sx = if self.size[0].abs() > 1e-3 { self.size[0] } else { 100.0 };
        let sy = if self.size[1].abs() > 1e-3 { self.size[1] } else { 25.0 };
        let sz = if self.size[2].abs() > 1e-3 { self.size[2] } else { 100.0 };

        let sample_h = |ix: usize, iz: usize| -> f32 {
            self.heights.get(iz * w + ix).copied().unwrap_or(0.0)
        };

        let mut positions = Vec::with_capacity(gw * gz);
        let mut normals = Vec::with_capacity(gw * gz);
        let mut uvs = Vec::with_capacity(gw * gz);
        let mut bounds_min = [f32::INFINITY; 3];
        let mut bounds_max = [f32::NEG_INFINITY; 3];

        let cell_dx = (sx / (w - 1) as f32).max(1e-4);
        let cell_dz = (sz / (h - 1) as f32).max(1e-4);

        for &iz in &zs {
            let v = iz as f32 / (h - 1) as f32;
            let z_pos = (v - 0.5) * sz;
            for &ix in &xs {
                let u = ix as f32 / (w - 1) as f32;
                let x_pos = (u - 0.5) * sx;
                let y_pos = sample_h(ix, iz) * sy;

                let hl = sample_h(ix.saturating_sub(1), iz) * sy;
                let hr = sample_h((ix + 1).min(w - 1), iz) * sy;
                let hd = sample_h(ix, iz.saturating_sub(1)) * sy;
                let hu = sample_h(ix, (iz + 1).min(h - 1)) * sy;
                let dx_h = (hr - hl) / (2.0 * cell_dx);
                let dz_h = (hu - hd) / (2.0 * cell_dz);
                let mut n = [-dx_h, 1.0, -dz_h];
                let nlen = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
                n[0] /= nlen;
                n[1] /= nlen;
                n[2] /= nlen;

                let p = [x_pos, y_pos, z_pos];
                for k in 0..3 {
                    bounds_min[k] = bounds_min[k].min(p[k]);
                    bounds_max[k] = bounds_max[k].max(p[k]);
                }
                positions.push(p);
                normals.push(n);
                uvs.push([u, v]);
            }
        }

        let mut indices = Vec::with_capacity((gw - 1) * (gz - 1) * 6);
        for row in 0..(gz - 1) {
            for col in 0..(gw - 1) {
                let a = (row * gw + col) as u32;
                let b = a + 1;
                let c = ((row + 1) * gw + col) as u32;
                let d = c + 1;
                indices.extend_from_slice(&[a, c, b, b, c, d]);
            }
        }

        // Synthesize an elevation + contour tint map matching heightfield samples.
        let tex_w = gw.min(256) as u32;
        let tex_h = gz.min(256) as u32;
        let mut rgba8 = Vec::with_capacity((tex_w * tex_h * 4) as usize);
        let span = (self.max_height - self.min_height).max(1e-4);
        for ty in 0..tex_h {
            let row = ((ty as usize) * gz / tex_h as usize).min(gz - 1);
            for tx in 0..tex_w {
                let col = ((tx as usize) * gw / tex_w as usize).min(gw - 1);
                let ix = xs[col];
                let iz = zs[row];
                let norm_h = ((sample_h(ix, iz) - self.min_height) / span).clamp(0.0, 1.0);
                let slope = 1.0 - normals[row * gw + col][1].clamp(0.0, 1.0);
                let mut rgb = hypsometric_color(norm_h, slope);
                // Subtle iso-contour bands every 10% elevation.
                if (norm_h * 10.0).fract() < 0.045 {
                    for c in &mut rgb {
                        *c = (*c as f32 * 0.84) as u8;
                    }
                }
                rgba8.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }

        let mesh = DecodedMesh {
            name: self.name.clone(),
            positions,
            normals,
            uvs,
            indices: indices.clone(),
            submeshes: vec![SubMeshSlice {
                first_index: 0,
                index_count: indices.len() as u32,
                topology: 0,
            }],
            bounds_min,
            bounds_max,
        };
        let albedo = DecodedImage {
            width: tex_w,
            height: tex_h,
            rgba8,
            format_name: "RGBA8".into(),
            mip_count: 1,
        };
        (mesh, albedo)
    }
}

/// Decodes a Unity `TerrainData` (`class_id == 156`).
pub fn decode_terrain_data(obj: &UnityObject) -> CodecResult<DecodedTerrainData> {
    let props = obj.class.properties();
    let name = props
        .get("m_Name")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .or_else(|| obj.name())
        .unwrap_or_else(|| "TerrainData".to_owned());

    let hm = props
        .get("m_Heightmap")
        .and_then(|v| v.as_object())
        .ok_or_else(|| CodecError::UnsupportedTerrain("TerrainData missing `m_Heightmap`".into()))?;

    let raw_heights = extract_height_samples(hm.get("m_Heights"));
    let res = hm
        .get("m_Resolution")
        .and_then(extract_i32_field)
        .filter(|&v| v > 0)
        .map(|v| v as u32);
    let mut width = res
        .or_else(|| hm.get("m_Width").and_then(extract_i32_field).filter(|&v| v > 0).map(|v| v as u32))
        .unwrap_or(0);
    let mut height = res
        .or_else(|| hm.get("m_Height").and_then(extract_i32_field).filter(|&v| v > 0).map(|v| v as u32))
        .unwrap_or(0);

    if (width == 0 || height == 0) && !raw_heights.is_empty() {
        let side = (raw_heights.len() as f64).sqrt().round() as u32;
        if side * side == raw_heights.len() as u32 {
            width = side;
            height = side;
        } else {
            width = raw_heights.len() as u32;
            height = 1;
        }
    }

    let size = hm
        .get("m_Scale")
        .and_then(|v| v.as_object())
        .map(|o| {
            [
                o.get("x").and_then(UnityValue::as_f64).unwrap_or(100.0) as f32,
                o.get("y").and_then(UnityValue::as_f64).unwrap_or(25.0) as f32,
                o.get("z").and_then(UnityValue::as_f64).unwrap_or(100.0) as f32,
            ]
        })
        .unwrap_or([100.0, 25.0, 100.0]);

    let (min_height, max_height) = if raw_heights.is_empty() {
        (0.0, 0.0)
    } else {
        let mut mn = f32::INFINITY;
        let mut mx = f32::NEG_INFINITY;
        for &v in &raw_heights {
            mn = mn.min(v);
            mx = mx.max(v);
        }
        (mn, mx)
    };

    let hole_count = hm
        .get("m_Holes")
        .and_then(|v| v.as_array())
        .map_or(0, |arr| {
            arr.iter()
                .filter(|v| extract_i32_field(v).unwrap_or(0) == 0)
                .count()
        });

    let splat_db = props.get("m_SplatDatabase").and_then(|v| v.as_object());
    let mut layers = Vec::new();
    if let Some(sdb) = splat_db {
        if let Some(tl) = sdb.get("m_TerrainLayers").and_then(|v| v.as_array()) {
            for (idx, item) in tl.iter().enumerate() {
                let pid = item
                    .as_object()
                    .and_then(|o| o.get("m_PathID"))
                    .and_then(extract_i32_field)
                    .unwrap_or(0);
                layers.push(TerrainLayerSummary {
                    index: idx,
                    label: format!("TerrainLayer#{idx} (pid={pid})"),
                    tile_size: [15.0, 15.0],
                    tile_offset: [0.0, 0.0],
                });
            }
        } else if let Some(splats) = sdb.get("m_Splats").and_then(|v| v.as_array()) {
            for (idx, item) in splats.iter().enumerate() {
                let Some(so) = item.as_object() else {
                    continue;
                };
                let tile_size = so
                    .get("tileSize")
                    .and_then(|v| v.as_object())
                    .map_or([15.0, 15.0], |t| {
                        [
                            t.get("x").and_then(UnityValue::as_f64).unwrap_or(15.0) as f32,
                            t.get("y").and_then(UnityValue::as_f64).unwrap_or(15.0) as f32,
                        ]
                    });
                let tile_offset = so
                    .get("tileOffset")
                    .and_then(|v| v.as_object())
                    .map_or([0.0, 0.0], |t| {
                        [
                            t.get("x").and_then(UnityValue::as_f64).unwrap_or(0.0) as f32,
                            t.get("y").and_then(UnityValue::as_f64).unwrap_or(0.0) as f32,
                        ]
                    });
                layers.push(TerrainLayerSummary {
                    index: idx,
                    label: format!("Splat#{idx}"),
                    tile_size,
                    tile_offset,
                });
            }
        }
    }

    let alpha_texture_count = splat_db
        .and_then(|s| s.get("m_AlphaTextures").and_then(|v| v.as_array()))
        .map_or(0, Vec::len);

    let detail_db = props.get("m_DetailDatabase").and_then(|v| v.as_object());
    let detail_prototype_count = detail_db
        .and_then(|d| d.get("m_DetailPrototypes").and_then(|v| v.as_array()))
        .map_or(0, Vec::len);
    let tree_prototype_count = detail_db
        .and_then(|d| d.get("m_TreePrototypes").and_then(|v| v.as_array()))
        .map_or(0, Vec::len);
    let tree_instance_count = detail_db
        .and_then(|d| d.get("m_TreeInstances").and_then(|v| v.as_array()))
        .map_or(0, Vec::len);

    Ok(DecodedTerrainData {
        name,
        width,
        height,
        size,
        heights: raw_heights,
        min_height,
        max_height,
        hole_count,
        layers,
        alpha_texture_count,
        detail_prototype_count,
        tree_prototype_count,
        tree_instance_count,
    })
}

fn extract_height_samples(val: Option<&UnityValue>) -> Vec<f32> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut nums = Vec::with_capacity(arr.len());
    let mut max_raw = 0.0f64;
    let mut has_fraction = false;
    for item in arr {
        if let Some(f) = item.as_f64() {
            let v = f.max(0.0);
            if v.fract().abs() > 1e-6 && v <= 1.05 {
                has_fraction = true;
            }
            max_raw = max_raw.max(v);
            nums.push(v);
        } else if let Some(i) = extract_i32_field(item) {
            let v = i.max(0) as f64;
            max_raw = max_raw.max(v);
            nums.push(v);
        }
    }
    if nums.is_empty() {
        return Vec::new();
    }
    let scale = if has_fraction || max_raw <= 1.05 {
        1.0
    } else if max_raw <= 32767.0 {
        1.0 / 32766.0
    } else {
        1.0 / 65535.0
    };
    nums.into_iter()
        .map(|v| (v * scale).clamp(0.0, 1.0) as f32)
        .collect()
}

fn hypsometric_color(elev: f32, slope: f32) -> [u8; 3] {
    let base = if elev < 0.25 {
        lerp_rgb([46, 98, 72], [72, 138, 82], elev / 0.25)
    } else if elev < 0.6 {
        lerp_rgb([72, 138, 82], [148, 136, 96], (elev - 0.25) / 0.35)
    } else if elev < 0.85 {
        lerp_rgb([148, 136, 96], [130, 128, 132], (elev - 0.6) / 0.25)
    } else {
        lerp_rgb([130, 128, 132], [235, 240, 246], (elev - 0.85) / 0.15)
    };
    let rock_t = (slope * 1.9).clamp(0.0, 0.85);
    lerp_rgb(base, [118, 114, 108], rock_t)
}

fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let f = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * f) as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * f) as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * f) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use unity_asset_binary::asset::types::ObjectInfo;
    use unity_asset_core::UnityClass;

    #[test]
    fn terrain_data_decodes_heightfield_and_builds_bounded_mesh() {
        let mut hm = IndexMap::new();
        hm.insert("m_Resolution".into(), UnityValue::Integer(5));
        let mut heights = Vec::with_capacity(25);
        for z in 0..5 {
            for x in 0..5 {
                let dx = x as f32 - 2.0;
                let dz = z as f32 - 2.0;
                let norm = (1.0 - (dx * dx + dz * dz) / 8.0).clamp(0.0, 1.0);
                heights.push(UnityValue::Integer((norm * 32766.0) as i64));
            }
        }
        hm.insert("m_Heights".into(), UnityValue::Array(heights));
        let mut scale = IndexMap::new();
        scale.insert("x".into(), UnityValue::Float(40.0));
        scale.insert("y".into(), UnityValue::Float(12.0));
        scale.insert("z".into(), UnityValue::Float(40.0));
        hm.insert("m_Scale".into(), UnityValue::Object(scale));

        let mut splats = IndexMap::new();
        splats.insert(
            "m_AlphaTextures".into(),
            UnityValue::Array(vec![UnityValue::Integer(1)]),
        );

        let mut class = UnityClass::new(156, "TerrainData".into(), "0".into());
        class.set("m_Name".into(), UnityValue::String("AlpineRidge".into()));
        class.set("m_Heightmap".into(), UnityValue::Object(hm));
        class.set("m_SplatDatabase".into(), UnityValue::Object(splats));
        let obj = UnityObject::from_info_and_class(ObjectInfo::new(1, 0, 0, 156, 0), class);

        let decoded = decode_terrain_data(&obj).expect("decode terrain");
        assert_eq!(decoded.name, "AlpineRidge");
        assert_eq!((decoded.width, decoded.height), (5, 5));
        assert_eq!(decoded.alpha_texture_count, 1);
        assert!(decoded.max_height > 0.9);

        let (mesh, img) = decoded.build_preview_mesh_and_albedo(16);
        assert_eq!(mesh.positions.len(), 25);
        assert_eq!(mesh.indices.len(), 4 * 4 * 6);
        assert_eq!(img.width, 5);
        assert_eq!(img.rgba8.len(), 5 * 5 * 4);
    }

    #[test]
    fn tpk_embedded_has_terrain_data_typetree() {
        fn has_node(nodes: &[unity_asset_binary::typetree::TypeTreeNode], target: &str) -> bool {
            nodes
                .iter()
                .any(|n| n.name == target || has_node(&n.children, target))
        }
        fn print_nodes(nodes: &[unity_asset_binary::typetree::TypeTreeNode], depth: usize) {
            if depth > 3 {
                return;
            }
            for n in nodes {
                println!("{}{} ({})", "  ".repeat(depth), n.name, n.type_name);
                print_nodes(&n.children, depth + 1);
            }
        }
        let reg = devx_unity_io::shared_tpk_registry();
        let tt = reg.resolve("2020.3.0f1", 156).expect("TerrainData in TPK");
        print_nodes(&tt.nodes, 0);
        assert!(has_node(&tt.nodes, "m_Heightmap"));
        assert!(has_node(&tt.nodes, "m_Heights"));
    }
}
