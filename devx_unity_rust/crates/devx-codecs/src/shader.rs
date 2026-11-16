//! Decoders and decompilers for Unity `Shader` (`class_id == 48`) objects:
//! ShaderLab `Properties`/`SubShader`/`Pass` render-state reconstruction,
//! multi-segment Unity `2019.3+` LZ4 `compressedBlob` extraction,
//! `ShaderSubProgramBlob` parsing, GLSL -> CG/HLSL rewriting,
//! SMOL-V / SPIR-V disassembly, DXBC chunk/SM4/5 inspection, and Metal source extraction.

use crate::smolv_spirv::{disassemble_spirv, extract_spirv_from_unity_subprogram};
use crate::{CodecError, CodecResult};
use devx_unity_io::extract_bytes_field;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use unity_asset_binary::object::UnityObject;
use unity_asset_core::UnityValue;

/// Classified backend bytecode kind inside a Unity shader sub-program blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderBytecodeKind {
    GlslText,
    MetalSource,
    Dxbc,
    Spirv,
    Smolv,
    Binary,
}

impl ShaderBytecodeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::GlslText => "GLSL",
            Self::MetalSource => "Metal",
            Self::Dxbc => "DXBC",
            Self::Spirv => "SPIR-V",
            Self::Smolv => "SMOL-V",
            Self::Binary => "Binary",
        }
    }
}

/// One individual compiled `ShaderSubProgram` extracted from a platform blob.
#[derive(Debug, Clone)]
pub struct DecompiledSubProgram {
    pub segment_index: usize,
    pub blob_index: usize,
    pub gpu_program_type: u32,
    pub gpu_program_name: &'static str,
    pub stage_hint: &'static str,
    pub keywords: Vec<String>,
    pub bytecode_kind: ShaderBytecodeKind,
    pub raw_size: usize,
    pub source_text: String,
    pub cg_text: Option<String>,
}

/// A decompressed platform blob extracted from `compressedBlob`.
#[derive(Debug, Clone)]
pub struct ShaderPlatformBlob {
    pub platform_id: u32,
    pub platform_name: String,
    pub decompressed_bytes: Vec<u8>,
    pub detected_kinds: Vec<ShaderBytecodeKind>,
    pub text_snippets: Vec<String>,
    pub subprograms: Vec<DecompiledSubProgram>,
}

/// High-level decoded representation of a Unity `Shader`.
#[derive(Debug, Clone)]
pub struct DecodedShader {
    pub name: String,
    pub fallback_name: Option<String>,
    pub custom_editor: Option<String>,
    pub shaderlab_text: String,
    pub decompiled_cg_text: String,
    pub total_subprograms: usize,
    pub platforms: Vec<ShaderPlatformBlob>,
}

/// Parses and reconstructs readable ShaderLab + decompiled subprograms from a Unity `Shader`.
pub fn decode_shader(obj: &UnityObject) -> CodecResult<DecodedShader> {
    let props = obj.class.properties();

    // Pre-5.5 shaders often store full ShaderLab directly in `m_Script`.
    if let Some(script) = props.get("m_Script").and_then(|v| v.as_str())
        && !script.trim().is_empty()
    {
        let name = extract_shader_header_name(script)
            .or_else(|| props.get("m_Name").and_then(|v| v.as_str()).map(str::to_owned))
            .unwrap_or_else(|| "Shader".to_owned());
        return Ok(DecodedShader {
            name,
            fallback_name: None,
            custom_editor: None,
            shaderlab_text: script.to_owned(),
            decompiled_cg_text: script.to_owned(),
            total_subprograms: 0,
            platforms: Vec::new(),
        });
    }

    if props.is_empty() {
        return Err(CodecError::MalformedData(
            "Shader object has no parsed TypeTree properties".to_owned(),
        ));
    }

    let parsed_form = props.get("m_ParsedForm").and_then(|v| v.as_object());
    let name = parsed_form
        .and_then(|pf| pf.get("m_Name"))
        .and_then(|v| v.as_str())
        .or_else(|| props.get("m_Name").and_then(|v| v.as_str()))
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .or_else(|| obj.name())
        .unwrap_or_else(|| "UnnamedShader".to_owned());

    let fallback_name = parsed_form
        .and_then(|pf| pf.get("m_FallbackName"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    let custom_editor = parsed_form
        .and_then(|pf| pf.get("m_CustomEditorName"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    let platforms = extract_platform_blobs(props);
    let total_subprograms: usize = platforms.iter().map(|p| p.subprograms.len()).sum();

    let mut shaderlab = String::new();
    let _ = writeln!(shaderlab, "// Reconstructed ShaderLab + Decompiled Programs");
    let _ = writeln!(shaderlab, "Shader \"{}\" {{", escape_shader_str(&name));

    if let Some(pf) = parsed_form {
        append_properties_block(pf, &mut shaderlab);
        append_subshaders_block(pf, &platforms, &mut shaderlab);
    }

    if let Some(fb) = &fallback_name {
        let _ = writeln!(shaderlab, "    Fallback \"{}\"", escape_shader_str(fb));
    }
    if let Some(ed) = &custom_editor {
        let _ = writeln!(shaderlab, "    CustomEditor \"{}\"", escape_shader_str(ed));
    }
    let _ = writeln!(shaderlab, "}}");

    let decompiled_cg_text = build_decompiled_cg_report(
        &name,
        &shaderlab,
        &platforms,
    );

    if !platforms.is_empty() {
        let _ = writeln!(shaderlab, "\n// Compiled Graphics API Blobs ({}):", platforms.len());
        for plat in &platforms {
            let kinds = plat
                .detected_kinds
                .iter()
                .map(|k| k.label())
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                shaderlab,
                "// - {} (id={}): {} bytes, {} subprograms [{}]",
                plat.platform_name,
                plat.platform_id,
                plat.decompressed_bytes.len(),
                plat.subprograms.len(),
                if kinds.is_empty() { "Opaque" } else { &kinds }
            );
        }
    }

    Ok(DecodedShader {
        name,
        fallback_name,
        custom_editor,
        shaderlab_text: shaderlab,
        decompiled_cg_text,
        total_subprograms,
        platforms,
    })
}

fn build_decompiled_cg_report(
    shader_name: &str,
    shaderlab_header: &str,
    platforms: &[ShaderPlatformBlob],
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "// ========================================================");
    let _ = writeln!(out, "// Decompiled Shader: {}", shader_name);
    let _ = writeln!(out, "// ========================================================\n");
    out.push_str(shaderlab_header);

    for plat in platforms {
        let _ = writeln!(
            out,
            "\n// --------------------------------------------------------\n// Platform: {} (id={}, {} subprograms)\n// --------------------------------------------------------",
            plat.platform_name,
            plat.platform_id,
            plat.subprograms.len()
        );
        if plat.subprograms.is_empty() {
            for (idx, snippet) in plat.text_snippets.iter().take(6).enumerate() {
                let _ = writeln!(out, "\n// --- Raw snippet #{idx} ---\n{snippet}");
            }
            continue;
        }
        // Group identical source bodies or cap verbose huge mega-shaders nicely while keeping all unique programs readable.
        for sp in plat.subprograms.iter().take(64) {
            let kw = if sp.keywords.is_empty() {
                "<default>".to_owned()
            } else {
                sp.keywords.join(" ")
            };
            let _ = writeln!(
                out,
                "\n// [seg={}, blob={}, {}, {}] keywords: {} ({} bytes)",
                sp.segment_index,
                sp.blob_index,
                sp.stage_hint,
                sp.gpu_program_name,
                kw,
                sp.raw_size
            );
            if let Some(cg) = &sp.cg_text {
                let _ = writeln!(out, "CGPROGRAM // approximated HLSL/CG from {}", sp.bytecode_kind.label());
                out.push_str(cg);
                if !cg.ends_with('\n') {
                    out.push('\n');
                }
                let _ = writeln!(out, "ENDCG");
            } else {
                out.push_str(&sp.source_text);
                if !sp.source_text.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
        if plat.subprograms.len() > 64 {
            let _ = writeln!(
                out,
                "\n// ... {} additional subprograms available in Subprograms tab ...",
                plat.subprograms.len() - 64
            );
        }
    }
    out
}

fn append_properties_block(pf: &indexmap::IndexMap<String, UnityValue>, out: &mut String) {
    let props_list = pf
        .get("m_PropInfo")
        .and_then(|v| v.as_object())
        .and_then(|pi| pi.get("m_Props"))
        .and_then(|v| v.as_array());
    let Some(props_list) = props_list else {
        return;
    };
    let _ = writeln!(out, "    Properties {{");
    for prop in props_list {
        let Some(p) = prop.as_object() else {
            continue;
        };
        let prop_name = p.get("m_Name").and_then(|v| v.as_str()).unwrap_or("_Prop");
        let desc = p
            .get("m_Description")
            .and_then(|v| v.as_str())
            .unwrap_or(prop_name);
        let ptype = p.get("m_Type").and_then(|v| v.as_i64()).unwrap_or(2);
        let attrs = p
            .get("m_Attributes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| a.as_str())
                    .map(|a| format!("[{a}] "))
                    .collect::<String>()
            })
            .unwrap_or_default();

        let def_str = format_property_default(p, ptype);
        let _ = writeln!(
            out,
            "        {attrs}{prop_name} (\"{}\", {def_str}",
            escape_shader_str(desc)
        );
    }
    let _ = writeln!(out, "    }}");
}

fn format_property_default(p: &indexmap::IndexMap<String, UnityValue>, ptype: i64) -> String {
    let def_val = |idx: usize| -> f64 {
        let key = format!("m_DefValue[{idx}]");
        p.get(&key)
            .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64)))
            .unwrap_or(0.0)
    };
    let d0 = def_val(0);
    let d1 = def_val(1);
    let d2 = def_val(2);
    let d3 = def_val(3);
    match ptype {
        0 => format!("Color) = ({d0:.3}, {d1:.3}, {d2:.3}, {d3:.3})"),
        1 => format!("Vector) = ({d0:.3}, {d1:.3}, {d2:.3}, {d3:.3})"),
        2 => format!("Float) = {d0:.4}"),
        3 => format!("Range({d1:.4}, {d2:.4})) = {d0:.4}"),
        4 => {
            let tex_name = p
                .get("m_DefTexture")
                .and_then(|v| v.as_object())
                .and_then(|t| t.get("m_DefaultName"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("white");
            format!("2D) = \"{tex_name}\" {{}}")
        }
        5 => "Int) = 0".to_owned(),
        _ => format!("Float) = {d0:.4}"),
    }
}

fn append_subshaders_block(
    pf: &indexmap::IndexMap<String, UnityValue>,
    platforms: &[ShaderPlatformBlob],
    out: &mut String,
) {
    let Some(subshaders) = pf.get("m_SubShaders").and_then(|v| v.as_array()) else {
        return;
    };
    for (sub_idx, sub) in subshaders.iter().enumerate() {
        let Some(sub_obj) = sub.as_object() else {
            continue;
        };
        let lod = sub_obj.get("m_LOD").and_then(|v| v.as_i64()).unwrap_or(0);
        let _ = writeln!(out, "    SubShader {{ // #{sub_idx}");
        if lod != 0 {
            let _ = writeln!(out, "        LOD {lod}");
        }
        if let Some(tags) = sub_obj
            .get("m_Tags")
            .and_then(|v| v.as_object())
            .and_then(|t| t.get("tags"))
            .and_then(|v| v.as_array())
            && !tags.is_empty()
        {
            let tag_str = format_tags(tags);
            if !tag_str.is_empty() {
                let _ = writeln!(out, "        Tags {{ {tag_str} }}");
            }
        }
        if let Some(passes) = sub_obj.get("m_Passes").and_then(|v| v.as_array()) {
            for (pass_idx, pass) in passes.iter().enumerate() {
                let Some(po) = pass.as_object() else {
                    continue;
                };
                let pass_name = po
                    .get("m_State")
                    .and_then(|v| v.as_object())
                    .and_then(|st| st.get("m_Name"))
                    .or_else(|| po.get("m_Name"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());

                if let Some(name) = pass_name {
                    let _ = writeln!(out, "        Pass {{ // #{pass_idx}");
                    let _ = writeln!(out, "            Name \"{}\"", escape_shader_str(name));
                } else {
                    let _ = writeln!(out, "        Pass {{ // #{pass_idx}");
                }

                if let Some(state) = po.get("m_State").and_then(|v| v.as_object()) {
                    append_pass_render_state(state, out);
                }
                append_pass_subprogram_summary(po, platforms, out);
                let _ = writeln!(out, "        }}");
            }
        }
        let _ = writeln!(out, "    }}");
    }
}

fn append_pass_render_state(state: &indexmap::IndexMap<String, UnityValue>, out: &mut String) {
    if let Some(tags) = state
        .get("m_Tags")
        .and_then(|v| v.as_object())
        .and_then(|t| t.get("tags"))
        .and_then(|v| v.as_array())
        && !tags.is_empty()
    {
        let tag_str = format_tags(tags);
        if !tag_str.is_empty() {
            let _ = writeln!(out, "            Tags {{ {tag_str} }}");
        }
    }

    let get_val = |k: &str| -> Option<f64> {
        state
            .get(k)
            .and_then(|v| v.as_object())
            .and_then(|o| o.get("val"))
            .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64)))
            .or_else(|| state.get(k).and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64))))
    };

    if let Some(cull) = get_val("culling").map(|v| v as i64)
        && cull != 2
    {
        let label = match cull {
            0 => "Off",
            1 => "Front",
            _ => "Back",
        };
        let _ = writeln!(out, "            Cull {label}");
    }
    if let Some(zwrite) = get_val("zWrite").map(|v| v as i64)
        && zwrite == 0
    {
        let _ = writeln!(out, "            ZWrite Off");
    }
    if let Some(ztest) = get_val("zTest").map(|v| v as i64)
        && ztest != 4
        && ztest != 0
    {
        let label = match ztest {
            1 => "Never",
            2 => "Less",
            3 => "Equal",
            4 => "LEqual",
            5 => "Greater",
            6 => "NotEqual",
            7 => "GEqual",
            8 => "Always",
            _ => "LEqual",
        };
        let _ = writeln!(out, "            ZTest {label}");
    }

    // Blend state from RT blend 0 (`rtBlend0`) or flat keys
    if let Some(rt0) = state.get("rtBlend0").and_then(|v| v.as_object()) {
        let rt_val = |k: &str| -> i64 {
            rt0.get(k)
                .and_then(|v| v.as_object())
                .and_then(|o| o.get("val"))
                .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
                .unwrap_or(0)
        };
        let src = rt_val("srcBlend");
        let dst = rt_val("destBlend");
        let src_a = rt_val("srcBlendAlpha");
        let dst_a = rt_val("destBlendAlpha");
        let col_mask = rt_val("colMask");
        if src != 1 || dst != 0 {
            if src == src_a && dst == dst_a {
                let _ = writeln!(
                    out,
                    "            Blend {} {}",
                    blend_mode_name(src),
                    blend_mode_name(dst)
                );
            } else {
                let _ = writeln!(
                    out,
                    "            Blend {} {}, {} {}",
                    blend_mode_name(src),
                    blend_mode_name(dst),
                    blend_mode_name(src_a),
                    blend_mode_name(dst_a)
                );
            }
        }
        if col_mask != 15 && col_mask >= 0 {
            let mut m = String::new();
            if col_mask & 8 != 0 {
                m.push('R');
            }
            if col_mask & 4 != 0 {
                m.push('G');
            }
            if col_mask & 2 != 0 {
                m.push('B');
            }
            if col_mask & 1 != 0 {
                m.push('A');
            }
            if m.is_empty() {
                m.push('0');
            }
            let _ = writeln!(out, "            ColorMask {m}");
        }
    }

    let off_f = get_val("offsetFactor").unwrap_or(0.0);
    let off_u = get_val("offsetUnits").unwrap_or(0.0);
    if off_f.abs() > f64::EPSILON || off_u.abs() > f64::EPSILON {
        let _ = writeln!(out, "            Offset {off_f}, {off_u}");
    }
    if get_val("alphaToMask").map(|v| v as i64).unwrap_or(0) != 0 {
        let _ = writeln!(out, "            AlphaToMask On");
    }
}

fn append_pass_subprogram_summary(
    po: &indexmap::IndexMap<String, UnityValue>,
    platforms: &[ShaderPlatformBlob],
    out: &mut String,
) {
    let stages = [
        ("progVertex", "Vertex"),
        ("progFragment", "Fragment"),
        ("progGeometry", "Geometry"),
        ("progHull", "Hull"),
        ("progDomain", "Domain"),
    ];
    let mut linked = Vec::new();
    for (field, label) in stages {
        let Some(prog_obj) = po.get(field).and_then(|v| v.as_object()) else {
            continue;
        };
        let mut blob_ids = BTreeSet::new();
        if let Some(subs) = prog_obj.get("m_SubPrograms").and_then(|v| v.as_array()) {
            collect_blob_indices(subs, &mut blob_ids);
        }
        if let Some(tiers) = prog_obj.get("m_PlayerSubPrograms").and_then(|v| v.as_array()) {
            for tier in tiers {
                if let Some(arr) = tier.as_array() {
                    collect_blob_indices(arr, &mut blob_ids);
                } else if let Some(inner) = tier
                    .as_object()
                    .and_then(|o| o.get("Array"))
                    .and_then(|v| v.as_array())
                {
                    collect_blob_indices(inner, &mut blob_ids);
                }
            }
        }
        if !blob_ids.is_empty() {
            let ids: Vec<String> = blob_ids.iter().take(8).map(|id| format!("#{id}")).collect();
            let suffix = if blob_ids.len() > 8 { ", ..." } else { "" };
            linked.push(format!("{label}: [{}]{suffix}", ids.join(", ")));
        }
    }
    if !linked.is_empty() {
        let _ = writeln!(out, "            // Subprograms -> {}", linked.join(" | "));
    } else if !platforms.is_empty() {
        let _ = writeln!(out, "            // Uses compiled platform subprograms");
    }
}

fn collect_blob_indices(subs: &[UnityValue], out: &mut BTreeSet<i64>) {
    for sub in subs {
        if let Some(idx) = sub
            .as_object()
            .and_then(|o| o.get("m_BlobIndex"))
            .and_then(|v| v.as_i64())
        {
            out.insert(idx);
        }
    }
}

fn blend_mode_name(code: i64) -> &'static str {
    match code {
        0 => "Zero",
        1 => "One",
        2 => "DstColor",
        3 => "SrcColor",
        4 => "OneMinusDstColor",
        5 => "SrcAlpha",
        6 => "OneMinusSrcAlpha",
        7 => "DstAlpha",
        8 => "OneMinusDstAlpha",
        9 => "SrcAlphaSaturate",
        10 => "OneMinusSrcAlpha",
        _ => "One",
    }
}

fn format_tags(tags: &[UnityValue]) -> String {
    let mut parts = Vec::new();
    for pair in tags {
        if let Some(obj) = pair.as_object() {
            let k = obj.get("first").and_then(|v| v.as_str()).unwrap_or("");
            let v = obj.get("second").and_then(|v| v.as_str()).unwrap_or("");
            if !k.is_empty() {
                parts.push(format!("\"{k}\" = \"{v}\""));
            }
        }
    }
    parts.join(" ")
}

fn extract_platform_blobs(props: &indexmap::IndexMap<String, UnityValue>) -> Vec<ShaderPlatformBlob> {
    let platforms: Vec<u32> = props
        .get("platforms")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_i64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default();

    let Some(blob) = props
        .get("compressedBlob")
        .or_else(|| props.get("m_SubProgramBlob"))
        .and_then(extract_bytes_field)
    else {
        return Vec::new();
    };
    if blob.is_empty() {
        return Vec::new();
    }

    // Legacy single-blob Unity 5.x shaders without platform tables
    if platforms.is_empty() {
        if let Some(decomp_len) = props.get("decompressedSize").and_then(|v| v.as_i64())
            && decomp_len > 0
            && let Ok(decompressed) = lz4_flex::block::decompress(&blob, decomp_len as usize)
        {
            return vec![build_platform_blob(0, vec![decompressed])];
        }
        return Vec::new();
    }

    let offset_rows = extract_u32_rows(props.get("offsets"));
    let comp_rows = extract_u32_rows(props.get("compressedLengths"));
    let decomp_rows = extract_u32_rows(props.get("decompressedLengths"));

    let mut result = Vec::new();
    for (plat_idx, &plat_id) in platforms.iter().enumerate() {
        let Some(offsets) = offset_rows.get(plat_idx) else {
            continue;
        };
        let Some(comp_lens) = comp_rows.get(plat_idx) else {
            continue;
        };
        let Some(decomp_lens) = decomp_rows.get(plat_idx) else {
            continue;
        };

        let mut segments = Vec::new();
        for seg_idx in 0..offsets.len().min(comp_lens.len()).min(decomp_lens.len()) {
            let start = offsets[seg_idx] as usize;
            let comp_len = comp_lens[seg_idx] as usize;
            let decomp_len = decomp_lens[seg_idx] as usize;
            let end = start.saturating_add(comp_len);
            if end > blob.len() || decomp_len == 0 {
                continue;
            }
            let slice = &blob[start..end];
            if decomp_len == comp_len {
                segments.push(slice.to_vec());
            } else if let Ok(bytes) = lz4_flex::block::decompress(slice, decomp_len) {
                segments.push(bytes);
            }
        }
        if !segments.is_empty() {
            result.push(build_platform_blob(plat_id, segments));
        }
    }
    result
}

fn build_platform_blob(plat_id: u32, segments: Vec<Vec<u8>>) -> ShaderPlatformBlob {
    let mut combined = Vec::new();
    let mut all_subprograms = Vec::new();
    let mut detected_kinds = Vec::new();
    let mut text_snippets = Vec::new();

    for (seg_idx, seg) in segments.iter().enumerate() {
        combined.extend_from_slice(seg);
        let parsed = parse_subprogram_blob(seg_idx, seg);
        if !parsed.is_empty() {
            for sp in parsed {
                if !detected_kinds.contains(&sp.bytecode_kind) {
                    detected_kinds.push(sp.bytecode_kind);
                }
                if text_snippets.len() < 8 {
                    let preview = sp.cg_text.as_ref().unwrap_or(&sp.source_text);
                    if !preview.is_empty() {
                        text_snippets.push(preview.clone());
                    }
                }
                all_subprograms.push(sp);
            }
        } else {
            let (kinds, snippets) = classify_shader_blob(seg);
            for k in kinds {
                if !detected_kinds.contains(&k) {
                    detected_kinds.push(k);
                }
            }
            for s in snippets {
                if text_snippets.len() < 8 {
                    text_snippets.push(s);
                }
            }
        }
    }

    if detected_kinds.is_empty() && !combined.is_empty() {
        detected_kinds.push(ShaderBytecodeKind::Binary);
    }

    ShaderPlatformBlob {
        platform_id: plat_id,
        platform_name: shader_compiler_platform_name(plat_id).to_owned(),
        decompressed_bytes: combined,
        detected_kinds,
        text_snippets,
        subprograms: all_subprograms,
    }
}

/// Parses BOTH Unity `2019.3+` nested `Array<Array<u32>>` (all segments!) and flat `Array<u32>`.
fn extract_u32_rows(val: Option<&UnityValue>) -> Vec<Vec<u32>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .map(|item| {
            if let Some(n) = item.as_i64() {
                vec![n as u32]
            } else if let Some(inner) = item.as_array() {
                inner.iter().filter_map(|x| x.as_i64().map(|n| n as u32)).collect()
            } else if let Some(inner) = item
                .as_object()
                .and_then(|o| o.get("Array"))
                .and_then(|v| v.as_array())
            {
                inner.iter().filter_map(|x| x.as_i64().map(|n| n as u32)).collect()
            } else {
                Vec::new()
            }
        })
        .collect()
}

/// Parses Unity `ShaderSubProgramBlob` (auto-detecting 8-byte pre-2019.3 vs 12-byte 2019.3+ table).
fn parse_subprogram_blob(seg_idx: usize, bytes: &[u8]) -> Vec<DecompiledSubProgram> {
    if bytes.len() < 12 {
        return Vec::new();
    }
    let count = read_u32_le(bytes, 0) as usize;
    if count == 0 || count > 100_000 {
        return Vec::new();
    }
    let h8 = 4 + count * 8;
    let h12 = 4 + count * 12;
    let first_off = read_u32_le(bytes, 4) as usize;
    let stride = if first_off == h12 && h12 <= bytes.len() {
        12usize
    } else if first_off == h8 && h8 <= bytes.len() {
        8usize
    } else if h12 <= bytes.len() && first_off >= h12 {
        12usize
    } else if h8 <= bytes.len() && first_off >= h8 {
        8usize
    } else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(count.min(512));
    for idx in 0..count {
        let base = 4 + idx * stride;
        let off = read_u32_le(bytes, base) as usize;
        let len = read_u32_le(bytes, base + 4) as usize;
        let blob_index = if stride == 12 {
            read_u32_le(bytes, base + 8) as usize
        } else {
            idx
        };
        let end = off.saturating_add(len);
        if len < 20 || end > bytes.len() {
            continue;
        }
        if let Some(sp) = parse_single_subprogram(seg_idx, blob_index, &bytes[off..end]) {
            out.push(sp);
        }
    }
    out
}

fn parse_single_subprogram(
    seg_idx: usize,
    blob_index: usize,
    entry: &[u8],
) -> Option<DecompiledSubProgram> {
    if entry.len() < 16 {
        return None;
    }
    let version = read_i32_le(entry, 0);
    if !(2000_00000..=2050_00000).contains(&version) {
        return None;
    }
    let gpu_program_type = read_u32_le(entry, 4);
    let stats_ints = if version >= 201608170 { 4usize } else { 3usize };
    let mut pos = 8 + stats_ints * 4;
    if pos + 4 > entry.len() {
        return None;
    }

    let mut keywords = read_aligned_string_array(entry, &mut pos)?;
    if (201806140..202012090).contains(&version)
        && let Some(local_kws) = read_aligned_string_array(entry, &mut pos)
    {
        for lk in local_kws {
            if !keywords.contains(&lk) {
                keywords.push(lk);
            }
        }
    }

    if pos + 4 > entry.len() {
        return None;
    }
    let prog_len = read_u32_le(entry, pos) as usize;
    pos += 4;
    let prog_end = pos.checked_add(prog_len)?.min(entry.len());
    let prog_bytes = &entry[pos..prog_end];

    let (bytecode_kind, source_text, cg_text) = decompile_program_bytes(gpu_program_type, prog_bytes);
    Some(DecompiledSubProgram {
        segment_index: seg_idx,
        blob_index,
        gpu_program_type,
        gpu_program_name: gpu_program_type_name(gpu_program_type),
        stage_hint: infer_stage_hint(gpu_program_type, &source_text),
        keywords,
        bytecode_kind,
        raw_size: prog_bytes.len(),
        source_text,
        cg_text,
    })
}

fn read_aligned_string_array(data: &[u8], pos: &mut usize) -> Option<Vec<String>> {
    if *pos + 4 > data.len() {
        return None;
    }
    let count = read_u32_le(data, *pos) as usize;
    *pos += 4;
    if count > 4096 {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if *pos + 4 > data.len() {
            return None;
        }
        let len = read_u32_le(data, *pos) as usize;
        *pos += 4;
        if *pos + len > data.len() {
            return None;
        }
        out.push(String::from_utf8_lossy(&data[*pos..*pos + len]).into_owned());
        let aligned = (len + 3) & !3;
        *pos = (*pos + aligned).min(data.len());
    }
    Some(out)
}

fn decompile_program_bytes(
    gpu_program_type: u32,
    bytes: &[u8],
) -> (ShaderBytecodeKind, String, Option<String>) {
    // 1. Direct DXBC container (`"DXBC"`)
    if bytes.starts_with(b"DXBC") {
        let asm = decompile_dxbc(bytes);
        return (ShaderBytecodeKind::Dxbc, asm, None);
    }

    // 2. Vulkan SMOL-V / SPIR-V (type 23 or header magic inside Unity Vulkan wrapper)
    if gpu_program_type == 23
        || bytes.windows(4).any(|w| w == b"LOMS" || w == [0x03, 0x02, 0x23, 0x07])
    {
        let chunks = extract_spirv_from_unity_subprogram(bytes);
        if !chunks.is_empty() {
            let is_smolv = bytes.windows(4).any(|w| w == b"LOMS");
            let mut text = String::new();
            for (label, dis) in chunks {
                let _ = writeln!(text, "// --- Vulkan {label} ---");
                text.push_str(&dis.text);
                text.push('\n');
            }
            return (
                if is_smolv {
                    ShaderBytecodeKind::Smolv
                } else {
                    ShaderBytecodeKind::Spirv
                },
                text,
                None,
            );
        }
        if let Some(dis) = disassemble_spirv(bytes) {
            return (ShaderBytecodeKind::Spirv, dis.text, None);
        }
    }

    // 3. Printable GLSL / Metal / ARB source (trim trailing NULs cleanly)
    let trimmed_bytes = match bytes.iter().rposition(|&b| b != 0) {
        Some(last) => &bytes[..=last],
        None => &[],
    };
    if let Ok(text) = std::str::from_utf8(trimmed_bytes)
        && is_likely_shader_source(text)
    {
        let clean = text.trim_matches('\0').trim().to_owned();
        if clean.contains("#include <metal_stdlib>") || (20..=22).contains(&gpu_program_type) {
            return (ShaderBytecodeKind::MetalSource, clean, None);
        }
        let cg = glsl_to_readable_cg(&clean);
        return (ShaderBytecodeKind::GlslText, clean, Some(cg));
    }

    // 4. Fallback embedded ASCII scan
    let (kinds, snippets) = classify_shader_blob(bytes);
    if let Some(first) = snippets.into_iter().next() {
        let kind = kinds.into_iter().next().unwrap_or(ShaderBytecodeKind::GlslText);
        let cg = (kind == ShaderBytecodeKind::GlslText).then(|| glsl_to_readable_cg(&first));
        return (kind, first, cg);
    }

    // 5. Fallback hex summary
    let hex_preview: String = bytes
        .iter()
        .take(64)
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    (
        ShaderBytecodeKind::Binary,
        format!("// Binary payload ({} bytes)\n// Head: {hex_preview}", bytes.len()),
        None,
    )
}

fn is_likely_shader_source(s: &str) -> bool {
    let sample = s.trim_start();
    sample.starts_with("#version ")
        || sample.starts_with("#ifdef ")
        || sample.starts_with("#ifndef ")
        || sample.starts_with("#define ")
        || sample.starts_with("#include ")
        || sample.starts_with("!!ARB")
        || sample.contains("void main(")
        || sample.contains("gl_Position")
}

/// Converts GLSL shader source into clean CG/HLSL-style pseudocode while preserving structure.
pub fn glsl_to_readable_cg(glsl: &str) -> String {
    let mut out = String::with_capacity(glsl.len() + 128);
    for line in glsl.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#version ") {
            let _ = writeln!(out, "// Source GLSL: {trimmed}");
            continue;
        }
        if trimmed.starts_with("precision ") {
            continue;
        }
        let mut converted = line.to_owned();
        for (from, to) in [
            ("highp ", ""),
            ("mediump ", ""),
            ("lowp ", ""),
            ("vec2", "float2"),
            ("vec3", "float3"),
            ("vec4", "float4"),
            ("ivec2", "int2"),
            ("ivec3", "int3"),
            ("ivec4", "int4"),
            ("uvec2", "uint2"),
            ("uvec3", "uint3"),
            ("uvec4", "uint4"),
            ("bvec2", "bool2"),
            ("bvec3", "bool3"),
            ("bvec4", "bool4"),
            ("mat2", "float2x2"),
            ("mat3", "float3x3"),
            ("mat4", "float4x4"),
            ("mix", "lerp"),
            ("fract", "frac"),
            ("inversesqrt", "rsqrt"),
            ("dFdx", "ddx"),
            ("dFdy", "ddy"),
            ("atan", "atan2"),
            ("texture2D", "tex2D"),
            ("texture2DProj", "tex2Dproj"),
            ("texture2DLodEXT", "tex2Dlod"),
            ("textureCube", "texCUBE"),
            ("textureLod", "tex2Dlod"),
            ("texture", "tex2D"),
            ("gl_Position", "out.pos"),
            ("gl_FragColor", "out.sv_target"),
            ("gl_FragData", "out.sv_target_arr"),
            ("discard;", "clip(-1);"),
        ] {
            converted = replace_ident(&converted, from, to);
        }
        out.push_str(&converted);
        out.push('\n');
    }
    out
}

fn replace_ident(input: &str, needle: &str, replacement: &str) -> String {
    if !input.contains(needle) {
        return input.to_owned();
    }
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = input.as_bytes();
    let needle_b = needle.as_bytes();
    let starts_word = needle_b.first().copied().is_some_and(is_word);
    let ends_word = needle_b.last().copied().is_some_and(is_word);

    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i..].starts_with(needle_b) {
            let prev_ok = !starts_word || i == 0 || !is_word(bytes[i - 1]);
            let after_idx = i + needle_b.len();
            let next_ok = !ends_word || after_idx >= bytes.len() || !is_word(bytes[after_idx]);
            if prev_ok && next_ok {
                out.push_str(replacement);
                i = after_idx;
                continue;
            }
        }
        let ch = input[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Decodes Direct3D DXBC container chunks (`RDEF`, `ISGN`, `OSGN`, `STAT`, `SHDR`/`SHEX`)
/// into readable HLSL resource/signature reflection + SM4/SM5 disassembly.
pub fn decompile_dxbc(bytes: &[u8]) -> String {
    if bytes.len() < 32 || !bytes.starts_with(b"DXBC") {
        return "// Invalid DXBC container".to_owned();
    }
    let total_size = read_u32_le(bytes, 24);
    let chunk_count = read_u32_le(bytes, 28) as usize;
    let mut out = String::new();
    let _ = writeln!(out, "// Direct3D DXBC ({} bytes, {} chunks)", total_size, chunk_count);

    for ci in 0..chunk_count.min(32) {
        let ptr_off = 32 + ci * 4;
        if ptr_off + 4 > bytes.len() {
            break;
        }
        let chunk_off = read_u32_le(bytes, ptr_off) as usize;
        if chunk_off + 8 > bytes.len() {
            continue;
        }
        let fourcc = &bytes[chunk_off..chunk_off + 4];
        let chunk_len = read_u32_le(bytes, chunk_off + 4) as usize;
        let payload_end = (chunk_off + 8).saturating_add(chunk_len).min(bytes.len());
        let payload = &bytes[chunk_off + 8..payload_end];
        let tag = String::from_utf8_lossy(fourcc);

        match fourcc {
            b"ISGN" | b"OSGN" | b"PCSG" => {
                parse_dxbc_signature(&tag, payload, &mut out);
            }
            b"RDEF" => {
                parse_dxbc_rdef(payload, &mut out);
            }
            b"STAT" if payload.len() >= 16 => {
                let instrs = read_u32_le(payload, 0);
                let temps = read_u32_le(payload, 4);
                let _ = writeln!(out, "// STAT: instructions={instrs}, temp_regs={temps}");
            }
            b"SHDR" | b"SHEX" => {
                parse_dxbc_bytecode(payload, &mut out);
            }
            _ => {
                let _ = writeln!(out, "// Chunk {tag} ({chunk_len} bytes)");
            }
        }
    }
    out
}

fn parse_dxbc_signature(tag: &str, payload: &[u8], out: &mut String) {
    if payload.len() < 8 {
        return;
    }
    let count = read_u32_le(payload, 0) as usize;
    let _ = writeln!(out, "// {tag} ({count} semantics):");
    for i in 0..count.min(64) {
        let entry_off = 8 + i * 24;
        if entry_off + 24 > payload.len() {
            break;
        }
        let name_off = read_u32_le(payload, entry_off) as usize;
        let sem_idx = read_u32_le(payload, entry_off + 4);
        let reg = read_u32_le(payload, entry_off + 12);
        let mask = payload[entry_off + 20] & 0x0F;
        let name = read_cstr(payload, name_off).unwrap_or_else(|| "PARAM".into());
        let _ = writeln!(
            out,
            "//   {name}{sem_idx} -> r{reg} (mask=0x{mask:X})"
        );
    }
}

fn parse_dxbc_rdef(payload: &[u8], out: &mut String) {
    if payload.len() < 28 {
        return;
    }
    let cbuf_count = read_u32_le(payload, 0) as usize;
    let cbuf_off = read_u32_le(payload, 4) as usize;
    let bind_count = read_u32_le(payload, 8) as usize;
    let bind_off = read_u32_le(payload, 12) as usize;
    let creator_off = read_u32_le(payload, 24) as usize;
    if let Some(creator) = read_cstr(payload, creator_off) {
        let _ = writeln!(out, "// RDEF Creator: {creator}");
    }
    for i in 0..cbuf_count.min(32) {
        let off = cbuf_off + i * 24;
        if off + 24 > payload.len() {
            break;
        }
        let name_off = read_u32_le(payload, off) as usize;
        let var_count = read_u32_le(payload, off + 4);
        let byte_size = read_u32_le(payload, off + 12);
        let name = read_cstr(payload, name_off).unwrap_or_else(|| format!("cb{i}"));
        let _ = writeln!(out, "cbuffer {name} // {var_count} vars, {byte_size} bytes");
    }
    for i in 0..bind_count.min(64) {
        let off = bind_off + i * 32;
        if off + 32 > payload.len() {
            break;
        }
        let name_off = read_u32_le(payload, off) as usize;
        let res_type = read_u32_le(payload, off + 4);
        let bind_point = read_u32_le(payload, off + 16);
        let name = read_cstr(payload, name_off).unwrap_or_else(|| format!("res{i}"));
        let ty_name = match res_type {
            0 => "cbuffer",
            1 => "tbuffer",
            2 => "Texture",
            3 => "SamplerState",
            4..=7 => "UAV",
            _ => "Resource",
        };
        let _ = writeln!(out, "{ty_name} {name} : register(slot{bind_point});");
    }
}

fn parse_dxbc_bytecode(payload: &[u8], out: &mut String) {
    if payload.len() < 8 {
        return;
    }
    let ver_tok = read_u32_le(payload, 0);
    let dword_len = read_u32_le(payload, 4) as usize;
    let sh_type = (ver_tok >> 16) & 0xFFFF;
    let major = (ver_tok >> 4) & 0xF;
    let minor = ver_tok & 0xF;
    let prefix = match sh_type {
        0 => "ps",
        1 => "vs",
        2 => "gs",
        3 => "hs",
        4 => "ds",
        5 => "cs",
        _ => "sm",
    };
    let _ = writeln!(out, "{prefix}_{major}_{minor} // {dword_len} dwords");

    let mut idx = 2usize;
    let max_words = dword_len.min(payload.len() / 4);
    let mut emitted = 0usize;
    while idx < max_words && emitted < 256 {
        let tok = read_u32_le(payload, idx * 4);
        let op = tok & 0x7FF;
        let inst_len = ((tok >> 24) & 0x7F) as usize;
        let step = inst_len.max(1);
        let op_name = dxbc_sm4_opcode_name(op);
        let _ = writeln!(out, "  {op_name} ({step} dwords, raw=0x{tok:08x})");
        idx += step;
        emitted += 1;
    }
    if idx < max_words {
        let _ = writeln!(out, "  // ... {} more instructions", max_words - idx);
    }
}

fn dxbc_sm4_opcode_name(op: u32) -> &'static str {
    match op {
        0 => "add",
        1 => "and",
        2 => "break",
        10 => "derv_rtx",
        11 => "derv_rty",
        12 => "discard",
        13 => "div",
        14 => "dp2",
        15 => "dp3",
        16 => "dp4",
        18 => "else",
        19 => "endif",
        22 => "exp",
        31 => "if",
        43 => "log",
        44 => "loop",
        48 => "mad",
        49 => "min",
        50 => "max",
        54 => "mov",
        55 => "movc",
        56 => "mul",
        62 => "ret",
        68 => "rsq",
        69 => "sample",
        70 => "sample_c",
        71 => "sample_l",
        74 => "sqrt",
        89 => "dcl_constantbuffer",
        90 => "dcl_sampler",
        91 => "dcl_resource",
        95 => "dcl_input",
        96 => "dcl_input_ps",
        97 => "dcl_input_siv",
        101 => "dcl_output",
        103 => "dcl_output_siv",
        104 => "dcl_temps",
        106 => "dcl_globalFlags",
        _ => "op",
    }
}

fn read_cstr(buf: &[u8], off: usize) -> Option<String> {
    let slice = buf.get(off..)?;
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    Some(String::from_utf8_lossy(&slice[..end.min(256)]).into_owned())
}

fn read_u32_le(data: &[u8], off: usize) -> u32 {
    data.get(off..off + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .unwrap_or(0)
}

fn read_i32_le(data: &[u8], off: usize) -> i32 {
    data.get(off..off + 4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .unwrap_or(0)
}

fn classify_shader_blob(bytes: &[u8]) -> (Vec<ShaderBytecodeKind>, Vec<String>) {
    let mut kinds = Vec::new();
    let mut snippets = Vec::new();

    if bytes.windows(4).any(|w| w == b"DXBC") {
        kinds.push(ShaderBytecodeKind::Dxbc);
    }
    if bytes.windows(4).any(|w| w == [0x03, 0x02, 0x23, 0x07]) {
        kinds.push(ShaderBytecodeKind::Spirv);
    }
    if bytes.windows(4).any(|w| w == b"SMOL" || w == b"LOMS") {
        kinds.push(ShaderBytecodeKind::Smolv);
    }

    // Scan printable ASCII runs that look like GLSL or Metal shaders.
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"#version ")
            || bytes[i..].starts_with(b"#include <metal_stdlib>")
            || bytes[i..].starts_with(b"!!ARB")
        {
            let is_metal = bytes[i..].starts_with(b"#include <metal_stdlib>");
            let end = bytes[i..]
                .iter()
                .position(|&b| b == 0)
                .map(|rel| (i + rel).min(i + 32 * 1024))
                .unwrap_or((i + 32 * 1024).min(bytes.len()));
            if let Ok(text) = std::str::from_utf8(&bytes[i..end]) {
                let kind = if is_metal {
                    ShaderBytecodeKind::MetalSource
                } else {
                    ShaderBytecodeKind::GlslText
                };
                if !kinds.contains(&kind) {
                    kinds.push(kind);
                }
                snippets.push(text.trim_end().to_owned());
            }
            i = end;
        } else {
            i += 1;
        }
    }

    if kinds.is_empty() && !bytes.is_empty() {
        kinds.push(ShaderBytecodeKind::Binary);
    }
    (kinds, snippets)
}

fn shader_compiler_platform_name(id: u32) -> &'static str {
    match id {
        4 => "Direct3D 11 / 12",
        5 => "OpenGL ES 2.0",
        9 => "OpenGL Core / Desktop",
        11 => "OpenGL ES 3.x",
        14 => "Metal",
        18 => "Vulkan (SPIR-V)",
        19 => "Switch NVN",
        20 => "WebGL",
        _ => "Unknown Graphics API",
    }
}

fn gpu_program_type_name(id: u32) -> &'static str {
    match id {
        1 => "GLLegacy",
        2 | 3 => "DX9 Vertex/Fragment",
        4 | 5 => "DX11 Vertex SM4/5",
        6 | 7 => "DX11 Pixel SM4/5",
        8 | 9 => "DX11 Geometry",
        10 | 11 => "DX11 Hull/Domain",
        12 => "DX11 Compute",
        14..=16 => "OpenGL ES 3.x",
        17..=19 => "OpenGL Core",
        20 => "Metal Vertex",
        21 => "Metal Fragment",
        22 => "Metal Compute",
        23 => "Vulkan SPIR-V",
        _ => "ShaderProgram",
    }
}

fn infer_stage_hint(gpu_program_type: u32, source: &str) -> &'static str {
    match gpu_program_type {
        4 | 5 | 20 => "Vertex",
        6 | 7 | 21 => "Fragment",
        8 | 9 => "Geometry",
        10 => "Hull",
        11 => "Domain",
        12 | 22 => "Compute",
        _ => {
            if source.contains("gl_Position") || source.contains("VERTEX") {
                "Vertex"
            } else if source.contains("gl_FragColor") || source.contains("FRAGMENT") {
                "Fragment"
            } else {
                "Stage"
            }
        }
    }
}

fn extract_shader_header_name(script: &str) -> Option<String> {
    let idx = script.find("Shader \"")?;
    let rest = &script[idx + 8..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

fn escape_shader_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_spirv_dxbc_and_glsl() {
        let mut blob = Vec::new();
        blob.extend_from_slice(b"DXBC\x00\x00\x00\x00");
        blob.extend_from_slice(&[0x03, 0x02, 0x23, 0x07]);
        blob.extend_from_slice(b"#version 300 es\nvoid main() {}\0");
        let (kinds, snippets) = classify_shader_blob(&blob);
        assert!(kinds.contains(&ShaderBytecodeKind::Dxbc));
        assert!(kinds.contains(&ShaderBytecodeKind::Spirv));
        assert!(kinds.contains(&ShaderBytecodeKind::GlslText));
        assert_eq!(snippets.len(), 1);
        assert!(snippets[0].starts_with("#version 300 es"));
    }

    #[test]
    fn parses_unity_subprogram_blob_and_rewrites_glsl_to_cg() {
        let glsl = b"#version 300 es\nprecision highp float;\nuniform vec4 _Color;\nvoid main() { gl_FragColor = mix(_Color, vec4(1.0), 0.5); }\0";
        let mut subprog = Vec::new();
        subprog.extend_from_slice(&202012090i32.to_le_bytes()); // version
        subprog.extend_from_slice(&16u32.to_le_bytes());       // GLES3
        subprog.extend_from_slice(&[0u8; 16]);                  // 4 stats ints
        // keywords: 1 string "DIRECTIONAL"
        subprog.extend_from_slice(&1u32.to_le_bytes());
        subprog.extend_from_slice(&11u32.to_le_bytes());
        subprog.extend_from_slice(b"DIRECTIONAL\0");
        // program length + bytes
        subprog.extend_from_slice(&(glsl.len() as u32).to_le_bytes());
        subprog.extend_from_slice(glsl);

        // 12-byte table ShaderSubProgramBlob (1 entry)
        let mut seg = Vec::new();
        seg.extend_from_slice(&1u32.to_le_bytes());
        seg.extend_from_slice(&16u32.to_le_bytes()); // offset = 4 + 12
        seg.extend_from_slice(&(subprog.len() as u32).to_le_bytes());
        seg.extend_from_slice(&0u32.to_le_bytes()); // segment 0
        seg.extend_from_slice(&subprog);

        let parsed = parse_subprogram_blob(0, &seg);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].keywords, vec!["DIRECTIONAL"]);
        assert_eq!(parsed[0].bytecode_kind, ShaderBytecodeKind::GlslText);
        let cg = parsed[0].cg_text.as_deref().expect("cg translation");
        assert!(cg.contains("float4 _Color"));
        assert!(cg.contains("lerp("));
        assert!(cg.contains("out.sv_target"));
    }

    #[test]
    fn parses_dxbc_signatures_and_shader_header() {
        // Construct minimal DXBC with 1 SHEX chunk (`ps_5_0`)
        let mut shex = Vec::new();
        shex.extend_from_slice(b"SHEX");
        shex.extend_from_slice(&12u32.to_le_bytes());
        shex.extend_from_slice(&0x0000_0050u32.to_le_bytes()); // ps_5_0
        shex.extend_from_slice(&3u32.to_le_bytes());         // 3 dwords
        shex.extend_from_slice(&((1u32 << 24) | 62).to_le_bytes()); // ret

        let mut dxbc = Vec::new();
        dxbc.extend_from_slice(b"DXBC");
        dxbc.extend_from_slice(&[0u8; 20]); // checksum + version
        dxbc.extend_from_slice(&(36 + shex.len() as u32).to_le_bytes());
        dxbc.extend_from_slice(&1u32.to_le_bytes()); // chunk count
        dxbc.extend_from_slice(&36u32.to_le_bytes()); // chunk offset
        dxbc.extend_from_slice(&shex);

        let asm = decompile_dxbc(&dxbc);
        assert!(asm.contains("ps_5_0"));
        assert!(asm.contains("ret"));
    }
}
