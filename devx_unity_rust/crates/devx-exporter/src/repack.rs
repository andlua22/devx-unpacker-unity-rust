//! `rabex`-backed UnityFS bundle repack and in-place object write-back helpers.

use crate::{ExportError, ExportResult};
use rabex::UnityVersion;
use rabex::files::bundlefile::{BundleFileBuilder, BundleFileReader, ExtractionConfig};
pub use rabex::files::bundlefile::CompressionType;
use rabex::files::serializedfile::{SerializedFile, write_serialized_with_objects};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

/// Entry stored inside a repacked UnityFS bundle.
#[derive(Debug, Clone)]
pub struct BundleFileEntry {
    pub name: String,
    pub bytes: Vec<u8>,
    pub is_serialized_file: bool,
}

/// Packs one or more serialized files / resource streams into a UnityFS v7 bundle in memory.
pub fn build_unityfs_bundle(
    unity_version: &str,
    entries: &[BundleFileEntry],
    compression: CompressionType,
) -> ExportResult<Vec<u8>> {
    let ver: UnityVersion = unity_version
        .trim()
        .parse()
        .or_else(|_| "2022.3.0f1".parse::<UnityVersion>())
        .map_err(|e| ExportError::Pack(format!("invalid Unity version '{unity_version}': {e:?}")))?;

    let mut builder = BundleFileBuilder::unityfs(7, &ver);
    builder.set_header_compression(compression);

    for entry in entries {
        if entry.is_serialized_file {
            builder.add_file(&entry.name, Cursor::new(&entry.bytes))?;
        } else {
            builder.add_file_with_flags(&entry.name, Cursor::new(&entry.bytes), 0)?;
        }
    }

    let mut out = Vec::new();
    builder.write(Cursor::new(&mut out), compression)?;
    Ok(out)
}

/// Writes a repacked UnityFS bundle directly to `out_path`.
pub fn write_unityfs_bundle(
    out_path: &Path,
    unity_version: &str,
    entries: &[BundleFileEntry],
    compression: CompressionType,
) -> ExportResult<()> {
    let bytes = build_unityfs_bundle(unity_version, entries, compression)?;
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out_path, &bytes)?;
    Ok(())
}

/// Replaces a single object's raw serialized payload inside a Unity `SerializedFile` buffer,
/// preserving header metadata, type trees, externals, and all sibling objects.
pub fn replace_object_payload_in_serialized_file(
    sf_bytes: &[u8],
    target_path_id: i64,
    replacement: &[u8],
) -> ExportResult<Vec<u8>> {
    let mut reader = Cursor::new(sf_bytes);
    let sf = SerializedFile::from_reader(&mut reader)
        .map_err(|e| ExportError::Pack(format!("parse SerializedFile failed: {e:?}")))?;

    let mut replaced = false;
    let mut objects = Vec::with_capacity(sf.objects().len());
    for info in sf.objects() {
        if info.m_PathID == target_path_id {
            replaced = true;
            objects.push((info.clone(), Cow::Borrowed(replacement)));
        } else {
            let start = info.m_Offset as usize;
            let end = start.saturating_add(info.m_Size as usize);
            let slice = sf_bytes.get(start..end).ok_or_else(|| {
                ExportError::Pack(format!(
                    "object {} range {start}..{end} out of bounds ({})",
                    info.m_PathID,
                    sf_bytes.len()
                ))
            })?;
            objects.push((info.clone(), Cow::Borrowed(slice)));
        }
    }

    if !replaced {
        return Err(ExportError::AssetNotFound {
            file_index: 0,
            path_id: target_path_id,
        });
    }

    let mut out = Vec::new();
    let empty_common = HashMap::new();
    write_serialized_with_objects(
        Cursor::new(&mut out),
        &sf,
        &empty_common,
        objects.into_iter(),
    )?;
    Ok(out)
}

/// Unpacks a UnityFS `.ab`/`.bundle` byte slice, replaces `target_path_id` in whichever serialized
/// cabinet contains it, preserves all `.resS`/resource streams, and repacks the bundle.
pub fn replace_object_payload_in_bundle(
    bundle_bytes: &[u8],
    target_path_id: i64,
    replacement: &[u8],
    fallback_unity_version: &str,
    compression: CompressionType,
) -> ExportResult<Vec<u8>> {
    let reader = BundleFileReader::from_reader(Cursor::new(bundle_bytes), &ExtractionConfig::default())
        .map_err(|e| ExportError::Pack(format!("parse UnityFS bundle failed: {e}")))?;

    let detected_version = reader
        .header()
        .unity_revision
        .as_ref()
        .map(|v| v.to_string())
        .filter(|s| !s.is_empty() && !s.starts_with("0."))
        .unwrap_or_else(|| fallback_unity_version.to_owned());

    let serialized_names: std::collections::HashSet<String> = reader
        .serialized_files()
        .map(|s| s.path.clone())
        .collect();

    let mut entries = Vec::with_capacity(reader.files().len());
    let mut found = false;

    for file in reader.files() {
        let raw = reader
            .read_at_entry(file)
            .map_err(|e| ExportError::Pack(format!("read bundle entry '{}' failed: {e}", file.path)))?;
        let is_serialized = serialized_names.contains(&file.path);
        let final_bytes = if is_serialized && !found {
            match replace_object_payload_in_serialized_file(&raw, target_path_id, replacement) {
                Ok(updated) => {
                    found = true;
                    updated
                }
                Err(ExportError::AssetNotFound { .. }) => raw,
                Err(other) => return Err(other),
            }
        } else {
            raw
        };
        entries.push(BundleFileEntry {
            name: file.path.clone(),
            bytes: final_bytes,
            is_serialized_file: is_serialized,
        });
    }

    if !found {
        return Err(ExportError::AssetNotFound {
            file_index: 0,
            path_id: target_path_id,
        });
    }

    build_unityfs_bundle(&detected_version, &entries, compression)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_valid_unityfs_bundle_header() {
        let entries = vec![BundleFileEntry {
            name: "CAB-test".into(),
            bytes: b"DUMMY_SERIALIZED".to_vec(),
            is_serialized_file: true,
        }];
        let bundle = build_unityfs_bundle("2022.3.10f1", &entries, CompressionType::None)
            .expect("bundle build");
        assert!(bundle.starts_with(b"UnityFS\0"));
    }
}
