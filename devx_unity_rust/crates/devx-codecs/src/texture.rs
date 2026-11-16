//! Unity `Texture2D` (`class_id == 28`), `Sprite` (`class_id == 213`), and Cubemap decoders,
//! channel isolation, DDS header generation, and PNG/TGA encoders.

use crate::{CodecError, CodecResult};
use devx_plugin_api::{AssetSummary, DecodedImage, PluginHostContext, PluginRegistry};
use devx_unity_io::{
    ResourceStreamStore, UnityWorkspaceData, extract_i32_field, extract_i64_field, read_object,
    sanitize_unity_version,
};
use image::{DynamicImage, ImageFormat, RgbaImage, imageops};
use std::collections::HashMap;
use std::io::Cursor;
use unity_asset_binary::object::UnityObject;
use unity_asset_core::UnityValue;
use unity_asset_decode::sprite::SpriteProcessor;
use unity_asset_decode::texture::{Texture2D, Texture2DConverter, TextureFormat};
use unity_asset_decode::unity_version::UnityVersion;

/// Options controlling texture decode post-processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureDecodeOptions {
    pub flip_vertical: bool,
    pub invert_alpha: bool,
    pub channel_mask: ChannelMask,
}

impl Default for TextureDecodeOptions {
    fn default() -> Self {
        Self {
            flip_vertical: true,
            invert_alpha: false,
            channel_mask: ChannelMask::All,
        }
    }
}

/// Color channel display filter matching DevX's RGBA viewer toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelMask {
    #[default]
    All,
    Red,
    Green,
    Blue,
    Alpha,
}

/// Decodes a Unity `Texture2D` (`class_id == 28`) object into RGBA8, resolving external `.resS` streams.
pub fn decode_texture2d(
    obj: &UnityObject,
    unity_version: &str,
    streams: &ResourceStreamStore,
    options: TextureDecodeOptions,
) -> CodecResult<DecodedImage> {
    let clean_ver = sanitize_unity_version(unity_version);
    let version = UnityVersion::parse_version(&clean_ver).unwrap_or_default();
    let converter = Texture2DConverter::new(version);
    let mut tex: Texture2D = converter
        .from_unity_object(obj)
        .map_err(|e| CodecError::MalformedData(format!("Texture2D parse failed: {e}")))?;

    if tex.image_data.is_empty() && !tex.stream_info.path.is_empty() && tex.stream_info.size > 0 {
        let Some(slice) = streams.read_slice(
            &tex.stream_info.path,
            tex.stream_info.offset,
            tex.stream_info.size,
        ) else {
            return Err(CodecError::MissingStream(tex.stream_info.path.clone()));
        };
        tex.image_data = slice.to_vec();
        tex.data_size = tex.image_data.len() as i32;
    }

    let raw_format_id = obj
        .class
        .properties()
        .get("m_TextureFormat")
        .and_then(extract_i32_field)
        .unwrap_or(tex.format as i32);
    let fmt_name = texture_format_name(raw_format_id).to_owned();

    if tex.image_data.is_empty() {
        return Err(CodecError::MalformedData(format!(
            "Texture2D '{}' ({}x{}, {fmt_name}) contains no pixel payload",
            tex.name, tex.width, tex.height
        )));
    }

    let width = u32::try_from(tex.width)
        .ok()
        .filter(|&v| v > 0)
        .ok_or_else(|| {
            CodecError::MalformedData(format!("Texture2D '{}' has invalid width {}", tex.name, tex.width))
        })?;
    let height = u32::try_from(tex.height)
        .ok()
        .filter(|&v| v > 0)
        .ok_or_else(|| {
            CodecError::MalformedData(format!(
                "Texture2D '{}' has invalid height {}",
                tex.name, tex.height
            ))
        })?;

    let mut rgba = decode_texture_bytes(raw_format_id, width, height, &tex.image_data)?;

    if options.flip_vertical {
        imageops::flip_vertical_in_place(&mut rgba);
    }

    let mut raw = rgba.into_raw();
    if options.invert_alpha {
        for px in raw.as_chunks_mut::<4>().0 {
            px[3] = 255 - px[3];
        }
    }

    let mut decoded = DecodedImage {
        width,
        height,
        rgba8: raw,
        format_name: fmt_name,
        mip_count: tex.mip_count.max(1) as u32,
    };

    if options.channel_mask != ChannelMask::All {
        decoded = apply_channel_mask(&decoded, options.channel_mask);
    }

    Ok(decoded)
}

/// Runs registered texture post-processor plugins after decoding a `Texture2D`.
pub fn decode_texture2d_with_plugins(
    obj: &UnityObject,
    summary: &AssetSummary,
    streams: &ResourceStreamStore,
    options: TextureDecodeOptions,
    plugins: &PluginRegistry,
    host: &mut dyn PluginHostContext,
) -> CodecResult<DecodedImage> {
    let mut img = decode_texture2d(obj, &summary.unity_version, streams, options)?;
    plugins
        .post_process_texture(summary, &mut img, host)
        .map_err(|e| CodecError::MalformedData(format!("plugin hook failed: {e}")))?;
    Ok(img)
}

/// Extracts metadata for a raw `Texture2D` so callers can export direct `.dds` when block-compressed.
pub fn inspect_texture2d_raw(
    obj: &UnityObject,
    unity_version: &str,
    streams: &ResourceStreamStore,
) -> CodecResult<Texture2D> {
    let clean_ver = sanitize_unity_version(unity_version);
    let version = UnityVersion::parse_version(&clean_ver).unwrap_or_default();
    let converter = Texture2DConverter::new(version);
    let mut tex = converter
        .from_unity_object(obj)
        .map_err(|e| CodecError::MalformedData(format!("Texture2D parse failed: {e}")))?;
    if tex.image_data.is_empty() && !tex.stream_info.path.is_empty() && tex.stream_info.size > 0
        && let Some(slice) = streams.read_slice(
            &tex.stream_info.path,
            tex.stream_info.offset,
            tex.stream_info.size,
        )
    {
        tex.image_data = slice.to_vec();
        tex.data_size = tex.image_data.len() as i32;
    }
    Ok(tex)
}

/// Decoded sprite crop plus sprite metadata.
#[derive(Debug, Clone)]
pub struct DecodedSprite {
    pub name: String,
    pub image: DecodedImage,
    pub rect: [f32; 4],
    pub pivot: [f32; 2],
    pub pixels_to_units: f32,
    pub source_texture: Option<(usize, i64)>,
}

/// Decodes a Unity `Sprite` (`class_id == 213`) by resolving its backing `Texture2D` (or packed
/// `SpriteAtlas` + `m_RenderDataKey` entry) across files and cropping its `textureRect` region.
pub fn decode_sprite(
    sprite_obj: &UnityObject,
    sprite_file_index: usize,
    unity_version: &str,
    workspace: &UnityWorkspaceData,
    tpk: &dyn unity_asset_binary::typetree::TypeTreeRegistry,
) -> CodecResult<DecodedSprite> {
    let mut cache = HashMap::new();
    decode_sprite_cached(
        sprite_obj,
        sprite_file_index,
        unity_version,
        workspace,
        tpk,
        &mut cache,
    )
}

/// Decodes a Unity `Sprite` while reusing previously decoded backing `Texture2D`s stored in
/// `texture_cache` (keyed by `(file_index, path_id)`), dramatically speeding up batch `SpriteAtlas`
/// exports.
pub fn decode_sprite_cached(
    sprite_obj: &UnityObject,
    sprite_file_index: usize,
    unity_version: &str,
    workspace: &UnityWorkspaceData,
    _tpk: &dyn unity_asset_binary::typetree::TypeTreeRegistry,
    texture_cache: &mut HashMap<(usize, i64), DecodedImage>,
) -> CodecResult<DecodedSprite> {
    let clean_ver = sanitize_unity_version(unity_version);
    let version = UnityVersion::parse_version(&clean_ver).unwrap_or_default();
    let processor = SpriteProcessor::new(version);
    let parsed = processor
        .parse_sprite(sprite_obj)
        .map_err(|e| CodecError::MalformedData(format!("Sprite parse failed: {e}")))?;
    let sprite = parsed.sprite;

    let resolved = resolve_sprite_render_source(sprite_obj, &sprite, sprite_file_index, workspace)?;

    let cache_key = (resolved.tex_file_idx, resolved.tex_path_id);
    if let std::collections::hash_map::Entry::Vacant(e) = texture_cache.entry(cache_key) {
        let mut decoded = decode_workspace_texture2d(
            workspace,
            resolved.tex_file_idx,
            resolved.tex_path_id,
            unity_version,
        )?;
        if let Some((alpha_file_idx, alpha_path_id)) = resolved.alpha_tex {
            apply_split_alpha_texture(
                &mut decoded,
                workspace,
                alpha_file_idx,
                alpha_path_id,
                unity_version,
            )?;
        }
        e.insert(decoded);
    }

    let full_img = &texture_cache[&cache_key];
    let (mut rx, mut ry, mut rw, mut rh) = resolved.rect;
    if rw <= 0.0 || rh <= 0.0 {
        rx = 0.0;
        ry = 0.0;
        rw = full_img.width as f32;
        rh = full_img.height as f32;
    }

    if full_img.width == 0 || full_img.height == 0 {
        return Err(CodecError::MalformedData(format!(
            "Sprite '{}' backing texture has zero dimensions ({}x{})",
            sprite.name, full_img.width, full_img.height
        )));
    }
    let x0 = (rx.floor().max(0.0) as u32).min(full_img.width - 1);
    let crop_w = (rw.round().max(1.0) as u32).min(full_img.width - x0);
    let y_bottom = (ry.floor().max(0.0) as u32).min(full_img.height - 1);
    let crop_h = (rh.round().max(1.0) as u32).min(full_img.height - y_bottom);
    let top_y = full_img.height - (y_bottom + crop_h);

    let cropped = crop_rgba_rect(full_img, x0, top_y, crop_w, crop_h)?;
    let final_img = undo_sprite_packing_rotation(cropped, resolved.settings_raw);

    Ok(DecodedSprite {
        name: sprite.name,
        image: DecodedImage {
            width: final_img.width(),
            height: final_img.height(),
            rgba8: final_img.into_raw(),
            format_name: format!("Sprite({})", full_img.format_name),
            mip_count: 1,
        },
        rect: [rx, ry, rw, rh],
        pivot: [sprite.pivot_x, sprite.pivot_y],
        pixels_to_units: sprite.pixels_to_units,
        source_texture: Some((resolved.tex_file_idx, resolved.tex_path_id)),
    })
}

fn crop_rgba_rect(
    src: &DecodedImage,
    x0: u32,
    top_y: u32,
    crop_w: u32,
    crop_h: u32,
) -> CodecResult<RgbaImage> {
    let expected = (src.width as usize)
        .checked_mul(src.height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| CodecError::MalformedData("atlas dimensions overflow".to_owned()))?;
    if src.rgba8.len() < expected {
        return Err(CodecError::MalformedData(
            "invalid RGBA buffer length".to_owned(),
        ));
    }
    let src_stride = (src.width as usize) * 4;
    let dst_stride = (crop_w as usize) * 4;
    let mut out = vec![0u8; dst_stride * (crop_h as usize)];
    for row in 0..(crop_h as usize) {
        let src_start = ((top_y as usize) + row) * src_stride + (x0 as usize) * 4;
        let dst_start = row * dst_stride;
        out[dst_start..dst_start + dst_stride]
            .copy_from_slice(&src.rgba8[src_start..src_start + dst_stride]);
    }
    RgbaImage::from_raw(crop_w, crop_h, out)
        .ok_or_else(|| CodecError::MalformedData("invalid cropped RGBA buffer length".to_owned()))
}

struct ResolvedSpriteSource {
    tex_file_idx: usize,
    tex_path_id: i64,
    alpha_tex: Option<(usize, i64)>,
    rect: (f32, f32, f32, f32),
    settings_raw: u32,
}

fn decode_workspace_texture2d(
    workspace: &UnityWorkspaceData,
    file_index: usize,
    path_id: i64,
    fallback_unity_version: &str,
) -> CodecResult<DecodedImage> {
    let (loaded, info) = workspace.object_info(file_index, path_id).ok_or_else(|| {
        CodecError::MalformedData(format!(
            "Texture2D m_PathID={path_id} not found in file #{file_index}"
        ))
    })?;
    let tex_obj = read_object(loaded, info)
        .map_err(|e| CodecError::MalformedData(format!("failed to read backing Texture2D: {e}")))?;
    let tex_ver = if !loaded.file.unity_version.is_empty() && !loaded.file.unity_version.starts_with("0.")
    {
        loaded.file.unity_version.as_str()
    } else {
        fallback_unity_version
    };

    // Decode upright (`flip_vertical: true`); Unity bottom-left `textureRect` Y is flipped below.
    decode_texture2d(
        &tex_obj,
        tex_ver,
        &workspace.streams,
        TextureDecodeOptions {
            flip_vertical: true,
            ..Default::default()
        },
    )
}

fn resolve_sprite_render_source(
    sprite_obj: &UnityObject,
    sprite: &unity_asset_decode::sprite::Sprite,
    sprite_file_index: usize,
    workspace: &UnityWorkspaceData,
) -> CodecResult<ResolvedSpriteSource> {
    let props = sprite_obj.class.properties();
    let rd = props.get("m_RD").and_then(|v| v.as_object());

    let (tex_file_id, mut tex_path_id) = rd
        .and_then(|m| extract_pptr(m.get("texture")))
        .unwrap_or((0, sprite.render_data.texture_path_id));
    if tex_path_id == 0 && sprite.render_data.texture_path_id != 0 {
        tex_path_id = sprite.render_data.texture_path_id;
    }

    if tex_path_id != 0 {
        let tex_file_idx = resolve_external_or_error(workspace, sprite_file_index, tex_file_id)?;
        let alpha_tex = rd
            .and_then(|m| extract_pptr(m.get("alphaTexture")))
            .filter(|&(_, pid)| pid != 0)
            .and_then(|(fid, pid)| {
                workspace
                    .resolve_external_file(sprite_file_index, fid)
                    .map(|idx| (idx, pid))
            });
        let direct_rect = rd.and_then(|m| extract_rect_fields(m.get("textureRect")));
        let rect = if let Some([x, y, w, h]) = direct_rect.filter(|r| r[2] > 0.0 && r[3] > 0.0) {
            (x, y, w, h)
        } else if sprite.render_data.texture_rect_width > 0.0
            && sprite.render_data.texture_rect_height > 0.0
        {
            (
                sprite.render_data.texture_rect_x,
                sprite.render_data.texture_rect_y,
                sprite.render_data.texture_rect_width,
                sprite.render_data.texture_rect_height,
            )
        } else {
            (sprite.rect_x, sprite.rect_y, sprite.rect_width, sprite.rect_height)
        };
        let settings_raw = rd
            .and_then(|m| m.get("settingsRaw"))
            .and_then(extract_i64_field)
            .unwrap_or(0) as u32;

        return Ok(ResolvedSpriteSource {
            tex_file_idx,
            tex_path_id,
            alpha_tex,
            rect,
            settings_raw,
        });
    }

    resolve_sprite_via_atlas(sprite_obj, sprite, sprite_file_index, workspace)
}

fn resolve_external_or_error(
    workspace: &UnityWorkspaceData,
    source_file_index: usize,
    m_file_id: i32,
) -> CodecResult<usize> {
    workspace
        .resolve_external_file(source_file_index, m_file_id)
        .ok_or_else(|| {
            if let Some(ext_name) = workspace.external_file_name(source_file_index, m_file_id) {
                CodecError::MissingStream(format!(
                    "external dependency `{ext_name}` (m_FileID={m_file_id}) is not loaded — load its matching .ab bundle first"
                ))
            } else {
                CodecError::MissingStream(format!(
                    "external file m_FileID={m_file_id} from file #{source_file_index}"
                ))
            }
        })
}

fn resolve_sprite_via_atlas(
    sprite_obj: &UnityObject,
    sprite: &unity_asset_decode::sprite::Sprite,
    sprite_file_index: usize,
    workspace: &UnityWorkspaceData,
) -> CodecResult<ResolvedSpriteSource> {
    let props = sprite_obj.class.properties();
    let render_data_key = parse_render_data_key(props.get("m_RenderDataKey"));
    let (atlas_fid, atlas_pid) = extract_pptr(props.get("m_SpriteAtlas")).unwrap_or((0, 0));

    // 1. Direct `m_SpriteAtlas` PPtr (same bundle or already-loaded dependency bundle).
    let mut unloaded_external_hint: Option<String> = None;
    if atlas_pid != 0 {
        if let Some(atlas_file_idx) = workspace.resolve_external_file(sprite_file_index, atlas_fid)
            && let Some((loaded, info)) = workspace.object_info(atlas_file_idx, atlas_pid)
            && let Ok(atlas_obj) = read_object(loaded, info)
            && let Some(resolved) = resolve_sprite_atlas_entry(
                &atlas_obj,
                atlas_file_idx,
                render_data_key,
                sprite_file_index,
                sprite_obj.path_id(),
                workspace,
            )?
        {
            return Ok(resolved);
        } else if atlas_fid != 0 {
            unloaded_external_hint = workspace
                .external_file_name(sprite_file_index, atlas_fid)
                .map(str::to_owned);
        }
    }

    // 2. Fallback search across loaded `SpriteAtlas` (687078895) objects:
    // prioritize the sprite's own file, then files matching `m_AtlasTags` / PathID, then the rest.
    let mut candidate_indices: Vec<usize> = Vec::with_capacity(workspace.files.len());
    if sprite_file_index < workspace.files.len() {
        candidate_indices.push(sprite_file_index);
    }
    let lower_tags: Vec<String> = sprite
        .atlas_tags
        .iter()
        .map(|t| t.to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    for (idx, file) in workspace.files.iter().enumerate() {
        if idx != sprite_file_index && !lower_tags.is_empty() {
            let vlower = file.virtual_path.to_ascii_lowercase();
            if lower_tags.iter().any(|tag| vlower.contains(tag)) {
                candidate_indices.push(idx);
            }
        }
    }
    for idx in 0..workspace.files.len() {
        if !candidate_indices.contains(&idx) {
            candidate_indices.push(idx);
        }
    }

    for file_idx in candidate_indices {
        let Some(loaded) = workspace.files.get(file_idx) else {
            continue;
        };
        for info in &loaded.file.objects {
            if info.type_id != 687078895 {
                continue;
            }
            let Ok(atlas_obj) = read_object(loaded, info) else {
                continue;
            };
            if let Some(resolved) = resolve_sprite_atlas_entry(
                &atlas_obj,
                file_idx,
                render_data_key,
                sprite_file_index,
                sprite_obj.path_id(),
                workspace,
            )? {
                return Ok(resolved);
            }
        }
    }

    if let Some(ext_name) = unloaded_external_hint {
        return Err(CodecError::MissingStream(format!(
            "Sprite '{}' uses external SpriteAtlas `{ext_name}` (m_PathID={atlas_pid}) which is not loaded — load matching .ab bundles in the Explorer",
            sprite.name
        )));
    }
    Err(CodecError::MalformedData(format!(
        "Sprite '{}' references null Texture2D (m_PathID = 0) and no matching SpriteAtlas entry was found",
        sprite.name
    )))
}

fn resolve_sprite_atlas_entry(
    atlas_obj: &UnityObject,
    atlas_file_idx: usize,
    sprite_key: Option<([u32; 4], i64)>,
    sprite_file_idx: usize,
    sprite_path_id: i64,
    workspace: &UnityWorkspaceData,
) -> CodecResult<Option<ResolvedSpriteSource>> {
    let props = atlas_obj.class.properties();
    let Some(render_map) = props.get("m_RenderDataMap").and_then(|v| v.as_array()) else {
        return Ok(None);
    };

    let mut matched_val: Option<&UnityValue> = None;
    let mut guid_only_match: Option<&UnityValue> = None;
    let mut guid_only_ambiguous = false;

    if let Some((want_guid, want_sub)) = sprite_key {
        for pair_val in render_map {
            let Some(pair) = pair_val.as_array() else {
                continue;
            };
            if pair.len() < 2 {
                continue;
            }
            let Some((entry_guid, entry_sub)) = parse_render_data_key(Some(&pair[0])) else {
                continue;
            };
            if entry_guid == want_guid {
                if entry_sub == want_sub {
                    matched_val = Some(&pair[1]);
                    break;
                }
                if guid_only_match.is_some() {
                    guid_only_ambiguous = true;
                } else {
                    guid_only_match = Some(&pair[1]);
                }
            }
        }
        if matched_val.is_none() && !guid_only_ambiguous {
            matched_val = guid_only_match;
        }
    }

    // Single-sprite atlas fallback when `m_RenderDataKey` is missing/zeroed.
    if matched_val.is_none()
        && render_map.len() == 1
        && sprite_path_id != 0
        && let Some(packed) = props.get("m_PackedSprites").and_then(|v| v.as_array())
        && packed.iter().any(|p| {
            extract_pptr(Some(p)).is_some_and(|(fid, pid)| {
                pid == sprite_path_id
                    && workspace.resolve_external_file(atlas_file_idx, fid) == Some(sprite_file_idx)
            })
        })
        && let Some(pair) = render_map[0].as_array()
        && pair.len() >= 2
    {
        matched_val = Some(&pair[1]);
    }

    let Some(entry_obj) = matched_val.and_then(|v| v.as_object()) else {
        return Ok(None);
    };
    let Some((tex_fid, tex_pid)) = extract_pptr(entry_obj.get("texture")) else {
        return Ok(None);
    };
    if tex_pid == 0 {
        return Ok(None);
    }

    let tex_file_idx = resolve_external_or_error(workspace, atlas_file_idx, tex_fid)?;
    let alpha_tex = extract_pptr(entry_obj.get("alphaTexture"))
        .filter(|&(_, pid)| pid != 0)
        .and_then(|(fid, pid)| {
            workspace
                .resolve_external_file(atlas_file_idx, fid)
                .map(|idx| (idx, pid))
        });
    let [rx, ry, rw, rh] = extract_rect_fields(entry_obj.get("textureRect")).unwrap_or([0.0; 4]);
    let settings_raw = entry_obj
        .get("settingsRaw")
        .and_then(extract_i64_field)
        .unwrap_or(0) as u32;

    Ok(Some(ResolvedSpriteSource {
        tex_file_idx,
        tex_path_id: tex_pid,
        alpha_tex,
        rect: (rx, ry, rw, rh),
        settings_raw,
    }))
}

fn apply_split_alpha_texture(
    color_img: &mut DecodedImage,
    workspace: &UnityWorkspaceData,
    alpha_file_idx: usize,
    alpha_path_id: i64,
    fallback_unity_version: &str,
) -> CodecResult<()> {
    let alpha_img = decode_workspace_texture2d(
        workspace,
        alpha_file_idx,
        alpha_path_id,
        fallback_unity_version,
    )?;
    if alpha_img.width == 0 || alpha_img.height == 0 {
        return Ok(());
    }
    let alpha_resized = if alpha_img.width == color_img.width && alpha_img.height == color_img.height {
        alpha_img.rgba8
    } else if let Some(buf) = RgbaImage::from_raw(alpha_img.width, alpha_img.height, alpha_img.rgba8) {
        imageops::resize(
            &buf,
            color_img.width,
            color_img.height,
            imageops::FilterType::Triangle,
        )
        .into_raw()
    } else {
        return Ok(());
    };
    for (dst, src) in color_img
        .rgba8
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(alpha_resized.as_chunks::<4>().0.iter())
    {
        dst[3] = src[0];
    }
    Ok(())
}

fn undo_sprite_packing_rotation(cropped: RgbaImage, settings_raw: u32) -> RgbaImage {
    let packing_rotation = (settings_raw >> 2) & 0xF;
    match packing_rotation {
        1 => imageops::flip_horizontal(&cropped),
        2 => imageops::flip_vertical(&cropped),
        3 => imageops::rotate180(&cropped),
        4 => imageops::rotate270(&cropped),
        _ => cropped,
    }
}

fn extract_pptr(val: Option<&UnityValue>) -> Option<(i32, i64)> {
    let obj = val?.as_object()?;
    let pid = obj.get("m_PathID").and_then(extract_i64_field).unwrap_or(0);
    let fid = obj.get("m_FileID").and_then(extract_i32_field).unwrap_or(0);
    Some((fid, pid))
}

fn extract_f32_field(val: &UnityValue) -> Option<f32> {
    match val {
        UnityValue::Float(f) => Some(*f as f32),
        UnityValue::Integer(i) => Some(*i as f32),
        _ => None,
    }
}

fn extract_rect_fields(val: Option<&UnityValue>) -> Option<[f32; 4]> {
    let obj = val?.as_object()?;
    let x = obj.get("x").and_then(extract_f32_field).unwrap_or(0.0);
    let y = obj.get("y").and_then(extract_f32_field).unwrap_or(0.0);
    let w = obj.get("width").and_then(extract_f32_field).unwrap_or(0.0);
    let h = obj.get("height").and_then(extract_f32_field).unwrap_or(0.0);
    Some([x, y, w, h])
}

fn parse_guid_words(val: &UnityValue) -> Option<[u32; 4]> {
    match val {
        UnityValue::Object(map) => {
            let d0 = map.get("data[0]").and_then(extract_i64_field)? as u64 as u32;
            let d1 = map.get("data[1]").and_then(extract_i64_field)? as u64 as u32;
            let d2 = map.get("data[2]").and_then(extract_i64_field)? as u64 as u32;
            let d3 = map.get("data[3]").and_then(extract_i64_field)? as u64 as u32;
            Some([d0, d1, d2, d3])
        }
        UnityValue::Array(items) if items.len() == 4 => {
            let d0 = items[0].as_i64()? as u64 as u32;
            let d1 = items[1].as_i64()? as u64 as u32;
            let d2 = items[2].as_i64()? as u64 as u32;
            let d3 = items[3].as_i64()? as u64 as u32;
            Some([d0, d1, d2, d3])
        }
        UnityValue::Bytes(bytes) if bytes.len() == 16 => Some([
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        ]),
        _ => None,
    }
}

fn parse_render_data_key(val: Option<&UnityValue>) -> Option<([u32; 4], i64)> {
    let pair = val?.as_array()?;
    if pair.len() < 2 {
        return None;
    }
    let guid = parse_guid_words(&pair[0])?;
    let sub_id = extract_i64_field(&pair[1])?;
    Some((guid, sub_id))
}

/// Produces a channel-isolated RGBA8 preview (`Red`, `Green`, `Blue`, or grayscale `Alpha`).
pub fn apply_channel_mask(img: &DecodedImage, mask: ChannelMask) -> DecodedImage {
    if mask == ChannelMask::All {
        return img.clone();
    }
    let mut out = img.rgba8.clone();
    for px in out.as_chunks_mut::<4>().0 {
        match mask {
            ChannelMask::All => {}
            ChannelMask::Red => {
                px[1] = 0;
                px[2] = 0;
                px[3] = 255;
            }
            ChannelMask::Green => {
                px[0] = 0;
                px[2] = 0;
                px[3] = 255;
            }
            ChannelMask::Blue => {
                px[0] = 0;
                px[1] = 0;
                px[3] = 255;
            }
            ChannelMask::Alpha => {
                let a = px[3];
                px[0] = a;
                px[1] = a;
                px[2] = a;
                px[3] = 255;
            }
        }
    }
    DecodedImage {
        width: img.width,
        height: img.height,
        rgba8: out,
        format_name: img.format_name.clone(),
        mip_count: img.mip_count,
    }
}

/// Encodes a [`DecodedImage`] to PNG bytes.
pub fn encode_png(img: &DecodedImage) -> CodecResult<Vec<u8>> {
    encode_with_format(img, ImageFormat::Png)
}

/// Encodes a [`DecodedImage`] to TGA bytes.
pub fn encode_tga(img: &DecodedImage) -> CodecResult<Vec<u8>> {
    encode_with_format(img, ImageFormat::Tga)
}

/// Encodes a [`DecodedImage`] to JPEG bytes (`quality` clamped to `1..=100`).
pub fn encode_jpeg(img: &DecodedImage, quality: u8) -> CodecResult<Vec<u8>> {
    let rgba = RgbaImage::from_raw(img.width, img.height, img.rgba8.clone())
        .ok_or_else(|| CodecError::MalformedData("invalid RGBA buffer length".to_owned()))?;
    let rgb = DynamicImage::ImageRgba8(rgba).to_rgb8();
    let mut buf = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality.clamp(1, 100));
    enc.encode_image(&rgb)?;
    Ok(buf)
}

/// Encodes a [`DecodedImage`] to BMP bytes.
pub fn encode_bmp(img: &DecodedImage) -> CodecResult<Vec<u8>> {
    encode_with_format(img, ImageFormat::Bmp)
}

/// Encodes a [`DecodedImage`] to lossless WebP bytes.
pub fn encode_webp(img: &DecodedImage) -> CodecResult<Vec<u8>> {
    encode_with_format(img, ImageFormat::WebP)
}

fn encode_with_format(img: &DecodedImage, fmt: ImageFormat) -> CodecResult<Vec<u8>> {
    let rgba = RgbaImage::from_raw(img.width, img.height, img.rgba8.clone())
        .ok_or_else(|| CodecError::MalformedData("invalid RGBA buffer length".to_owned()))?;
    let mut buf = Vec::new();
    DynamicImage::ImageRgba8(rgba).write_to(&mut Cursor::new(&mut buf), fmt)?;
    Ok(buf)
}

/// Builds a DDS file preserving raw DXT1/DXT5/BC4/BC5/BC6H/BC7 payload when present,
/// otherwise falling back to uncompressed BGRA8888 DDS from the decoded RGBA image.
pub fn encode_dds(raw_tex: Option<&Texture2D>, decoded: &DecodedImage) -> CodecResult<Vec<u8>> {
    if let Some(tex) = raw_tex
        && !tex.image_data.is_empty()
        && let Some(dds) = try_wrap_raw_dds(tex)
    {
        return Ok(dds);
    }
    Ok(build_uncompressed_bgra_dds(decoded))
}

fn try_wrap_raw_dds(tex: &Texture2D) -> Option<Vec<u8>> {
    let w = u32::try_from(tex.width).ok()?.max(1);
    let h = u32::try_from(tex.height).ok()?.max(1);
    let mips = u32::try_from(tex.mip_count).unwrap_or(1).max(1);

    enum DdsKind {
        FourCc([u8; 4]),
        Dx10(u32),
    }

    let kind = match tex.format {
        TextureFormat::DXT1 => DdsKind::FourCc(*b"DXT1"),
        TextureFormat::DXT5 => DdsKind::FourCc(*b"DXT5"),
        TextureFormat::BC4 => DdsKind::FourCc(*b"ATI1"),
        TextureFormat::BC5 => DdsKind::FourCc(*b"ATI2"),
        TextureFormat::BC6H => DdsKind::Dx10(95),
        TextureFormat::BC7 => DdsKind::Dx10(98),
        _ => return None,
    };

    let linear_size = ((w.div_ceil(4) * h.div_ceil(4))
        * match tex.format {
            TextureFormat::DXT1 | TextureFormat::BC4 => 8,
            _ => 16,
        })
    .min(tex.image_data.len() as u32);

    let mut out = Vec::with_capacity(148 + tex.image_data.len());
    out.extend_from_slice(b"DDS ");
    out.extend_from_slice(&124u32.to_le_bytes()); // dwSize
    let flags: u32 = 0x1 | 0x2 | 0x4 | 0x1000 | 0x80000 | if mips > 1 { 0x20000 } else { 0 };
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&linear_size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // dwDepth
    out.extend_from_slice(&mips.to_le_bytes());
    out.extend_from_slice(&[0u8; 44]); // dwReserved1[11]

    // DDS_PIXELFORMAT (32 bytes)
    out.extend_from_slice(&32u32.to_le_bytes());
    out.extend_from_slice(&0x4u32.to_le_bytes()); // DDPF_FOURCC
    match kind {
        DdsKind::FourCc(cc) => out.extend_from_slice(&cc),
        DdsKind::Dx10(_) => out.extend_from_slice(b"DX10"),
    }
    out.extend_from_slice(&[0u8; 20]);

    // caps
    let caps1 = 0x1000u32 | if mips > 1 { 0x400008 } else { 0 };
    out.extend_from_slice(&caps1.to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);

    if let DdsKind::Dx10(dxgi_format) = kind {
        out.extend_from_slice(&dxgi_format.to_le_bytes());
        out.extend_from_slice(&3u32.to_le_bytes()); // D3D10_RESOURCE_DIMENSION_TEXTURE2D
        out.extend_from_slice(&0u32.to_le_bytes()); // miscFlag
        out.extend_from_slice(&1u32.to_le_bytes()); // arraySize
        out.extend_from_slice(&0u32.to_le_bytes()); // miscFlags2
    }

    out.extend_from_slice(&tex.image_data);
    Some(out)
}

fn build_uncompressed_bgra_dds(img: &DecodedImage) -> Vec<u8> {
    let w = img.width.max(1);
    let h = img.height.max(1);
    let pitch = w * 4;
    let mut out = Vec::with_capacity(128 + (pitch * h) as usize);
    out.extend_from_slice(b"DDS ");
    out.extend_from_slice(&124u32.to_le_bytes());
    out.extend_from_slice(&(0x1u32 | 0x2 | 0x4 | 0x8 | 0x1000).to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&pitch.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 44]);

    // DDS_PIXELFORMAT BGRA8888
    out.extend_from_slice(&32u32.to_le_bytes());
    out.extend_from_slice(&0x41u32.to_le_bytes()); // DDPF_RGB | DDPF_ALPHAPIXELS
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes());
    out.extend_from_slice(&0x00FF_0000u32.to_le_bytes());
    out.extend_from_slice(&0x0000_FF00u32.to_le_bytes());
    out.extend_from_slice(&0x0000_00FFu32.to_le_bytes());
    out.extend_from_slice(&0xFF00_0000u32.to_le_bytes());

    out.extend_from_slice(&0x1000u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);

    for px in img.rgba8.as_chunks::<4>().0 {
        out.push(px[2]);
        out.push(px[1]);
        out.push(px[0]);
        out.push(px[3]);
    }
    out
}

/// Human-readable Unity `TextureFormat` name from numeric format ID (`m_TextureFormat`).
pub fn texture_format_name(format_id: i32) -> &'static str {
    match format_id {
        1 => "Alpha8",
        2 => "ARGB4444",
        3 => "RGB24",
        4 => "RGBA32",
        5 => "ARGB32",
        7 => "RGB565",
        9 => "R16",
        10 => "DXT1",
        11 => "DXT3",
        12 => "DXT5",
        13 => "RGBA4444",
        14 => "BGRA32",
        15 => "RHalf",
        16 => "RGHalf",
        17 => "RGBAHalf",
        18 => "RFloat",
        19 => "RGFloat",
        20 => "RGBAFloat",
        21 => "YUY2",
        22 => "RGB9e5Float",
        24 => "BC6H",
        25 => "BC7",
        26 => "BC4",
        27 => "BC5",
        28 => "DXT1Crunched",
        29 => "DXT5Crunched",
        30 => "PVRTC_RGB2",
        31 => "PVRTC_RGBA2",
        32 => "PVRTC_RGB4",
        33 => "PVRTC_RGBA4",
        34 => "ETC_RGB4",
        35 => "ATC_RGB4",
        36 => "ATC_RGBA8",
        41 => "EAC_R",
        42 => "EAC_R_SIGNED",
        43 => "EAC_RG",
        44 => "EAC_RG_SIGNED",
        45 => "ETC2_RGB",
        46 => "ETC2_RGBA1",
        47 => "ETC2_RGBA8",
        48 => "ASTC_RGB_4x4",
        49 => "ASTC_RGB_5x5",
        50 => "ASTC_RGB_6x6",
        51 => "ASTC_RGB_8x8",
        52 => "ASTC_RGB_10x10",
        53 => "ASTC_RGB_12x12",
        54 => "ASTC_RGBA_4x4",
        55 => "ASTC_RGBA_5x5",
        56 => "ASTC_RGBA_6x6",
        57 => "ASTC_RGBA_8x8",
        58 => "ASTC_RGBA_10x10",
        59 => "ASTC_RGBA_12x12",
        60 => "ETC_RGB4_3DS",
        61 => "ETC2_RGBA8_3DS",
        62 => "RG16",
        63 => "R8",
        64 => "ETC_RGB4Crunched",
        65 => "ETC2_RGBA8Crunched",
        66 => "ASTC_HDR_4x4",
        67 => "ASTC_HDR_5x5",
        68 => "ASTC_HDR_6x6",
        69 => "ASTC_HDR_8x8",
        70 => "ASTC_HDR_10x10",
        71 => "ASTC_HDR_12x12",
        72 => "RG32",
        73 => "RGB48",
        74 => "RGBA64",
        _ => "Unknown",
    }
}

/// Decodes raw Unity texture mip0 pixel payload across all supported compressed, ASTC, ETC,
/// PVRTC, ATC, BCn, Crunched, uncompressed, half-float, and float formats into `RGBA8`.
pub fn decode_texture_bytes(
    format_id: i32,
    width: u32,
    height: u32,
    data: &[u8],
) -> CodecResult<RgbaImage> {
    let w = width as usize;
    let h = height as usize;
    let fmt_name = texture_format_name(format_id);

    if let Some((bw, bh)) = astc_block_size(format_id) {
        let mip0 = slice_block_mip0(data, w, h, bw, bh, 16, fmt_name)?;
        return run_u32_decoder(w, h, fmt_name, |out| {
            texture2ddecoder::decode_astc(mip0, w, h, bw, bh, out)
        });
    }

    match format_id {
        // Basic integer formats
        1 | 63 => decode_single_channel_u8(w, h, data, format_id == 1, fmt_name),
        62 => decode_rg16(w, h, data),
        9 => decode_r16(w, h, data),
        72 => decode_rg32(w, h, data),
        73 => decode_rgb48(w, h, data),
        74 => decode_rgba64(w, h, data),
        2 => decode_4444(w, h, data, true),
        13 => decode_4444(w, h, data, false),
        7 => decode_rgb565(w, h, data),
        3 => decode_rgb24(w, h, data),
        4 | 5 | 14 => decode_rgba_order_8bit(w, h, data, format_id),
        // Half / Float / Special formats
        15 => decode_half_channels(w, h, data, 1, fmt_name),
        16 => decode_half_channels(w, h, data, 2, fmt_name),
        17 => decode_half_channels(w, h, data, 4, fmt_name),
        18 => decode_float_channels(w, h, data, 1, fmt_name),
        19 => decode_float_channels(w, h, data, 2, fmt_name),
        20 => decode_float_channels(w, h, data, 4, fmt_name),
        21 => decode_yuy2(w, h, data),
        22 => decode_rgb9e5(w, h, data),
        // Desktop BCn / DXT formats
        10 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 8, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| texture2ddecoder::decode_bc1(mip0, w, h, out))
        }
        11 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 16, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| texture2ddecoder::decode_bc2(mip0, w, h, out))
        }
        12 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 16, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| texture2ddecoder::decode_bc3(mip0, w, h, out))
        }
        24 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 16, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| {
                texture2ddecoder::decode_bc6_unsigned(mip0, w, h, out)
            })
        }
        25 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 16, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| texture2ddecoder::decode_bc7(mip0, w, h, out))
        }
        26 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 8, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| texture2ddecoder::decode_bc4(mip0, w, h, out))
        }
        27 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 16, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| texture2ddecoder::decode_bc5(mip0, w, h, out))
        }
        // Crunched formats
        28 | 29 | 64 | 65 => run_u32_decoder(w, h, fmt_name, |out| {
            texture2ddecoder::decode_unity_crunch(data, w, h, out)
                .or_else(|_| texture2ddecoder::decode_crunch(data, w, h, out))
        }),
        // Mobile PVRTC / ETC / ATC / EAC
        30 | 31 => run_u32_decoder(w, h, fmt_name, |out| {
            texture2ddecoder::decode_pvrtc_2bpp(data, w, h, out)
        }),
        32 | 33 => run_u32_decoder(w, h, fmt_name, |out| {
            texture2ddecoder::decode_pvrtc_4bpp(data, w, h, out)
        }),
        34 | 60 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 8, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| texture2ddecoder::decode_etc1(mip0, w, h, out))
        }
        35 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 8, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| {
                texture2ddecoder::decode_atc_rgb4(mip0, w, h, out)
            })
        }
        36 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 16, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| {
                texture2ddecoder::decode_atc_rgba8(mip0, w, h, out)
            })
        }
        41 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 8, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| texture2ddecoder::decode_eacr(mip0, w, h, out))
        }
        42 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 8, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| {
                texture2ddecoder::decode_eacr_signed(mip0, w, h, out)
            })
        }
        43 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 16, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| {
                texture2ddecoder::decode_eacrg(mip0, w, h, out)
            })
        }
        44 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 16, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| {
                texture2ddecoder::decode_eacrg_signed(mip0, w, h, out)
            })
        }
        45 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 8, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| {
                texture2ddecoder::decode_etc2_rgb(mip0, w, h, out)
            })
        }
        46 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 8, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| {
                texture2ddecoder::decode_etc2_rgba1(mip0, w, h, out)
            })
        }
        47 | 61 => {
            let mip0 = slice_block_mip0(data, w, h, 4, 4, 16, fmt_name)?;
            run_u32_decoder(w, h, fmt_name, |out| {
                texture2ddecoder::decode_etc2_rgba8(mip0, w, h, out)
            })
        }
        _ => Err(CodecError::UnsupportedFormat(format!(
            "{fmt_name} (id={format_id})"
        ))),
    }
}

fn astc_block_size(format_id: i32) -> Option<(usize, usize)> {
    match format_id {
        48 | 54 | 66 => Some((4, 4)),
        49 | 55 | 67 => Some((5, 5)),
        50 | 56 | 68 => Some((6, 6)),
        51 | 57 | 69 => Some((8, 8)),
        52 | 58 | 70 => Some((10, 10)),
        53 | 59 | 71 => Some((12, 12)),
        _ => None,
    }
}

fn slice_block_mip0<'a>(
    data: &'a [u8],
    w: usize,
    h: usize,
    bw: usize,
    bh: usize,
    bytes_per_block: usize,
    fmt_name: &str,
) -> CodecResult<&'a [u8]> {
    let needed = w.div_ceil(bw) * h.div_ceil(bh) * bytes_per_block;
    if data.len() < needed {
        return Err(CodecError::MalformedData(format!(
            "{fmt_name}: expected at least {needed} bytes for mip0 ({w}x{h}), got {}",
            data.len()
        )));
    }
    Ok(&data[..needed])
}

fn run_u32_decoder<F>(w: usize, h: usize, fmt_name: &str, decode_fn: F) -> CodecResult<RgbaImage>
where
    F: FnOnce(&mut [u32]) -> Result<(), &'static str>,
{
    let mut pixels = vec![0u32; w * h];
    decode_fn(&mut pixels)
        .map_err(|e| CodecError::MalformedData(format!("{fmt_name} decode failed: {e}")))?;
    // `texture2ddecoder::color::color` packs bytes as little-endian `[B, G, R, A]`.
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in pixels {
        let [b, g, r, a] = px.to_le_bytes();
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    RgbaImage::from_raw(w as u32, h as u32, rgba)
        .ok_or_else(|| CodecError::MalformedData("decoded pixel buffer size mismatch".into()))
}

fn check_pixel_bytes<'a>(
    data: &'a [u8],
    w: usize,
    h: usize,
    bpp: usize,
    fmt_name: &str,
) -> CodecResult<&'a [u8]> {
    let needed = w * h * bpp;
    if data.len() < needed {
        return Err(CodecError::MalformedData(format!(
            "{fmt_name}: expected at least {needed} bytes, got {}",
            data.len()
        )));
    }
    Ok(&data[..needed])
}

fn rgba_from_vec(w: usize, h: usize, rgba: Vec<u8>) -> CodecResult<RgbaImage> {
    RgbaImage::from_raw(w as u32, h as u32, rgba)
        .ok_or_else(|| CodecError::MalformedData("pixel buffer size mismatch".into()))
}

fn decode_single_channel_u8(
    w: usize,
    h: usize,
    data: &[u8],
    is_alpha8: bool,
    fmt_name: &str,
) -> CodecResult<RgbaImage> {
    let slice = check_pixel_bytes(data, w, h, 1, fmt_name)?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for &v in slice {
        if is_alpha8 {
            rgba.extend_from_slice(&[255, 255, 255, v]);
        } else {
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    rgba_from_vec(w, h, rgba)
}

fn decode_rg16(w: usize, h: usize, data: &[u8]) -> CodecResult<RgbaImage> {
    let slice = check_pixel_bytes(data, w, h, 2, "RG16")?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in slice.as_chunks::<2>().0 {
        rgba.extend_from_slice(&[px[0], px[1], 0, 255]);
    }
    rgba_from_vec(w, h, rgba)
}

fn decode_r16(w: usize, h: usize, data: &[u8]) -> CodecResult<RgbaImage> {
    let slice = check_pixel_bytes(data, w, h, 2, "R16")?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in slice.as_chunks::<2>().0 {
        let v = (u16::from_le_bytes(*px) >> 8) as u8;
        rgba.extend_from_slice(&[v, v, v, 255]);
    }
    rgba_from_vec(w, h, rgba)
}

fn decode_rg32(w: usize, h: usize, data: &[u8]) -> CodecResult<RgbaImage> {
    let slice = check_pixel_bytes(data, w, h, 4, "RG32")?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in slice.as_chunks::<4>().0 {
        let r = (u16::from_le_bytes([px[0], px[1]]) >> 8) as u8;
        let g = (u16::from_le_bytes([px[2], px[3]]) >> 8) as u8;
        rgba.extend_from_slice(&[r, g, 0, 255]);
    }
    rgba_from_vec(w, h, rgba)
}

fn decode_rgb48(w: usize, h: usize, data: &[u8]) -> CodecResult<RgbaImage> {
    let slice = check_pixel_bytes(data, w, h, 6, "RGB48")?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in slice.as_chunks::<6>().0 {
        rgba.extend_from_slice(&[
            (u16::from_le_bytes([px[0], px[1]]) >> 8) as u8,
            (u16::from_le_bytes([px[2], px[3]]) >> 8) as u8,
            (u16::from_le_bytes([px[4], px[5]]) >> 8) as u8,
            255,
        ]);
    }
    rgba_from_vec(w, h, rgba)
}

fn decode_rgba64(w: usize, h: usize, data: &[u8]) -> CodecResult<RgbaImage> {
    let slice = check_pixel_bytes(data, w, h, 8, "RGBA64")?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in slice.as_chunks::<8>().0 {
        rgba.extend_from_slice(&[
            (u16::from_le_bytes([px[0], px[1]]) >> 8) as u8,
            (u16::from_le_bytes([px[2], px[3]]) >> 8) as u8,
            (u16::from_le_bytes([px[4], px[5]]) >> 8) as u8,
            (u16::from_le_bytes([px[6], px[7]]) >> 8) as u8,
        ]);
    }
    rgba_from_vec(w, h, rgba)
}

fn decode_4444(w: usize, h: usize, data: &[u8], is_argb: bool) -> CodecResult<RgbaImage> {
    let slice = check_pixel_bytes(data, w, h, 2, if is_argb { "ARGB4444" } else { "RGBA4444" })?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in slice.as_chunks::<2>().0 {
        let v = u16::from_le_bytes(*px);
        let (n3, n2, n1, n0) = (
            ((v >> 12) & 0xF) as u8,
            ((v >> 8) & 0xF) as u8,
            ((v >> 4) & 0xF) as u8,
            (v & 0xF) as u8,
        );
        let (r, g, b, a) = if is_argb { (n2, n1, n0, n3) } else { (n3, n2, n1, n0) };
        rgba.extend_from_slice(&[(r << 4) | r, (g << 4) | g, (b << 4) | b, (a << 4) | a]);
    }
    rgba_from_vec(w, h, rgba)
}

fn decode_rgb565(w: usize, h: usize, data: &[u8]) -> CodecResult<RgbaImage> {
    let slice = check_pixel_bytes(data, w, h, 2, "RGB565")?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in slice.as_chunks::<2>().0 {
        let v = u16::from_le_bytes(*px);
        let r5 = ((v >> 11) & 0x1F) as u8;
        let g6 = ((v >> 5) & 0x3F) as u8;
        let b5 = (v & 0x1F) as u8;
        rgba.extend_from_slice(&[
            (r5 << 3) | (r5 >> 2),
            (g6 << 2) | (g6 >> 4),
            (b5 << 3) | (b5 >> 2),
            255,
        ]);
    }
    rgba_from_vec(w, h, rgba)
}

fn decode_rgb24(w: usize, h: usize, data: &[u8]) -> CodecResult<RgbaImage> {
    let slice = check_pixel_bytes(data, w, h, 3, "RGB24")?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in slice.as_chunks::<3>().0 {
        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    rgba_from_vec(w, h, rgba)
}

fn decode_rgba_order_8bit(w: usize, h: usize, data: &[u8], format_id: i32) -> CodecResult<RgbaImage> {
    let slice = check_pixel_bytes(data, w, h, 4, texture_format_name(format_id))?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in slice.as_chunks::<4>().0 {
        match format_id {
            5 => rgba.extend_from_slice(&[px[1], px[2], px[3], px[0]]),  // ARGB32
            14 => rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]), // BGRA32
            _ => rgba.extend_from_slice(px.as_slice()),                  // RGBA32
        }
    }
    rgba_from_vec(w, h, rgba)
}

fn float_to_u8(val: f32) -> u8 {
    if !val.is_finite() {
        return 0;
    }
    (val.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn f16_bits_to_f32(bits: u16) -> f32 {
    let sign = ((bits & 0x8000) as u32) << 16;
    let exp = ((bits >> 10) & 0x1F) as u32;
    let mant = (bits & 0x03FF) as u32;
    let f32_bits = if exp == 0 {
        if mant == 0 {
            sign
        } else {
            let mut m = mant;
            let mut e = 127 - 15 + 1;
            while (m & 0x0400) == 0 {
                m <<= 1;
                e -= 1;
            }
            m &= 0x03FF;
            sign | (e << 23) | (m << 13)
        }
    } else if exp == 31 {
        sign | 0x7F80_0000 | (mant << 13)
    } else {
        sign | ((exp + 127 - 15) << 23) | (mant << 13)
    };
    f32::from_bits(f32_bits)
}

fn decode_half_channels(
    w: usize,
    h: usize,
    data: &[u8],
    channels: usize,
    fmt_name: &str,
) -> CodecResult<RgbaImage> {
    let slice = check_pixel_bytes(data, w, h, channels * 2, fmt_name)?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in slice.chunks_exact(channels * 2) {
        let mut ch = [0u8, 0, 0, 255];
        for i in 0..channels {
            ch[i] = float_to_u8(f16_bits_to_f32(u16::from_le_bytes([px[i * 2], px[i * 2 + 1]])));
        }
        if channels == 1 {
            ch[1] = ch[0];
            ch[2] = ch[0];
        }
        rgba.extend_from_slice(&ch);
    }
    rgba_from_vec(w, h, rgba)
}

fn decode_float_channels(
    w: usize,
    h: usize,
    data: &[u8],
    channels: usize,
    fmt_name: &str,
) -> CodecResult<RgbaImage> {
    let slice = check_pixel_bytes(data, w, h, channels * 4, fmt_name)?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in slice.chunks_exact(channels * 4) {
        let mut ch = [0u8, 0, 0, 255];
        for i in 0..channels {
            let f = f32::from_le_bytes([px[i * 4], px[i * 4 + 1], px[i * 4 + 2], px[i * 4 + 3]]);
            ch[i] = float_to_u8(f);
        }
        if channels == 1 {
            ch[1] = ch[0];
            ch[2] = ch[0];
        }
        rgba.extend_from_slice(&ch);
    }
    rgba_from_vec(w, h, rgba)
}

fn decode_yuy2(w: usize, h: usize, data: &[u8]) -> CodecResult<RgbaImage> {
    let row_bytes = w.div_ceil(2) * 4;
    let needed = row_bytes * h;
    if data.len() < needed {
        return Err(CodecError::MalformedData(format!(
            "YUY2: expected {needed} bytes, got {}",
            data.len()
        )));
    }
    let yuv_to_rgb = |y: u8, u: u8, v: u8| -> [u8; 4] {
        let c = y as i32 - 16;
        let d = u as i32 - 128;
        let e = v as i32 - 128;
        let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
        let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
        let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
        [r, g, b, 255]
    };
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = &data[y * row_bytes..(y + 1) * row_bytes];
        let mut x = 0usize;
        for pair in row.as_chunks::<4>().0 {
            if x < w {
                rgba.extend_from_slice(&yuv_to_rgb(pair[0], pair[1], pair[3]));
                x += 1;
            }
            if x < w {
                rgba.extend_from_slice(&yuv_to_rgb(pair[2], pair[1], pair[3]));
                x += 1;
            }
        }
    }
    rgba_from_vec(w, h, rgba)
}

fn decode_rgb9e5(w: usize, h: usize, data: &[u8]) -> CodecResult<RgbaImage> {
    let slice = check_pixel_bytes(data, w, h, 4, "RGB9e5Float")?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in slice.as_chunks::<4>().0 {
        let v = u32::from_le_bytes(*px);
        let exp = ((v >> 27) & 0x1F) as i32 - 15 - 9;
        let scale = 2.0f32.powi(exp);
        let r = float_to_u8((v & 0x1FF) as f32 * scale);
        let g = float_to_u8(((v >> 9) & 0x1FF) as f32 * scale);
        let b = float_to_u8(((v >> 18) & 0x1FF) as f32 * scale);
        rgba.extend_from_slice(&[r, g, b, 255]);
    }
    rgba_from_vec(w, h, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_png_and_bgra_dds_roundtrip() {
        let img = DecodedImage {
            width: 2,
            height: 2,
            rgba8: vec![
                255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 64, 255, 255, 255, 0,
            ],
            format_name: "RGBA32".into(),
            mip_count: 1,
        };
        let png = encode_png(&img).expect("png encode");
        assert!(png.starts_with(b"\x89PNG"));

        let dds = encode_dds(None, &img).expect("dds encode");
        assert!(dds.starts_with(b"DDS "));
        assert_eq!(dds.len(), 128 + 16);

        let masked = apply_channel_mask(&img, ChannelMask::Alpha);
        assert_eq!(&masked.rgba8[4..8], &[128, 128, 128, 255]);
    }

    #[test]
    fn decodes_astc_rgb_6x6_without_red_blue_swap() {
        // ASTC void-extent (solid color) block:
        // header = 0x0DFC (void-extent LDR + reserved 1s), min/max extent coords = 0xFFFF * 4,
        // R=0xFFFF, G=0x0000, B=0x0000, A=0xFFFF (pure opaque red).
        let void_extent_red: [u8; 16] = [
            0xFC, 0xFD, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // void-extent header + extents
            0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, // RGBA16: (FFFF, 0000, 0000, FFFF)
        ];
        // Test format ID 50 = ASTC_RGB_6x6 across 6x6 block
        let decoded = decode_texture_bytes(50, 6, 6, &void_extent_red).expect("ASTC_RGB_6x6 decode");
        assert_eq!(decoded.width(), 6);
        assert_eq!(decoded.height(), 6);
        let px = decoded.get_pixel(0, 0).0;
        assert_eq!(px, [255, 0, 0, 255], "red channel must NOT swap with blue");

        // Also test ASTC_RGBA_6x6 (56) and ASTC_HDR_6x6 (68) with NPOT dimensions + mip trail
        let mut with_mip = void_extent_red.to_vec();
        with_mip.extend_from_slice(&[0u8; 16]);
        let npot = decode_texture_bytes(56, 5, 4, &with_mip).expect("ASTC_RGBA_6x6 NPOT");
        assert_eq!(npot.get_pixel(4, 3).0, [255, 0, 0, 255]);
        let hdr = decode_texture_bytes(68, 6, 6, &void_extent_red).expect("ASTC_HDR_6x6");
        assert_eq!(hdr.get_pixel(0, 0).0, [255, 0, 0, 255]);
    }

    #[test]
    fn decodes_dxt1_half_r8_and_rg16_with_true_channels() {
        // DXT1 solid red block: color0 = RGB565(31, 0, 0) = 0xF800, color1 = 0, indices = 0
        let dxt1_red: [u8; 8] = [0x00, 0xF8, 0x00, 0x00, 0, 0, 0, 0];
        let dxt1 = decode_texture_bytes(10, 4, 4, &dxt1_red).expect("DXT1 red");
        assert_eq!(dxt1.get_pixel(0, 0).0, [255, 0, 0, 255]);

        // RGBAHalf (17): 1.0 (0x3C00), 0.5 (0x3800), 0.0 (0x0000), 1.0 (0x3C00)
        let half_px = [
            0x00, 0x3C, 0x00, 0x38, 0x00, 0x00, 0x00, 0x3C,
        ];
        let half = decode_texture_bytes(17, 1, 1, &half_px).expect("RGBAHalf");
        assert_eq!(half.get_pixel(0, 0).0, [255, 128, 0, 255]);

        // R8 (63)
        let r8 = decode_texture_bytes(63, 1, 1, &[200]).expect("R8");
        assert_eq!(r8.get_pixel(0, 0).0, [200, 200, 200, 255]);

        // RG16 (62)
        let rg16 = decode_texture_bytes(62, 1, 1, &[12, 34]).expect("RG16");
        assert_eq!(rg16.get_pixel(0, 0).0, [12, 34, 0, 255]);
    }

    #[test]
    fn resolves_sprite_atlas_render_data_map_and_rotation() {
        use unity_asset_binary::asset::types::ObjectInfo;
        use unity_asset_core::UnityClass;

        let make_guid = |a: i64, b: i64, c: i64, d: i64| {
            let mut m = UnityClass::new(0, "GUID".into(), "0".into());
            m.set("data[0]".into(), UnityValue::Integer(a));
            m.set("data[1]".into(), UnityValue::Integer(b));
            m.set("data[2]".into(), UnityValue::Integer(c));
            m.set("data[3]".into(), UnityValue::Integer(d));
            UnityValue::Object(m.properties().clone())
        };

        let make_pptr = |fid: i64, pid: i64| {
            let mut m = UnityClass::new(0, "PPtr".into(), "0".into());
            m.set("m_FileID".into(), UnityValue::Integer(fid));
            m.set("m_PathID".into(), UnityValue::Integer(pid));
            UnityValue::Object(m.properties().clone())
        };

        let make_rect = |x: f64, y: f64, w: f64, h: f64| {
            let mut m = UnityClass::new(0, "Rectf".into(), "0".into());
            m.set("x".into(), UnityValue::Float(x));
            m.set("y".into(), UnityValue::Float(y));
            m.set("width".into(), UnityValue::Float(w));
            m.set("height".into(), UnityValue::Float(h));
            UnityValue::Object(m.properties().clone())
        };

        let key_pair = UnityValue::Array(vec![
            make_guid(432258629, 1325697250, 3168827285, 2604307760),
            UnityValue::Integer(21300000),
        ]);
        let parsed_key = parse_render_data_key(Some(&key_pair)).expect("render data key");
        assert_eq!(parsed_key.0, [432258629, 1325697250, 3168827285, 2604307760]);
        assert_eq!(parsed_key.1, 21300000);

        let mut atlas_val = UnityClass::new(687078895, "SpriteAtlas".into(), "999".into());
        let mut entry_data = UnityClass::new(0, "SpriteAtlasData".into(), "0".into());
        entry_data.set("texture".into(), make_pptr(0, -5112569003854411848));
        entry_data.set("alphaTexture".into(), make_pptr(0, 0));
        entry_data.set("textureRect".into(), make_rect(48.0, 204.0, 22.0, 21.0));
        entry_data.set("settingsRaw".into(), UnityValue::Integer(3));
        atlas_val.set(
            "m_RenderDataMap".into(),
            UnityValue::Array(vec![UnityValue::Array(vec![
                key_pair,
                UnityValue::Object(entry_data.properties().clone()),
            ])]),
        );

        let atlas_obj = UnityObject::from_info_and_class(
            ObjectInfo::new(999, 0, 0, 687078895, 0),
            atlas_val,
        );
        let ws = UnityWorkspaceData::new(std::path::PathBuf::from("test"));
        let resolved = resolve_sprite_atlas_entry(
            &atlas_obj,
            0,
            Some(parsed_key),
            0,
            4689285444076214296,
            &ws,
        )
        .expect("atlas lookup ok")
        .expect("entry matched");
        assert_eq!(resolved.tex_path_id, -5112569003854411848);
        assert_eq!(resolved.rect, (48.0, 204.0, 22.0, 21.0));
        assert_eq!(resolved.settings_raw, 3);

        // Verify crop + rotation un-transform: 2x3 cropped buffer rotated by 180 deg (rotation=3 -> bits (3 << 2) = 12).
        let atlas_img = DecodedImage {
            width: 4,
            height: 4,
            rgba8: vec![
                10, 0, 0, 255, 20, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0,
                30, 0, 0, 255, 40, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            format_name: "RGBA32".into(),
            mip_count: 1,
        };
        let cropped = crop_rgba_rect(&atlas_img, 0, 0, 2, 2).expect("crop");
        let rotated = undo_sprite_packing_rotation(cropped, 3 << 2);
        assert_eq!(rotated.get_pixel(0, 0).0, [40, 0, 0, 255]);
    }

    #[test]
    fn encodes_jpeg_bmp_and_webp() {
        let img = DecodedImage {
            width: 2,
            height: 2,
            rgba8: vec![
                255, 0, 0, 255, 0, 255, 0, 255,
                0, 0, 255, 255, 255, 255, 255, 255,
            ],
            format_name: "RGBA32".into(),
            mip_count: 1,
        };
        let jpg = encode_jpeg(&img, 90).expect("jpg");
        assert!(jpg.starts_with(&[0xFF, 0xD8, 0xFF]));

        let bmp = encode_bmp(&img).expect("bmp");
        assert!(bmp.starts_with(b"BM"));

        let webp = encode_webp(&img).expect("webp");
        assert!(webp.starts_with(b"RIFF"));
        assert_eq!(&webp[8..12], b"WEBP");
    }
}
