//! Clean-room .NET PE/COFF + ECMA-335 CLI metadata (`BSJB` / `#~` / `#Strings`) parser
//! for Unity `Managed/*.dll` assemblies without third-party dependencies.

use crate::{ScriptError, ScriptResult};
use serde::{Deserialize, Serialize};

/// ECMA-335 summary extracted from a managed `.dll` assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedAssemblyInfo {
    pub file_name: String,
    pub virtual_path: String,
    pub runtime_version: String,
    pub assembly_name: String,
    pub assembly_version: String,
    pub is_pe64: bool,
    pub referenced_assemblies: Vec<String>,
    pub type_defs: Vec<ManagedTypeInfo>,
    pub method_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedTypeInfo {
    pub namespace: String,
    pub name: String,
    pub kind: String,
    pub is_public: bool,
}

#[derive(Debug, Clone, Copy)]
struct PeSection {
    virtual_address: u32,
    virtual_size: u32,
    raw_size: u32,
    raw_offset: u32,
}

/// Parses a .NET PE/DLL binary and extracts its ECMA-335 assembly & `TypeDef` table.
pub fn parse_managed_pe(
    virtual_path: &str,
    data: &[u8],
) -> ScriptResult<ManagedAssemblyInfo> {
    if data.len() < 0x100 || &data[0..2] != b"MZ" {
        return Err(ScriptError::InvalidPe("missing MZ header".to_owned()));
    }
    let pe_off = read_u32(data, 0x3C)? as usize;
    if data.get(pe_off..pe_off + 4) != Some(b"PE\0\0") {
        return Err(ScriptError::InvalidPe("missing PE signature".to_owned()));
    }

    let coff_off = pe_off + 4;
    let num_sections = read_u16(data, coff_off + 2)? as usize;
    let opt_hdr_size = read_u16(data, coff_off + 16)? as usize;
    let opt_off = coff_off + 20;
    let magic = read_u16(data, opt_off)?;
    let (is_pe64, dd_off, num_dd) = match magic {
        0x010B => {
            let num = read_u32(data, opt_off + 92)? as usize;
            (false, opt_off + 96, num)
        }
        0x020B => {
            let num = read_u32(data, opt_off + 108)? as usize;
            (true, opt_off + 112, num)
        }
        other => {
            return Err(ScriptError::InvalidPe(format!(
                "unsupported PE optional header magic 0x{other:04X}"
            )));
        }
    };

    if num_dd <= 14 {
        return Err(ScriptError::NotDotNet);
    }
    let clr_rva = read_u32(data, dd_off + 14 * 8)?;
    let clr_size = read_u32(data, dd_off + 14 * 8 + 4)?;
    if clr_rva == 0 || clr_size < 16 {
        return Err(ScriptError::NotDotNet);
    }

    let sec_table_off = opt_off + opt_hdr_size;
    let mut sections = Vec::with_capacity(num_sections);
    for i in 0..num_sections {
        let s_off = sec_table_off + i * 40;
        if s_off + 40 > data.len() {
            break;
        }
        sections.push(PeSection {
            virtual_size: read_u32(data, s_off + 8)?,
            virtual_address: read_u32(data, s_off + 12)?,
            raw_size: read_u32(data, s_off + 16)?,
            raw_offset: read_u32(data, s_off + 20)?,
        });
    }

    let clr_off = rva_to_offset(&sections, clr_rva)
        .ok_or_else(|| ScriptError::InvalidPe("CLR header RVA unmapped".to_owned()))?;
    let meta_rva = read_u32(data, clr_off + 8)?;
    let meta_size = read_u32(data, clr_off + 12)? as usize;
    let meta_off = rva_to_offset(&sections, meta_rva)
        .ok_or_else(|| ScriptError::InvalidPe("CLI metadata RVA unmapped".to_owned()))?;
    let meta_slice = data
        .get(meta_off..meta_off.saturating_add(meta_size).min(data.len()))
        .ok_or_else(|| ScriptError::InvalidPe("CLI metadata truncated".to_owned()))?;

    let mut info = parse_cli_metadata_root(virtual_path, meta_slice)?;
    info.is_pe64 = is_pe64;
    Ok(info)
}

/// Parses an ECMA-335 `BSJB` metadata root directly (also testable standalone).
pub fn parse_cli_metadata_root(
    virtual_path: &str,
    meta: &[u8],
) -> ScriptResult<ManagedAssemblyInfo> {
    if read_u32(meta, 0)? != 0x424A_5342 {
        return Err(ScriptError::InvalidPe(
            "missing ECMA-335 BSJB metadata signature".to_owned(),
        ));
    }

    let ver_len = read_u32(meta, 12)? as usize;
    let ver_bytes = meta
        .get(16..16 + ver_len)
        .ok_or_else(|| ScriptError::InvalidPe("CLI version truncated".to_owned()))?;
    let runtime_version = String::from_utf8_lossy(ver_bytes)
        .trim_end_matches('\0')
        .to_owned();

    // Padded to 4 bytes; next is flags (2B) + streams count (2B)
    let streams_hdr_off = ((16 + ver_len + 3) & !3) + 2;
    let stream_count = read_u16(meta, streams_hdr_off)? as usize;
    let mut cursor = streams_hdr_off + 2;

    let mut strings_heap: &[u8] = &[];
    let mut tables_stream: &[u8] = &[];

    for _ in 0..stream_count {
        let off = read_u32(meta, cursor)? as usize;
        let size = read_u32(meta, cursor + 4)? as usize;
        cursor += 8;

        let name_start = cursor;
        while cursor < meta.len() && meta[cursor] != 0 {
            cursor += 1;
        }
        let name = String::from_utf8_lossy(&meta[name_start..cursor]).into_owned();
        cursor = (cursor + 1 + 3) & !3;

        let end = off.saturating_add(size).min(meta.len());
        if off < end {
            let slice = &meta[off..end];
            match name.as_str() {
                "#Strings" => strings_heap = slice,
                "#~" | "#-" => tables_stream = slice,
                _ => {}
            }
        }
    }

    let file_name = virtual_path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(virtual_path)
        .to_owned();

    let mut out = ManagedAssemblyInfo {
        file_name: file_name.clone(),
        virtual_path: virtual_path.to_owned(),
        runtime_version,
        assembly_name: file_name,
        assembly_version: String::new(),
        is_pe64: false,
        referenced_assemblies: Vec::new(),
        type_defs: Vec::new(),
        method_count: 0,
    };

    if !tables_stream.is_empty() {
        parse_tilde_tables(tables_stream, strings_heap, &mut out)?;
    }

    Ok(out)
}

fn parse_tilde_tables(
    ts: &[u8],
    strings_heap: &[u8],
    out: &mut ManagedAssemblyInfo,
) -> ScriptResult<()> {
    if ts.len() < 24 {
        return Err(ScriptError::InvalidPe("#~ header too short".to_owned()));
    }
    let heap_sizes = ts[6];
    let str_wide = (heap_sizes & 0x01) != 0;
    let guid_wide = (heap_sizes & 0x02) != 0;
    let blob_wide = (heap_sizes & 0x04) != 0;

    let str_idx_sz = if str_wide { 4usize } else { 2usize };
    let guid_idx_sz = if guid_wide { 4usize } else { 2usize };
    let blob_idx_sz = if blob_wide { 4usize } else { 2usize };

    let valid_mask = read_u64(ts, 8)?;
    let mut row_counts = [0u32; 64];
    let mut pos = 24usize;
    for (bit, slot) in row_counts.iter_mut().enumerate() {
        if ((valid_mask >> bit) & 1) != 0 {
            if pos + 4 > ts.len() {
                break;
            }
            *slot = read_u32(ts, pos)?;
            pos += 4;
        }
    }

    out.method_count = row_counts[0x06];

    let coded_sz = |bits_tag: u32, tables: &[usize]| -> usize {
        let max_rows = tables
            .iter()
            .filter_map(|&t| row_counts.get(t).copied())
            .max()
            .unwrap_or(0);
        let small_limit = 1u32 << (16 - bits_tag);
        if max_rows < small_limit { 2 } else { 4 }
    };
    let table_sz = |t: usize| -> usize {
        if row_counts.get(t).copied().unwrap_or(0) <= 0xFFFF {
            2
        } else {
            4
        }
    };

    let resolution_scope_sz = coded_sz(2, &[0x00, 0x1A, 0x23, 0x01]);
    let type_def_or_ref_sz = coded_sz(2, &[0x02, 0x01, 0x1B]);
    let has_constant_sz = coded_sz(2, &[0x04, 0x08, 0x17]);
    let has_custom_attr_sz = coded_sz(
        5,
        &[
            0x06, 0x04, 0x01, 0x02, 0x08, 0x09, 0x0A, 0x00, 0x0E, 0x17, 0x14, 0x11, 0x1A, 0x1B,
            0x20, 0x23, 0x26, 0x27, 0x28, 0x2A, 0x2C, 0x2B,
        ],
    );
    let has_field_marshal_sz = coded_sz(1, &[0x04, 0x08]);
    let has_decl_security_sz = coded_sz(2, &[0x02, 0x06, 0x20]);
    let member_ref_parent_sz = coded_sz(3, &[0x02, 0x01, 0x1A, 0x06, 0x1B]);
    let has_semantics_sz = coded_sz(1, &[0x14, 0x17]);
    let method_def_or_ref_sz = coded_sz(1, &[0x06, 0x0A]);
    let member_forwarded_sz = coded_sz(1, &[0x04, 0x06]);
    let implementation_sz = coded_sz(2, &[0x26, 0x23, 0x27]);
    let custom_attr_type_sz = coded_sz(3, &[0x06, 0x0A]);

    let table_row_size = |t: usize| -> usize {
        match t {
            0x00 => 2 + str_idx_sz + guid_idx_sz * 3, // Module
            0x01 => resolution_scope_sz + str_idx_sz * 2, // TypeRef
            0x02 => 4 + str_idx_sz * 2 + type_def_or_ref_sz + table_sz(0x04) + table_sz(0x06), // TypeDef
            0x03 => table_sz(0x04), // FieldPtr
            0x04 => 2 + str_idx_sz + blob_idx_sz, // Field
            0x05 => table_sz(0x06), // MethodPtr
            0x06 => 4 + 2 + 2 + str_idx_sz + blob_idx_sz + table_sz(0x08), // MethodDef
            0x07 => table_sz(0x08), // ParamPtr
            0x08 => 2 + 2 + str_idx_sz, // Param
            0x09 => table_sz(0x02) + type_def_or_ref_sz, // InterfaceImpl
            0x0A => member_ref_parent_sz + str_idx_sz + blob_idx_sz, // MemberRef
            0x0B => 2 + has_constant_sz + blob_idx_sz, // Constant
            0x0C => has_custom_attr_sz + custom_attr_type_sz + blob_idx_sz, // CustomAttribute
            0x0D => has_field_marshal_sz + blob_idx_sz, // FieldMarshal
            0x0E => 2 + has_decl_security_sz + blob_idx_sz, // DeclSecurity
            0x0F => 2 + 4 + table_sz(0x02), // ClassLayout
            0x10 => 4 + table_sz(0x04), // FieldLayout
            0x11 => blob_idx_sz, // StandAloneSig
            0x12 => table_sz(0x02) + table_sz(0x14), // EventMap
            0x13 => table_sz(0x14), // EventPtr
            0x14 => 2 + str_idx_sz + type_def_or_ref_sz, // Event
            0x15 => table_sz(0x02) + table_sz(0x17), // PropertyMap
            0x16 => table_sz(0x17), // PropertyPtr
            0x17 => 2 + str_idx_sz + blob_idx_sz, // Property
            0x18 => 2 + table_sz(0x06) + has_semantics_sz, // MethodSemantics
            0x19 => table_sz(0x02) + method_def_or_ref_sz + method_def_or_ref_sz, // MethodImpl
            0x1A => str_idx_sz, // ModuleRef
            0x1B => blob_idx_sz, // TypeSpec
            0x1C => 2 + member_forwarded_sz + str_idx_sz + table_sz(0x1A), // ImplMap
            0x1D => 4 + table_sz(0x04), // FieldRVA
            0x1E | 0x1F => 4, // ENCLog / ENCMap
            0x20 => 4 + 2 * 4 + 4 + blob_idx_sz + str_idx_sz * 2, // Assembly
            0x21 => 4, // AssemblyProcessor
            0x22 => 4 * 3, // AssemblyOS
            0x23 => 2 * 4 + 4 + blob_idx_sz + str_idx_sz * 2 + blob_idx_sz, // AssemblyRef
            0x24 => 4 + table_sz(0x23),
            0x25 => 4 * 3 + table_sz(0x23),
            0x26 => 4 + str_idx_sz + blob_idx_sz, // File
            0x27 => 4 + 4 + str_idx_sz * 2 + implementation_sz, // ExportedType
            0x28 => 4 + 4 + str_idx_sz + implementation_sz, // ManifestResource
            0x29 => table_sz(0x02) * 2, // NestedClass
            _ => 0,
        }
    };

    for (t, &row_count) in row_counts.iter().enumerate().take(0x2A) {
        let rows = row_count as usize;
        if rows == 0 {
            continue;
        }
        let rsz = table_row_size(t);
        if rsz == 0 || pos + rows * rsz > ts.len() {
            break;
        }
        let tbl = &ts[pos..pos + rows * rsz];

        match t {
            0x02 => {
                // TypeDef: Flags(4) | TypeName(str) | TypeNamespace(str) | ...
                for r in 0..rows {
                    let row = &tbl[r * rsz..];
                    let flags = read_u32(row, 0)?;
                    let name_idx = read_heap_idx(row, 4, str_wide)?;
                    let ns_idx = read_heap_idx(row, 4 + str_idx_sz, str_wide)?;
                    let name = read_c_str(strings_heap, name_idx);
                    if name == "<Module>" || name.is_empty() {
                        continue;
                    }
                    let ns = read_c_str(strings_heap, ns_idx);
                    let visibility = flags & 0x07;
                    let is_public = matches!(visibility, 1 | 2);
                    let kind = if (flags & 0x20) != 0 {
                        "interface"
                    } else if (flags & 0x0180) == 0x0180 {
                        "static class"
                    } else {
                        "class"
                    };
                    out.type_defs.push(ManagedTypeInfo {
                        namespace: ns,
                        name,
                        kind: kind.to_owned(),
                        is_public,
                    });
                }
            }
            0x20 => {
                // Assembly: HashAlgId(4) + Major(2) Minor(2) Build(2) Rev(2) + Flags(4) + PublicKey(blob) + Name(str)
                let row = &tbl[..rsz];
                let maj = read_u16(row, 4)?;
                let min = read_u16(row, 6)?;
                let bld = read_u16(row, 8)?;
                let rev = read_u16(row, 10)?;
                let name_idx = read_heap_idx(row, 16 + blob_idx_sz, str_wide)?;
                let name = read_c_str(strings_heap, name_idx);
                if !name.is_empty() {
                    out.assembly_name = name;
                }
                out.assembly_version = format!("{maj}.{min}.{bld}.{rev}");
            }
            0x23 => {
                // AssemblyRef: Major..Rev(8) + Flags(4) + PubKey(blob) + Name(str)
                for r in 0..rows {
                    let row = &tbl[r * rsz..];
                    let maj = read_u16(row, 0)?;
                    let min = read_u16(row, 2)?;
                    let name_idx = read_heap_idx(row, 12 + blob_idx_sz, str_wide)?;
                    let name = read_c_str(strings_heap, name_idx);
                    if !name.is_empty() {
                        out.referenced_assemblies.push(format!("{name} ({maj}.{min})"));
                    }
                }
            }
            _ => {}
        }

        pos += rows * rsz;
    }

    Ok(())
}

fn rva_to_offset(sections: &[PeSection], rva: u32) -> Option<usize> {
    for s in sections {
        let span = s.virtual_size.max(s.raw_size);
        if rva >= s.virtual_address && rva < s.virtual_address.saturating_add(span) {
            return Some((rva - s.virtual_address + s.raw_offset) as usize);
        }
    }
    None
}

fn read_u16(buf: &[u8], off: usize) -> ScriptResult<u16> {
    let b: [u8; 2] = buf
        .get(off..off + 2)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| ScriptError::InvalidPe(format!("truncated u16 at 0x{off:X}")))?;
    Ok(u16::from_le_bytes(b))
}

fn read_u32(buf: &[u8], off: usize) -> ScriptResult<u32> {
    let b: [u8; 4] = buf
        .get(off..off + 4)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| ScriptError::InvalidPe(format!("truncated u32 at 0x{off:X}")))?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64(buf: &[u8], off: usize) -> ScriptResult<u64> {
    let b: [u8; 8] = buf
        .get(off..off + 8)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| ScriptError::InvalidPe(format!("truncated u64 at 0x{off:X}")))?;
    Ok(u64::from_le_bytes(b))
}

fn read_heap_idx(buf: &[u8], off: usize, wide: bool) -> ScriptResult<usize> {
    if wide {
        Ok(read_u32(buf, off)? as usize)
    } else {
        Ok(read_u16(buf, off)? as usize)
    }
}

fn read_c_str(heap: &[u8], offset: usize) -> String {
    if offset >= heap.len() {
        return String::new();
    }
    let slice = &heap[offset..];
    let len = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    String::from_utf8_lossy(&slice[..len]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synthetic_cli_metadata_root() {
        let mut strings = Vec::new();
        strings.push(0); // 0 -> empty
        let off_asm = strings.len() as u16;
        strings.extend_from_slice(b"GameCore\0");
        let off_ns = strings.len() as u16;
        strings.extend_from_slice(b"Game.Player\0");
        let off_cls = strings.len() as u16;
        strings.extend_from_slice(b"PlayerController\0");
        while strings.len() % 4 != 0 {
            strings.push(0);
        }

        // #~ stream with TypeDef (0x02, 1 row) and Assembly (0x20, 1 row)
        let valid_mask: u64 = (1u64 << 0x02) | (1u64 << 0x20);
        let mut tilde = Vec::new();
        tilde.extend_from_slice(&[0u8; 4]); // Reserved
        tilde.push(2); // MajorVersion
        tilde.push(0); // MinorVersion
        tilde.push(0); // HeapSizes (small 2-byte indices)
        tilde.push(1); // Reserved
        tilde.extend_from_slice(&valid_mask.to_le_bytes());
        tilde.extend_from_slice(&0u64.to_le_bytes()); // Sorted
        tilde.extend_from_slice(&1u32.to_le_bytes()); // TypeDef rows
        tilde.extend_from_slice(&1u32.to_le_bytes()); // Assembly rows

        // TypeDef row: Flags(4) + Name(2) + Namespace(2) + Extends(2) + FieldList(2) + MethodList(2) = 14B
        tilde.extend_from_slice(&1u32.to_le_bytes()); // Public
        tilde.extend_from_slice(&off_cls.to_le_bytes());
        tilde.extend_from_slice(&off_ns.to_le_bytes());
        tilde.extend_from_slice(&0u16.to_le_bytes());
        tilde.extend_from_slice(&1u16.to_le_bytes());
        tilde.extend_from_slice(&1u16.to_le_bytes());

        // Assembly row: HashAlg(4) + Ver(8) + Flags(4) + PubKey(2) + Name(2) + Culture(2) = 22B
        tilde.extend_from_slice(&0x8004u32.to_le_bytes());
        tilde.extend_from_slice(&1u16.to_le_bytes());
        tilde.extend_from_slice(&2u16.to_le_bytes());
        tilde.extend_from_slice(&3u16.to_le_bytes());
        tilde.extend_from_slice(&4u16.to_le_bytes());
        tilde.extend_from_slice(&0u32.to_le_bytes());
        tilde.extend_from_slice(&0u16.to_le_bytes());
        tilde.extend_from_slice(&off_asm.to_le_bytes());
        tilde.extend_from_slice(&0u16.to_le_bytes());

        let mut meta = Vec::new();
        meta.extend_from_slice(&0x424A_5342u32.to_le_bytes());
        meta.extend_from_slice(&1u16.to_le_bytes());
        meta.extend_from_slice(&1u16.to_le_bytes());
        meta.extend_from_slice(&0u32.to_le_bytes());
        let ver = b"v4.0.30319\0\0";
        meta.extend_from_slice(&(ver.len() as u32).to_le_bytes());
        meta.extend_from_slice(ver);
        meta.extend_from_slice(&0u16.to_le_bytes()); // Flags
        meta.extend_from_slice(&2u16.to_le_bytes()); // 2 streams

        // Stream headers placeholder (two headers of 8 + 12 = 20 and 8 + 4 = 12)
        let hdr_pos = meta.len();
        let str_hdr_size = 8 + b"#Strings\0\0\0\0".len();
        let tilde_hdr_size = 8 + b"#~\0\0".len();
        let str_off = (hdr_pos + str_hdr_size + tilde_hdr_size) as u32;
        let tilde_off = str_off + strings.len() as u32;

        meta.extend_from_slice(&str_off.to_le_bytes());
        meta.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        meta.extend_from_slice(b"#Strings\0\0\0\0");

        meta.extend_from_slice(&tilde_off.to_le_bytes());
        meta.extend_from_slice(&(tilde.len() as u32).to_le_bytes());
        meta.extend_from_slice(b"#~\0\0");

        meta.extend_from_slice(&strings);
        meta.extend_from_slice(&tilde);

        let info = parse_cli_metadata_root("Managed/GameCore.dll", &meta).expect("metadata ok");
        assert_eq!(info.assembly_name, "GameCore");
        assert_eq!(info.assembly_version, "1.2.3.4");
        assert_eq!(info.type_defs.len(), 1);
        assert_eq!(info.type_defs[0].namespace, "Game.Player");
        assert_eq!(info.type_defs[0].name, "PlayerController");
    }
}
