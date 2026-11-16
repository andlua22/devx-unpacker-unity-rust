//! Decoders for Unity `Material` (`class_id == 21`) and `Cubemap` (`class_id == 89`).

use crate::texture::{decode_texture_bytes, texture_format_name};
use crate::{CodecError, CodecResult};
use devx_plugin_api::DecodedImage;
use devx_unity_io::{
    ResourceStreamStore, UnityValue, extract_bytes_field, extract_i32_field,
    extract_i64_field, sanitize_unity_version,
};
use image::{ImageBuffer, Rgba, imageops};
use serde::{Deserialize, Serialize};
use unity_asset_binary::object::UnityObject;

/// Material alpha rendering mode inferred from Unity shader properties & render queue.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MaterialAlphaMode {
    Opaque,
    Mask { cutoff: f32 },
    Blend,
}

/// Single texture slot from `Material.m_SavedProperties.m_TexEnvs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialTextureSlot {
    pub property_name: String,
    pub texture_file_id: i32,
    pub texture_path_id: i64,
    pub scale: [f32; 2],
    pub offset: [f32; 2],
}

/// Decoded Unity `Material` (`class_id == 21`) properties and texture bindings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedMaterial {
    pub name: String,
    pub shader_file_id: i32,
    pub shader_path_id: i64,
    pub render_queue: i32,
    pub valid_keywords: Vec<String>,
    pub string_tags: Vec<(String, String)>,
    pub textures: Vec<MaterialTextureSlot>,
    pub colors: Vec<(String, [f32; 4])>,
    pub floats: Vec<(String, f32)>,
    pub ints: Vec<(String, i32)>,
}

impl DecodedMaterial {
    /// Returns the primary albedo/base-color texture slot if bound (`m_PathID != 0`).
    pub fn albedo_slot(&self) -> Option<&MaterialTextureSlot> {
        self.find_texture_slot(&[
            "_BaseMap",
            "_MainTex",
            "_Albedo",
            "_AlbedoMap",
            "_DiffuseTex",
            "_Diffuse",
        ])
        .or_else(|| self.textures.iter().find(|t| t.texture_path_id != 0))
    }

    /// Returns a tangent-space normal map slot if bound.
    pub fn normal_slot(&self) -> Option<&MaterialTextureSlot> {
        self.find_texture_slot(&["_BumpMap", "_NormalMap", "_DetailNormalMap"])
    }

    /// Returns a metallic/smoothness/roughness texture slot if bound.
    pub fn metallic_gloss_slot(&self) -> Option<&MaterialTextureSlot> {
        self.find_texture_slot(&["_MetallicGlossMap", "_MaskMap", "_SpecGlossMap"])
    }

    /// Returns an emission map slot if bound.
    pub fn emission_slot(&self) -> Option<&MaterialTextureSlot> {
        self.find_texture_slot(&["_EmissionMap", "_EmissiveMap"])
    }

    /// Returns an ambient occlusion map slot if bound.
    pub fn occlusion_slot(&self) -> Option<&MaterialTextureSlot> {
        self.find_texture_slot(&["_OcclusionMap", "_AOMap"])
    }

    pub fn find_texture_slot(&self, names: &[&str]) -> Option<&MaterialTextureSlot> {
        for cand in names {
            if let Some(slot) = self
                .textures
                .iter()
                .find(|s| s.texture_path_id != 0 && s.property_name.eq_ignore_ascii_case(cand))
            {
                return Some(slot);
            }
        }
        None
    }

    pub fn find_color(&self, names: &[&str]) -> Option<[f32; 4]> {
        for cand in names {
            if let Some((_, c)) = self
                .colors
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(cand))
            {
                return Some(*c);
            }
        }
        None
    }

    pub fn find_float(&self, names: &[&str]) -> Option<f32> {
        for cand in names {
            if let Some((_, v)) = self
                .floats
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(cand))
            {
                return Some(*v);
            }
        }
        None
    }

    pub fn base_color(&self) -> [f32; 4] {
        self.find_color(&["_BaseColor", "_Color", "_MainColor"])
            .unwrap_or([1.0, 1.0, 1.0, 1.0])
    }

    /// Returns true when the Unity material actually has active non-black emission enabled
    /// (preventing unused `_EmissionMap` mask textures from making opaque meshes glow).
    pub fn has_active_emission(&self) -> bool {
        let has_keyword = self.valid_keywords.iter().any(|k| k == "_EMISSION");
        if self.find_float(&["_EmissionEnabled"]).is_some_and(|v| v <= 0.0) && !has_keyword {
            return false;
        }
        match self.find_color(&["_EmissionColor", "_EmissiveColor"]) {
            Some(c) => c[0] > 0.001 || c[1] > 0.001 || c[2] > 0.001,
            None => has_keyword,
        }
    }

    pub fn emissive_color(&self) -> Option<[f32; 3]> {
        if !self.has_active_emission() {
            return None;
        }
        let c = self.find_color(&["_EmissionColor", "_EmissiveColor"])?;
        Some([
            c[0].clamp(0.0, 1.0),
            c[1].clamp(0.0, 1.0),
            c[2].clamp(0.0, 1.0),
        ])
    }

    pub fn metallic_factor(&self) -> f32 {
        self.find_float(&["_Metallic"]).unwrap_or(0.0).clamp(0.0, 1.0)
    }

    pub fn roughness_factor(&self) -> f32 {
        if let Some(r) = self.find_float(&["_Roughness"]) {
            return r.clamp(0.05, 1.0);
        }
        if let Some(s) = self.find_float(&["_Smoothness", "_Glossiness", "_GlossMapScale"]) {
            return (1.0 - s).clamp(0.05, 1.0);
        }
        0.7
    }

    pub fn alpha_mode(&self) -> MaterialAlphaMode {
        let render_type = self
            .string_tags
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("RenderType"))
            .map(|(_, v)| v.to_ascii_lowercase());

        if self
            .valid_keywords
            .iter()
            .any(|k| k == "_ALPHATEST_ON" || k == "_ALPHACLIP_ON")
            || render_type.as_deref() == Some("transparentcutout")
            || (self.render_queue >= 2400 && self.render_queue < 2600)
        {
            let cutoff = self
                .find_float(&["_Cutoff", "_AlphaCutoff"])
                .unwrap_or(0.5)
                .clamp(0.0, 1.0);
            return MaterialAlphaMode::Mask { cutoff };
        }
        if self
            .valid_keywords
            .iter()
            .any(|k| k == "_ALPHABLEND_ON" || k == "_ALPHAPREMULTIPLY_ON")
            || self.find_float(&["_Surface"]).is_some_and(|v| v >= 0.5)
            || render_type.as_deref() == Some("transparent")
            || self.render_queue >= 2900
        {
            return MaterialAlphaMode::Blend;
        }
        MaterialAlphaMode::Opaque
    }

    pub fn double_sided(&self) -> bool {
        self.find_float(&["_Cull"]).is_some_and(|v| v <= 0.5)
    }
}

/// Decodes a Unity `Material` (`class_id == 21`).
pub fn decode_material(obj: &UnityObject) -> CodecResult<DecodedMaterial> {
    let props = obj.class.properties();
    let name = props
        .get("m_Name")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .or_else(|| obj.name())
        .unwrap_or_else(|| "Material".to_owned());

    let (shader_file_id, shader_path_id) = props
        .get("m_Shader")
        .and_then(extract_pptr_pair)
        .unwrap_or((0, 0));

    let render_queue = props
        .get("m_CustomRenderQueue")
        .and_then(extract_i32_field)
        .unwrap_or(-1);

    let valid_keywords = props
        .get("m_ValidKeywords")
        .and_then(UnityValue::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let string_tags = props
        .get("stringTagMap")
        .map(parse_kv_pairs)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| Some((k, v.as_str()?.to_owned())))
        .collect();

    let mut textures = Vec::new();
    let mut colors = Vec::new();
    let mut floats = Vec::new();
    let mut ints = Vec::new();

    if let Some(saved) = props.get("m_SavedProperties").and_then(UnityValue::as_object) {
        if let Some(tex_envs) = saved.get("m_TexEnvs") {
            for (prop_name, env) in parse_kv_pairs(tex_envs) {
                if let Some(env_map) = env.as_object() {
                    let (fid, pid) = env_map
                        .get("m_Texture")
                        .and_then(extract_pptr_pair)
                        .unwrap_or((0, 0));
                    let scale = env_map
                        .get("m_Scale")
                        .and_then(extract_vec2)
                        .unwrap_or([1.0, 1.0]);
                    let offset = env_map
                        .get("m_Offset")
                        .and_then(extract_vec2)
                        .unwrap_or([0.0, 0.0]);
                    textures.push(MaterialTextureSlot {
                        property_name: prop_name,
                        texture_file_id: fid,
                        texture_path_id: pid,
                        scale,
                        offset,
                    });
                }
            }
        }
        if let Some(col_arr) = saved.get("m_Colors") {
            for (prop_name, val) in parse_kv_pairs(col_arr) {
                if let Some(rgba) = extract_color(val) {
                    colors.push((prop_name, rgba));
                }
            }
        }
        if let Some(flt_arr) = saved.get("m_Floats") {
            for (prop_name, val) in parse_kv_pairs(flt_arr) {
                if let Some(f) = val.as_f64() {
                    floats.push((prop_name, f as f32));
                }
            }
        }
        if let Some(int_arr) = saved.get("m_Ints") {
            for (prop_name, val) in parse_kv_pairs(int_arr) {
                if let Some(i) = extract_i32_field(val) {
                    ints.push((prop_name, i));
                }
            }
        }
    }

    Ok(DecodedMaterial {
        name,
        shader_file_id,
        shader_path_id,
        render_queue,
        valid_keywords,
        string_tags,
        textures,
        colors,
        floats,
        ints,
    })
}

/// Decoded Unity `Cubemap` (`class_id == 89`) laid out as a horizontal cross + individual faces.
#[derive(Debug, Clone)]
pub struct DecodedCubemap {
    pub name: String,
    pub face_size: u32,
    pub format_name: String,
    pub cross_image: DecodedImage,
    pub faces: Vec<DecodedImage>,
}

/// Decodes a Unity `Cubemap` (`class_id == 89`) into 6 faces and a 4x3 horizontal cross panorama.
pub fn decode_cubemap(
    obj: &UnityObject,
    unity_version: &str,
    streams: &ResourceStreamStore,
) -> CodecResult<DecodedCubemap> {
    let _ = sanitize_unity_version(unity_version);
    let props = obj.class.properties();
    let name = props
        .get("m_Name")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .or_else(|| obj.name())
        .unwrap_or_else(|| "Cubemap".to_owned());

    let width = props
        .get("m_Width")
        .and_then(extract_i32_field)
        .unwrap_or(0)
        .max(0) as u32;
    let height = props
        .get("m_Height")
        .and_then(extract_i32_field)
        .unwrap_or(width as i32)
        .max(0) as u32;
    let image_count = props
        .get("m_ImageCount")
        .and_then(extract_i32_field)
        .unwrap_or(6)
        .clamp(1, 6) as usize;
    let format_id = props
        .get("m_TextureFormat")
        .and_then(extract_i32_field)
        .unwrap_or(4);
    let fmt_name = texture_format_name(format_id).to_owned();
    let mip_count = props
        .get("m_MipCount")
        .and_then(extract_i32_field)
        .unwrap_or(1)
        .max(1) as u32;

    if width == 0 || height == 0 {
        return Err(CodecError::MalformedData(format!(
            "Cubemap '{name}' has invalid dimensions {width}x{height}"
        )));
    }

    let mut raw = props
        .get("image data")
        .and_then(extract_bytes_field)
        .unwrap_or_default();

    if raw.is_empty()
        && let Some(sd) = props.get("m_StreamData").and_then(UnityValue::as_object)
        && let Some(path) = sd.get("path").and_then(|v| v.as_str())
        && !path.is_empty()
    {
        let offset = sd.get("offset").and_then(|v| v.as_i64()).unwrap_or(0).max(0) as u64;
        let size = sd.get("size").and_then(|v| v.as_i64()).unwrap_or(0).max(0) as u32;
        if size > 0 {
            raw = streams
                .read_slice(path, offset, size)
                .ok_or_else(|| CodecError::MissingStream(path.to_owned()))?
                .to_vec();
        }
    }

    if raw.is_empty() {
        return Err(CodecError::MalformedData(format!(
            "Cubemap '{name}' ({width}x{height}, {fmt_name}) contains no pixel payload"
        )));
    }

    let face_stride = (raw.len() / image_count).max(1);
    let mut faces = Vec::with_capacity(image_count);
    for face_idx in 0..image_count {
        let start = face_idx * face_stride;
        let slice = &raw[start..raw.len()];
        let mut rgba = decode_texture_bytes(format_id, width, height, slice)?;
        imageops::flip_vertical_in_place(&mut rgba);
        faces.push(DecodedImage {
            width,
            height,
            rgba8: rgba.into_raw(),
            format_name: fmt_name.clone(),
            mip_count,
        });
    }

    // Assemble standard 4x3 horizontal cross:
    //   [  ][+Y][  ][  ]   face 2 (+Y) at grid (1, 0)
    //   [-X][+Z][+X][-Z]   faces 1,4,0,5 at row 1
    //   [  ][-Y][  ][  ]   face 3 (-Y) at grid (1, 2)
    let cross_w = width * 4;
    let cross_h = height * 3;
    let mut cross: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(cross_w, cross_h, Rgba([22, 24, 30, 255]));
    let layout = [
        (2usize, 1u32, 0u32),
        (1, 0, 1),
        (4, 1, 1),
        (0, 2, 1),
        (5, 3, 1),
        (3, 1, 2),
    ];
    for (face_idx, gx, gy) in layout {
        if let Some(face) = faces.get(face_idx)
            && let Some(face_img) =
                ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(face.width, face.height, face.rgba8.clone())
        {
            imageops::overlay(&mut cross, &face_img, i64::from(gx * width), i64::from(gy * height));
        }
    }

    Ok(DecodedCubemap {
        name,
        face_size: width,
        format_name: fmt_name,
        cross_image: DecodedImage {
            width: cross_w,
            height: cross_h,
            rgba8: cross.into_raw(),
            format_name: format!("Cubemap Cross ({image_count} faces)"),
            mip_count,
        },
        faces,
    })
}

fn parse_kv_pairs(val: &UnityValue) -> Vec<(String, &UnityValue)> {
    let Some(arr) = val.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr {
        match item {
            UnityValue::Array(pair) if pair.len() >= 2 => {
                if let Some(k) = pair[0].as_str() {
                    out.push((k.to_owned(), &pair[1]));
                }
            }
            UnityValue::Object(map) => {
                if let (Some(k), Some(v)) = (
                    map.get("first").and_then(|s| s.as_str()),
                    map.get("second"),
                ) {
                    out.push((k.to_owned(), v));
                }
            }
            _ => {}
        }
    }
    out
}

fn extract_pptr_pair(val: &UnityValue) -> Option<(i32, i64)> {
    let m = val.as_object()?;
    let fid = m.get("m_FileID").and_then(extract_i32_field)?;
    let pid = m.get("m_PathID").and_then(extract_i64_field)?;
    Some((fid, pid))
}

fn extract_vec2(val: &UnityValue) -> Option<[f32; 2]> {
    let m = val.as_object()?;
    Some([
        m.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        m.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
    ])
}

fn extract_color(val: &UnityValue) -> Option<[f32; 4]> {
    let m = val.as_object()?;
    Some([
        m.get("r").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
        m.get("g").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
        m.get("b").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
        m.get("a").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mat_defaults_are_sane() {
        let mat = DecodedMaterial {
            name: "test".into(),
            shader_file_id: 0,
            shader_path_id: 0,
            render_queue: 2000,
            valid_keywords: vec![],
            string_tags: vec![],
            textures: vec![],
            colors: vec![],
            floats: vec![("_Smoothness".into(), 0.2)],
            ints: vec![],
        };
        assert_eq!(mat.base_color(), [1.0, 1.0, 1.0, 1.0]);
        assert!((mat.roughness_factor() - 0.8).abs() < 1e-5);
        assert_eq!(mat.alpha_mode(), MaterialAlphaMode::Opaque);
    }

    #[test]
    fn disabled_or_black_emission_ignores_mask_slot() {
        let mat = DecodedMaterial {
            name: "skin_head".into(),
            shader_file_id: 0,
            shader_path_id: 0,
            render_queue: 2000,
            valid_keywords: vec!["_NORMALMAP".into()],
            string_tags: vec![],
            textures: vec![MaterialTextureSlot {
                property_name: "_EmissionMap".into(),
                texture_file_id: 0,
                texture_path_id: 42,
                scale: [1.0, 1.0],
                offset: [0.0, 0.0],
            }],
            colors: vec![("_EmissionColor".into(), [0.0, 0.0, 0.0, 1.0])],
            floats: vec![("_EmissionEnabled".into(), 0.0)],
            ints: vec![],
        };
        assert!(!mat.has_active_emission());
        assert_eq!(mat.emissive_color(), None);

        let lit = DecodedMaterial {
            valid_keywords: vec!["_EMISSION".into()],
            colors: vec![("_EmissionColor".into(), [0.4, 0.8, 1.0, 1.0])],
            ..mat
        };
        assert!(lit.has_active_emission());
        assert_eq!(lit.emissive_color(), Some([0.4, 0.8, 1.0]));
    }
}
