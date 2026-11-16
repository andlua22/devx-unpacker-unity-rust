//! `.unitypackage` (`tar.gz`) exporter producing standard `<guid>/{pathname,asset,asset.meta}`
//! archives importable by Unity Editor.

use crate::{ExportError, ExportResult};
use flate2::Compression;
use flate2::write::GzEncoder;
use std::io::Write;
use std::path::Path;
use tar::{Builder, Header};

/// Single entry to pack into a `.unitypackage`.
#[derive(Debug, Clone)]
pub struct UnityPackageInputEntry {
    /// Unity project-relative path (e.g. `"Assets/Textures/hero.png"`).
    pub asset_path: String,
    /// Optional 32-hex GUID; generated deterministically from `asset_path` if `None`.
    pub guid: Option<String>,
    /// Raw file bytes stored at `<guid>/asset`.
    pub asset_bytes: Vec<u8>,
    /// Optional `.meta` file text; minimal Unity YAML `.meta` generated if `None`.
    pub meta_text: Option<String>,
}

/// Generates a deterministic 32-lowercase-hex Unity GUID from a seed string (FNV-1a 128-bit).
pub fn deterministic_guid(seed: &str) -> String {
    let mut h: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    for &b in seed.as_bytes() {
        h ^= b as u128;
        h = h.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
    }
    format!("{h:032x}")
}

/// Builds a compressed `.unitypackage` (`tar.gz`) in memory.
pub fn build_unitypackage_bytes(entries: &[UnityPackageInputEntry]) -> ExportResult<Vec<u8>> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = Builder::new(&mut gz);
        for entry in entries {
            let clean_path = entry
                .asset_path
                .trim_start_matches('/')
                .replace('\\', "/");
            let guid = entry
                .guid
                .as_deref()
                .filter(|g| g.len() == 32 && g.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(str::to_ascii_lowercase)
                .unwrap_or_else(|| deterministic_guid(&clean_path));

            append_tar_file(&mut tar, &format!("{guid}/pathname"), clean_path.as_bytes())?;
            append_tar_file(&mut tar, &format!("{guid}/asset"), &entry.asset_bytes)?;

            let default_meta = format!("fileFormatVersion: 2\nguid: {guid}\n");
            let meta = entry.meta_text.as_deref().unwrap_or(&default_meta);
            append_tar_file(&mut tar, &format!("{guid}/asset.meta"), meta.as_bytes())?;
        }
        tar.finish()?;
    }
    gz.finish().map_err(ExportError::Io)
}

/// Writes a compressed `.unitypackage` directly to `out_path`.
pub fn write_unitypackage(
    out_path: &Path,
    entries: &[UnityPackageInputEntry],
) -> ExportResult<()> {
    let bytes = build_unitypackage_bytes(entries)?;
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(out_path)?;
    file.write_all(&bytes)?;
    Ok(())
}

fn append_tar_file<W: Write>(
    tar: &mut Builder<W>,
    archive_path: &str,
    data: &[u8],
) -> ExportResult<()> {
    let mut hdr = Header::new_gnu();
    hdr.set_size(data.len() as u64);
    hdr.set_mode(0o644);
    hdr.set_cksum();
    tar.append_data(&mut hdr, archive_path, data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use tar::Archive;

    #[test]
    fn builds_valid_tar_gz_unitypackage() {
        let entries = vec![UnityPackageInputEntry {
            asset_path: "Assets/Texts/hello.txt".into(),
            guid: None,
            asset_bytes: b"hello unity".to_vec(),
            meta_text: None,
        }];
        let pkg = build_unitypackage_bytes(&entries).expect("pack ok");
        let gz = GzDecoder::new(&pkg[..]);
        let mut ar = Archive::new(gz);
        let paths: Vec<String> = ar
            .entries()
            .expect("entries ok")
            .filter_map(|e| e.ok())
            .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(paths.len(), 3);
        assert!(paths.iter().any(|p| p.ends_with("/pathname")));
        assert!(paths.iter().any(|p| p.ends_with("/asset")));
        assert!(paths.iter().any(|p| p.ends_with("/asset.meta")));
    }
}
