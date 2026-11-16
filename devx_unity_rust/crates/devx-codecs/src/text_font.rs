//! Decoders for Unity `TextAsset` (`class_id == 49`) and `Font` (`class_id == 128`) objects.

use crate::{CodecError, CodecResult};
use devx_unity_io::extract_bytes_field;
use unity_asset_binary::object::UnityObject;

/// Classified kind of a decoded `TextAsset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAssetKind {
    Json,
    Xml,
    Yaml,
    ShaderLab,
    Lua,
    CSharp,
    PlainText,
    Binary,
}

impl TextAssetKind {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Xml => "xml",
            Self::Yaml => "yaml",
            Self::ShaderLab => "shader",
            Self::Lua => "lua",
            Self::CSharp => "cs",
            Self::PlainText => "txt",
            Self::Binary => "bytes",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::Xml => "XML",
            Self::Yaml => "YAML",
            Self::ShaderLab => "ShaderLab",
            Self::Lua => "Lua",
            Self::CSharp => "C#",
            Self::PlainText => "Text",
            Self::Binary => "Binary",
        }
    }
}

/// Decoded `TextAsset` content + heuristic format classification.
#[derive(Debug, Clone)]
pub struct DecodedTextAsset {
    pub name: String,
    pub raw_bytes: Vec<u8>,
    pub text: Option<String>,
    pub kind: TextAssetKind,
}

impl DecodedTextAsset {
    pub fn suggested_extension(&self) -> &'static str {
        self.kind.extension()
    }
}

/// Decodes a Unity `TextAsset` (`m_Script` bytes/string field) with raw binary fallback.
pub fn decode_text_asset(obj: &UnityObject) -> CodecResult<DecodedTextAsset> {
    let props = obj.class.properties();
    let name = props
        .get("m_Name")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .or_else(|| obj.name())
        .unwrap_or_else(|| "TextAsset".to_owned());

    let raw_bytes = if let Some(val) = props.get("m_Script") {
        if let Some(s) = val.as_str() {
            s.as_bytes().to_vec()
        } else if let Some(bytes) = extract_bytes_field(val) {
            bytes
        } else {
            obj.raw_data().to_vec()
        }
    } else {
        extract_unity_aligned_string_after_name(obj.raw_data()).unwrap_or_else(|| obj.raw_data().to_vec())
    };

    let text = match std::str::from_utf8(&raw_bytes) {
        Ok(s) => Some(s.to_owned()),
        Err(_) => None,
    };

    let kind = classify_text_payload(&name, text.as_deref(), &raw_bytes);
    Ok(DecodedTextAsset {
        name,
        raw_bytes,
        text,
        kind,
    })
}

fn classify_text_payload(name: &str, text: Option<&str>, raw: &[u8]) -> TextAssetKind {
    let Some(text) = text else {
        return TextAssetKind::Binary;
    };
    let control_ratio = text
        .bytes()
        .filter(|&b| b < 0x20 && !matches!(b, b'\n' | b'\r' | b'\t'))
        .count();
    if !text.is_empty() && control_ratio * 16 > text.len() {
        return TextAssetKind::Binary;
    }

    let lower_name = name.to_ascii_lowercase();
    let trimmed = text.trim_start();
    if lower_name.ends_with(".json") || trimmed.starts_with('{') || trimmed.starts_with('[') {
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
                return TextAssetKind::Json;
            }
        } else if lower_name.ends_with(".json") {
            return TextAssetKind::Json;
        }
    }
    if lower_name.ends_with(".xml") || lower_name.ends_with(".svg") || trimmed.starts_with('<') {
        return TextAssetKind::Xml;
    }
    if lower_name.ends_with(".shader") || trimmed.starts_with("Shader \"") {
        return TextAssetKind::ShaderLab;
    }
    if lower_name.ends_with(".lua") || trimmed.contains("local function ") {
        return TextAssetKind::Lua;
    }
    if lower_name.ends_with(".cs") || trimmed.contains("namespace ") && trimmed.contains("class ") {
        return TextAssetKind::CSharp;
    }
    if lower_name.ends_with(".yaml")
        || lower_name.ends_with(".yml")
        || trimmed.starts_with("%YAML")
        || trimmed.starts_with("---\n")
    {
        return TextAssetKind::Yaml;
    }
    if raw.is_empty() {
        return TextAssetKind::PlainText;
    }
    TextAssetKind::PlainText
}

fn extract_unity_aligned_string_after_name(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < 8 {
        return None;
    }
    let name_len = u32::from_le_bytes(raw[0..4].try_into().ok()?) as usize;
    let aligned_name = 4usize.checked_add((name_len + 3) & !3)?;
    if aligned_name + 4 > raw.len() {
        return None;
    }
    let body_len = u32::from_le_bytes(raw[aligned_name..aligned_name + 4].try_into().ok()?) as usize;
    let start = aligned_name + 4;
    let end = start.checked_add(body_len)?;
    if end > raw.len() {
        return None;
    }
    Some(raw[start..end].to_vec())
}

/// Container format for a decoded `Font` asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFormat {
    TrueType,
    OpenType,
    Woff,
    Woff2,
    BitmapOnly,
    Unknown,
}

impl FontFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::TrueType => "ttf",
            Self::OpenType => "otf",
            Self::Woff => "woff",
            Self::Woff2 => "woff2",
            Self::BitmapOnly | Self::Unknown => "bin",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TrueType => "TrueType (.ttf)",
            Self::OpenType => "OpenType (.otf)",
            Self::Woff => "WOFF",
            Self::Woff2 => "WOFF2",
            Self::BitmapOnly => "Unity Bitmap Font",
            Self::Unknown => "Unknown Font Blob",
        }
    }
}

/// Decoded Unity `Font` (`class_id == 128`).
#[derive(Debug, Clone)]
pub struct DecodedFont {
    pub name: String,
    pub font_names: Vec<String>,
    pub font_size: f32,
    pub data: Vec<u8>,
    pub format: FontFormat,
}

/// Extracts embedded font bytes and metadata from a Unity `Font` object.
pub fn decode_font(obj: &UnityObject) -> CodecResult<DecodedFont> {
    let props = obj.class.properties();
    let name = props
        .get("m_Name")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .or_else(|| obj.name())
        .unwrap_or_else(|| "Font".to_owned());

    let font_names = props
        .get("m_FontNames")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let font_size = props
        .get("m_FontSize")
        .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64)))
        .unwrap_or(0.0) as f32;

    let data = props
        .get("m_FontData")
        .and_then(extract_bytes_field)
        .unwrap_or_default();

    let format = detect_font_format(&data);
    if data.is_empty() && font_names.is_empty() && name.is_empty() {
        return Err(CodecError::MalformedData(
            "Font object contains neither m_FontData nor font metadata".to_owned(),
        ));
    }

    Ok(DecodedFont {
        name,
        font_names,
        font_size,
        data,
        format,
    })
}

fn detect_font_format(data: &[u8]) -> FontFormat {
    if data.is_empty() {
        return FontFormat::BitmapOnly;
    }
    if data.len() < 4 {
        return FontFormat::Unknown;
    }
    match &data[..4] {
        [0x00, 0x01, 0x00, 0x00] | b"true" | b"typ1" | b"ttcf" => FontFormat::TrueType,
        b"OTTO" => FontFormat::OpenType,
        b"wOFF" => FontFormat::Woff,
        b"wOF2" => FontFormat::Woff2,
        _ => FontFormat::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_json_and_shaderlab() {
        assert_eq!(
            classify_text_payload("config", Some("{\"hp\": 100}"), b"{\"hp\": 100}"),
            TextAssetKind::Json
        );
        assert_eq!(
            classify_text_payload(
                "Diffuse",
                Some("Shader \"Custom/Diffuse\" { SubShader {} }"),
                b"Shader \"Custom/Diffuse\""
            ),
            TextAssetKind::ShaderLab
        );
    }

    #[test]
    fn detects_ttf_and_otf_magic() {
        assert_eq!(
            detect_font_format(&[0x00, 0x01, 0x00, 0x00, 0x00]),
            FontFormat::TrueType
        );
        assert_eq!(detect_font_format(b"OTTO\x00\x0C"), FontFormat::OpenType);
    }
}
