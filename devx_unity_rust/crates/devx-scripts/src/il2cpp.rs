//! Clean-room parser for Unity IL2CPP `global-metadata.dat` header, string literal table,
//! and symbol strings.

use crate::{ScriptError, ScriptResult};
use il2cpp_dumper::config::{Config as DumperConfig, MethodBodyMode};
use il2cpp_dumper::il2cpp::enums::{field_attributes, type_attributes};
use il2cpp_dumper::session::{Il2CppDumpWriteOptions, Il2CppSessionInfo, LoadedIl2CppSession};
pub use il2cpp_dumper::session::{
    DisassembledMethodReport, Il2CppDisasmOptions, Il2CppMethodEntry,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

const IL2CPP_METADATA_MAGIC: u32 = 0xFAB1_1BAF;

/// Recovered IL2CPP assembly metadata (`global-metadata.dat` image table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Il2CppRecoveredImage {
    pub image_index: usize,
    pub assembly_name: String,
    pub type_start: usize,
    pub type_count: usize,
}

/// Recovered IL2CPP field declaration with resolved C# type and struct offset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Il2CppRecoveredField {
    pub name: String,
    pub type_name: String,
    pub is_public: bool,
    pub is_static: bool,
    pub is_const: bool,
    pub offset: i32,
}

/// Recovered IL2CPP type (`TypeDef`) with fields, inheritance, and method count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Il2CppRecoveredType {
    pub type_def_index: usize,
    pub image_index: usize,
    pub assembly_name: String,
    pub namespace: String,
    pub name: String,
    pub full_name: String,
    pub kind: String,
    pub is_public: bool,
    pub parent: String,
    pub fields: Vec<Il2CppRecoveredField>,
    pub method_count: usize,
}

/// Thread-safe handle around an in-memory [`LoadedIl2CppSession`] for on-demand dumps & exports.
pub struct Il2CppSessionHolder(pub Mutex<LoadedIl2CppSession>);

impl std::fmt::Debug for Il2CppSessionHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Il2CppSessionHolder").finish()
    }
}

/// Complete recovered IL2CPP index pairing `libil2cpp.so` / `GameAssembly.dll` + `global-metadata.dat`.
#[derive(Debug, Clone)]
pub struct Il2CppDumpReport {
    pub binary_path: String,
    pub metadata_path: String,
    pub binary_format: String,
    pub arch: String,
    pub arch_has_native_code: bool,
    pub metadata_version: f64,
    pub unity_version: Option<String>,
    pub metadata_decryption: Option<String>,
    pub string_literal_count: usize,
    pub total_method_count: usize,
    pub images: Vec<Il2CppRecoveredImage>,
    pub types: Vec<Il2CppRecoveredType>,
    pub type_lookup: HashMap<String, usize>,
    pub session: Arc<Il2CppSessionHolder>,
}

/// Write mode options exposed by `devx-scripts` for IL2CPP directory exports.
#[derive(Debug, Clone)]
pub struct Il2CppExportConfig {
    pub write_dump_cs: bool,
    pub write_diffable_cs: bool,
    pub write_script_json_and_headers: bool,
    pub write_dummy_dlls: bool,
    pub write_generics_dump: bool,
    pub include_pseudo_c: bool,
    pub fold_ifix_wrappers: bool,
    pub max_method_bytes: Option<usize>,
    pub include_disassembly: bool,
    pub disasm: Il2CppDisasmOptions,
    pub include_assemblies: Vec<String>,
    pub include_types: Vec<String>,
    pub filter_headers: bool,
    pub full_dump: bool,
}

impl Default for Il2CppExportConfig {
    fn default() -> Self {
        Self {
            write_dump_cs: true,
            write_diffable_cs: true,
            write_script_json_and_headers: true,
            write_dummy_dlls: false,
            write_generics_dump: false,
            include_pseudo_c: false,
            fold_ifix_wrappers: false,
            max_method_bytes: None,
            include_disassembly: false,
            disasm: Il2CppDisasmOptions::default(),
            include_assemblies: Vec::new(),
            include_types: Vec::new(),
            filter_headers: true,
            full_dump: false,
        }
    }
}

impl Il2CppDumpReport {
    /// Recovers IL2CPP type tables, fields, offsets, and method RVAs directly from binary + metadata bytes.
    pub fn recover_from_bytes(
        binary_path: &str,
        binary_bytes: Vec<u8>,
        metadata_path: &str,
        metadata_bytes: Vec<u8>,
    ) -> ScriptResult<Self> {
        let mut session = LoadedIl2CppSession::open_from_bytes(
            binary_bytes,
            metadata_bytes,
            None,
            DumperConfig::default(),
        )
        .map_err(|e| ScriptError::InvalidIl2Cpp(e.to_string()))?;

        let arch_enum = session.arch();
        let arch = arch_enum.to_string();
        let arch_has_native_code = arch_enum.has_native_code();
        let info: Il2CppSessionInfo = session.info.clone();
        let mut images = Vec::with_capacity(session.metadata.image_defs.len());
        let mut types = Vec::with_capacity(session.metadata.type_defs.len());
        let mut type_lookup = HashMap::with_capacity(session.metadata.type_defs.len() * 2);

        for img_idx in 0..session.metadata.image_defs.len() {
            let img = session.metadata.image_defs[img_idx].clone();
            let assembly_name = session
                .metadata
                .get_string_from_index(img.name_index)
                .unwrap_or_else(|_| format!("Assembly-{img_idx}.dll"));
            let type_start = img.type_start.max(0) as usize;
            let type_count = img.type_count.max(0) as usize;
            let type_end = type_start
                .saturating_add(type_count)
                .min(session.metadata.type_defs.len());

            images.push(Il2CppRecoveredImage {
                image_index: img_idx,
                assembly_name: assembly_name.clone(),
                type_start,
                type_count,
            });

            for td_idx in type_start..type_end {
                let td = session.metadata.type_defs[td_idx].clone();
                let namespace = session
                    .metadata
                    .get_string_from_index(td.namespace_index)
                    .unwrap_or_default();
                let name = session
                    .metadata
                    .get_string_from_index(td.name_index)
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let full_name = if namespace.is_empty() {
                    name.clone()
                } else {
                    format!("{namespace}.{name}")
                };

                let kind = if (td.flags & type_attributes::INTERFACE) != 0 {
                    "interface"
                } else if td.is_enum() {
                    "enum"
                } else if td.is_value_type() {
                    "struct"
                } else {
                    "class"
                }
                .to_owned();

                let vis = td.flags & type_attributes::VISIBILITY_MASK;
                let is_public = matches!(
                    vis,
                    type_attributes::PUBLIC | type_attributes::NESTED_PUBLIC
                );

                let parent = if td.parent_index >= 0 {
                    session
                        .il2cpp
                        .types
                        .get(td.parent_index as usize)
                        .cloned()
                        .map(|pt| {
                            session.executor.get_type_name(
                                &pt,
                                &session.metadata,
                                &session.il2cpp,
                                false,
                                false,
                            )
                        })
                        .unwrap_or_default()
                } else {
                    String::new()
                };

                let field_start = td.field_start.max(0) as usize;
                let field_end = field_start
                    .saturating_add(td.field_count as usize)
                    .min(session.metadata.field_defs.len());
                let mut fields = Vec::with_capacity(field_end.saturating_sub(field_start));

                for fi in field_start..field_end {
                    let fd = session.metadata.field_defs[fi].clone();
                    let Some(ft) = session.il2cpp.types.get(fd.type_index as usize).cloned() else {
                        continue;
                    };
                    let f_name = session
                        .metadata
                        .get_string_from_index(fd.name_index)
                        .unwrap_or_default();
                    let f_type_name = session.executor.get_type_name(
                        &ft,
                        &session.metadata,
                        &session.il2cpp,
                        false,
                        false,
                    );
                    let f_vis = ft.attrs & field_attributes::FIELD_ACCESS_MASK;
                    let f_is_public = f_vis == field_attributes::PUBLIC;
                    let f_is_const = (ft.attrs & field_attributes::LITERAL) != 0;
                    let f_is_static = !f_is_const && (ft.attrs & field_attributes::STATIC) != 0;
                    let offset = session
                        .il2cpp
                        .get_field_offset_from_index(td_idx, fi - field_start, fi, td.is_value_type(), f_is_static);

                    fields.push(Il2CppRecoveredField {
                        name: f_name,
                        type_name: f_type_name,
                        is_public: f_is_public,
                        is_static: f_is_static,
                        is_const: f_is_const,
                        offset,
                    });
                }

                let idx = types.len();
                type_lookup.entry(full_name.clone()).or_insert(idx);
                if namespace.is_empty() {
                    type_lookup.entry(name.clone()).or_insert(idx);
                }
                let asm_key = format!("{}::{full_name}", assembly_name.to_ascii_lowercase());
                type_lookup.entry(asm_key).or_insert(idx);

                types.push(Il2CppRecoveredType {
                    type_def_index: td_idx,
                    image_index: img_idx,
                    assembly_name: assembly_name.clone(),
                    namespace,
                    name,
                    full_name,
                    kind,
                    is_public,
                    parent,
                    fields,
                    method_count: td.method_count as usize,
                });
            }
        }

        Ok(Self {
            binary_path: binary_path.to_owned(),
            metadata_path: metadata_path.to_owned(),
            binary_format: info.binary_format,
            arch,
            arch_has_native_code,
            metadata_version: info.metadata_version,
            unity_version: info.unity_version,
            metadata_decryption: info.metadata_decryption,
            string_literal_count: info.string_literal_count,
            total_method_count: info.method_def_count,
            images,
            types,
            type_lookup,
            session: Arc::new(Il2CppSessionHolder(Mutex::new(session))),
        })
    }

    /// Resolves a recovered class by `(assembly_name, namespace, class_name)`.
    pub fn find_type(
        &self,
        assembly_name: &str,
        namespace: &str,
        class_name: &str,
    ) -> Option<&Il2CppRecoveredType> {
        let full = if namespace.is_empty() {
            class_name.to_owned()
        } else {
            format!("{namespace}.{class_name}")
        };
        if !assembly_name.is_empty() {
            let asm_norm = if assembly_name.ends_with(".dll") {
                assembly_name.to_ascii_lowercase()
            } else {
                format!("{}.dll", assembly_name.to_ascii_lowercase())
            };
            if let Some(&i) = self.type_lookup.get(&format!("{asm_norm}::{full}")) {
                return self.types.get(i);
            }
        }
        self.type_lookup
            .get(&full)
            .or_else(|| self.type_lookup.get(class_name))
            .and_then(|&i| self.types.get(i))
    }

    fn lock_session(&self) -> ScriptResult<std::sync::MutexGuard<'_, LoadedIl2CppSession>> {
        self.session
            .0
            .lock()
            .map_err(|_| ScriptError::InvalidIl2Cpp("IL2CPP session lock poisoned".to_owned()))
    }

    /// Overrides the Pseudo-C decompiler method-size ceiling (`Some(0)` removes the cap).
    pub fn set_max_method_bytes(&self, max_bytes: Option<usize>) -> ScriptResult<()> {
        let mut guard = self.lock_session()?;
        guard.set_pseudo_c_max_body_size(max_bytes);
        Ok(())
    }

    /// Enables or disables experimental IFIX wrapper folding for subsequent type formats.
    pub fn set_fold_ifix_wrappers(&self, fold: bool) -> ScriptResult<()> {
        let mut guard = self.lock_session()?;
        guard.set_fold_ifix_wrappers(fold);
        Ok(())
    }

    /// Finds `(type_def_index, method_def_index)` owning `rva`, if `rva` is a managed method.
    pub fn find_method_owner_by_rva(&self, rva: u64) -> ScriptResult<Option<(usize, usize)>> {
        let guard = self.lock_session()?;
        Ok(guard.find_method_owner_by_rva(rva))
    }

    /// Formats the full IL2CPP dump declaration (fields/offsets, properties, methods/RVAs) on demand.
    pub fn format_type_csharp(
        &self,
        type_def_index: usize,
        include_pseudo_c: bool,
    ) -> ScriptResult<String> {
        self.format_type_source(
            type_def_index,
            false,
            include_pseudo_c,
            &Il2CppDisasmOptions::default(),
        )
    }

    /// Formats a `TypeDef` with optional native disassembly, Pseudo-C, and disassembler flags.
    pub fn format_type_source(
        &self,
        type_def_index: usize,
        include_disassembly: bool,
        include_pseudo_c: bool,
        disasm_opts: &Il2CppDisasmOptions,
    ) -> ScriptResult<String> {
        let mode = MethodBodyMode::from_flags(include_disassembly, include_pseudo_c);
        let mut guard = self.lock_session()?;
        guard
            .format_type_with_mode(type_def_index, mode, disasm_opts)
            .map_err(|e| ScriptError::InvalidIl2Cpp(e.to_string()))
    }

    /// Formats multiple `TypeDef` indices with Pseudo-C method bodies in a single recovery pass.
    pub fn format_types_pseudo_c(&self, type_def_indices: &[usize]) -> ScriptResult<Vec<(usize, String)>> {
        let mut guard = self.lock_session()?;
        guard
            .format_types_pseudo_c(type_def_indices)
            .map_err(|e| ScriptError::InvalidIl2Cpp(e.to_string()))
    }

    /// Lists all methods declared on `type_def_index` along with RVAs, VAs, offsets, and signatures.
    pub fn list_type_methods(&self, type_def_index: usize) -> ScriptResult<Vec<Il2CppMethodEntry>> {
        let mut guard = self.lock_session()?;
        guard
            .list_type_methods(type_def_index)
            .map_err(|e| ScriptError::InvalidIl2Cpp(e.to_string()))
    }

    /// Disassembles a single method or arbitrary RVA using cached IL2CPP annotations & CFG.
    pub fn disassemble_rva(
        &self,
        rva: u64,
        type_def_hint: Option<usize>,
        method_def_hint: Option<usize>,
        options: &Il2CppDisasmOptions,
    ) -> ScriptResult<DisassembledMethodReport> {
        let mut guard = self.lock_session()?;
        guard
            .disassemble_rva(rva, type_def_hint, method_def_hint, options)
            .map_err(|e| ScriptError::InvalidIl2Cpp(e.to_string()))
    }

    /// Writes `dump.cs`, `DiffableCs/`, `script.json`, `il2cpp.h`, `stringliteral.json`, and optional `DummyDll/`.
    pub fn write_full_dump(
        &self,
        out_dir: &Path,
        cfg: &Il2CppExportConfig,
        log: impl FnMut(&str),
    ) -> ScriptResult<Vec<String>> {
        let body_mode = MethodBodyMode::from_flags(cfg.include_disassembly, cfg.include_pseudo_c);
        let opts = Il2CppDumpWriteOptions {
            write_dump_cs: cfg.write_dump_cs,
            write_diffable_cs: cfg.write_diffable_cs,
            write_script_json_and_headers: cfg.write_script_json_and_headers,
            write_dummy_dlls: cfg.write_dummy_dlls,
            write_generics_dump: cfg.write_generics_dump,
            write_static_metadata: false,
            body_mode,
            fold_ifix_wrappers: cfg.fold_ifix_wrappers,
            max_method_bytes: cfg.max_method_bytes,
            disasm: cfg.disasm,
            include_assemblies: cfg.include_assemblies.clone(),
            include_types: cfg.include_types.clone(),
            filter_headers: cfg.filter_headers,
            full_dump: cfg.full_dump,
        };
        let mut guard = self.lock_session()?;
        guard
            .write_dump_to_dir(&self.binary_path, out_dir, &opts, log)
            .map_err(|e| ScriptError::InvalidIl2Cpp(e.to_string()))
    }

    /// Generates a clean compilable Unity C# script stub with `[SerializeField]` attributes.
    pub fn generate_unity_csharp_stub(
        &self,
        assembly_name: &str,
        namespace: &str,
        class_name: &str,
    ) -> Option<String> {
        let td = self.find_type(assembly_name, namespace, class_name)?;
        let clean_name = sanitize_cs_ident(&td.name);
        let base_type = if td.parent.is_empty() || td.parent == "object" {
            if td.kind == "class" {
                " : MonoBehaviour"
            } else {
                ""
            }
        } else {
            let short_parent = td.parent.rsplit('.').next().unwrap_or(&td.parent);
            if short_parent == "MonoBehaviour" {
                " : MonoBehaviour"
            } else if short_parent == "ScriptableObject" {
                " : ScriptableObject"
            } else {
                " : MonoBehaviour"
            }
        };

        let mut out = String::with_capacity(1024);
        writeln!(out, "// Recovered by DevX via il2cpp_dumper ({})", td.assembly_name).ok();
        writeln!(out, "using System;").ok();
        writeln!(out, "using System.Collections.Generic;").ok();
        writeln!(out, "using UnityEngine;").ok();
        writeln!(out).ok();
        let indent = if td.namespace.is_empty() {
            ""
        } else {
            writeln!(out, "namespace {}\n{{", td.namespace).ok();
            "    "
        };

        writeln!(
            out,
            "{indent}public {} {clean_name}{base_type}\n{indent}{{",
            td.kind
        )
        .ok();

        for f in &td.fields {
            if f.is_const || f.is_static || f.name.contains('<') || f.name.is_empty() {
                continue;
            }
            let cs_type = map_il2cpp_type_to_cs(&f.type_name);
            let field_ident = sanitize_cs_ident(&f.name);
            if !f.is_public {
                writeln!(out, "{indent}    [SerializeField] // Offset: 0x{:X}", f.offset.max(0)).ok();
                writeln!(out, "{indent}    private {cs_type} {field_ident};").ok();
            } else {
                writeln!(out, "{indent}    public {cs_type} {field_ident}; // Offset: 0x{:X}", f.offset.max(0)).ok();
            }
        }

        writeln!(out, "{indent}}}").ok();
        if !td.namespace.is_empty() {
            writeln!(out, "}}").ok();
        }
        Some(out)
    }
}

fn sanitize_cs_ident(raw: &str) -> String {
    let base = raw.split('`').next().unwrap_or(raw);
    let mut s: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if s.is_empty() || s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    s
}

fn map_il2cpp_type_to_cs(ty: &str) -> String {
    let clean = ty.trim();
    match clean {
        "System.Int32" | "int" => "int".to_owned(),
        "System.UInt32" | "uint" => "uint".to_owned(),
        "System.Int64" | "long" => "long".to_owned(),
        "System.UInt64" | "ulong" => "ulong".to_owned(),
        "System.Int16" | "short" => "short".to_owned(),
        "System.UInt16" | "ushort" => "ushort".to_owned(),
        "System.Byte" | "byte" => "byte".to_owned(),
        "System.SByte" | "sbyte" => "sbyte".to_owned(),
        "System.Single" | "float" => "float".to_owned(),
        "System.Double" | "double" => "double".to_owned(),
        "System.Boolean" | "bool" => "bool".to_owned(),
        "System.String" | "string" => "string".to_owned(),
        _ => {
            if let Some(inner) = clean.strip_prefix("System.Collections.Generic.List<") {
                return format!("List<{inner}");
            }
            if let Some(stripped) = clean.strip_prefix("UnityEngine.") {
                return stripped.to_owned();
            }
            clean.to_owned()
        }
    }
}

/// Summary extracted from Unity's IL2CPP `global-metadata.dat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Il2CppMetadataSummary {
    pub virtual_path: String,
    pub format_version: i32,
    pub string_literal_count: usize,
    pub string_table_bytes: usize,
    pub total_size_bytes: usize,
    pub string_literals_preview: Vec<String>,
    pub symbol_strings_preview: Vec<String>,
}

/// Parses `global-metadata.dat` header (`0xFAB11BAF`), string literals, and UTF-8 identifier table.
pub fn parse_global_metadata(
    virtual_path: &str,
    bytes: &[u8],
    max_preview_entries: usize,
) -> ScriptResult<Il2CppMetadataSummary> {
    if bytes.len() < 64 {
        return Err(ScriptError::InvalidIl2Cpp(
            "global-metadata.dat shorter than 64 bytes".to_owned(),
        ));
    }

    let magic = read_u32(bytes, 0)?;
    if magic != IL2CPP_METADATA_MAGIC {
        return Err(ScriptError::InvalidIl2Cpp(format!(
            "unexpected magic 0x{magic:08X}, expected 0x{IL2CPP_METADATA_MAGIC:08X}"
        )));
    }
    let version = read_i32(bytes, 4)?;

    // Across IL2CPP metadata versions 16..=31+:
    // offset  8: stringLiteralOffset (u32), offset 12: stringLiteralCount (u32) [byte size]
    // offset 16: stringLiteralDataOffset (u32), offset 20: stringLiteralDataCount (u32)
    // offset 24: stringOffset (u32), offset 28: stringCount (u32)
    let lit_off = read_u32(bytes, 8)? as usize;
    let lit_bytes = read_u32(bytes, 12)? as usize;
    let lit_data_off = read_u32(bytes, 16)? as usize;
    let lit_data_bytes = read_u32(bytes, 20)? as usize;
    let str_off = read_u32(bytes, 24)? as usize;
    let str_bytes = read_u32(bytes, 28)? as usize;

    let mut string_literals_preview = Vec::new();
    let lit_count = lit_bytes / 8;
    if lit_off + lit_bytes <= bytes.len() && lit_data_off + lit_data_bytes <= bytes.len() {
        let data_tbl = &bytes[lit_data_off..lit_data_off + lit_data_bytes];
        for idx in 0..lit_count.min(max_preview_entries) {
            let entry_off = lit_off + idx * 8;
            let len = read_u32(bytes, entry_off)? as usize;
            let data_index = read_u32(bytes, entry_off + 4)? as usize;
            if let Some(slice) = data_tbl.get(data_index..data_index.saturating_add(len)) {
                string_literals_preview.push(String::from_utf8_lossy(slice).into_owned());
            }
        }
    }

    let mut symbol_strings_preview = Vec::new();
    if str_off + str_bytes <= bytes.len() {
        let str_tbl = &bytes[str_off..str_off + str_bytes];
        let mut start = 0usize;
        for (idx, &b) in str_tbl.iter().enumerate() {
            if b == 0 {
                if idx > start
                    && let Ok(s) = std::str::from_utf8(&str_tbl[start..idx])
                {
                    symbol_strings_preview.push(s.to_owned());
                    if symbol_strings_preview.len() >= max_preview_entries {
                        break;
                    }
                }
                start = idx + 1;
            }
        }
    }

    Ok(Il2CppMetadataSummary {
        virtual_path: virtual_path.to_owned(),
        format_version: version,
        string_literal_count: lit_count,
        string_table_bytes: str_bytes,
        total_size_bytes: bytes.len(),
        string_literals_preview,
        symbol_strings_preview,
    })
}

/// Parses a hex or `0x`-prefixed RVA string (underscores and whitespace ignored).
pub fn parse_rva_hex(input: &str) -> ScriptResult<u64> {
    let trimmed = input.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed)
        .replace('_', "");
    if hex.is_empty() {
        return Err(ScriptError::InvalidIl2Cpp("empty RVA value".to_owned()));
    }
    u64::from_str_radix(&hex, 16)
        .map_err(|_| ScriptError::InvalidIl2Cpp(format!("invalid hex RVA `{trimmed}`")))
}

fn read_u32(bytes: &[u8], off: usize) -> ScriptResult<u32> {
    let b: [u8; 4] = bytes
        .get(off..off + 4)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| ScriptError::InvalidIl2Cpp(format!("truncated u32 at 0x{off:X}")))?;
    Ok(u32::from_le_bytes(b))
}

fn read_i32(bytes: &[u8], off: usize) -> ScriptResult<i32> {
    Ok(read_u32(bytes, off)? as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synthetic_il2cpp_global_metadata() {
        let lit_data = b"HelloIL2CPP";
        let sym_data = b"\0PlayerHealth\0TakeDamage\0";

        let mut buf = vec![0u8; 64];
        buf[0..4].copy_from_slice(&IL2CPP_METADATA_MAGIC.to_le_bytes());
        buf[4..8].copy_from_slice(&29i32.to_le_bytes());

        let lit_off = buf.len() as u32;
        buf.extend_from_slice(&(lit_data.len() as u32).to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        let lit_bytes = 8u32;

        let lit_data_off = buf.len() as u32;
        buf.extend_from_slice(lit_data);
        let lit_data_bytes = lit_data.len() as u32;

        let str_off = buf.len() as u32;
        buf.extend_from_slice(sym_data);
        let str_bytes = sym_data.len() as u32;

        buf[8..12].copy_from_slice(&lit_off.to_le_bytes());
        buf[12..16].copy_from_slice(&lit_bytes.to_le_bytes());
        buf[16..20].copy_from_slice(&lit_data_off.to_le_bytes());
        buf[20..24].copy_from_slice(&lit_data_bytes.to_le_bytes());
        buf[24..28].copy_from_slice(&str_off.to_le_bytes());
        buf[28..32].copy_from_slice(&str_bytes.to_le_bytes());

        let parsed = parse_global_metadata("global-metadata.dat", &buf, 100).expect("il2cpp ok");
        assert_eq!(parsed.format_version, 29);
        assert_eq!(parsed.string_literals_preview, vec!["HelloIL2CPP"]);
        assert_eq!(
            parsed.symbol_strings_preview,
            vec!["PlayerHealth", "TakeDamage"]
        );
    }

    #[test]
    fn disasm_options_and_export_config_defaults() {
        let disasm = Il2CppDisasmOptions::default();
        assert!(disasm.hex_bytes);
        assert!(disasm.field_names);
        assert!(disasm.annotations);
        assert!(disasm.cfg);
        assert_eq!(disasm.max_instructions, 512);

        let cfg = Il2CppExportConfig::default();
        assert!(cfg.write_dump_cs);
        assert!(cfg.write_diffable_cs);
        assert!(cfg.write_script_json_and_headers);
        assert!(!cfg.write_dummy_dlls);
        assert!(!cfg.write_generics_dump);
        assert!(!cfg.include_pseudo_c);
        assert!(!cfg.include_disassembly);
        assert_eq!(cfg.disasm, disasm);
        assert!(cfg.include_assemblies.is_empty());
        assert!(cfg.include_types.is_empty());
    }

    #[test]
    fn parses_hex_rva_variants() {
        assert_eq!(parse_rva_hex("0x1A2B_3C").unwrap(), 0x1A2B3C);
        assert_eq!(parse_rva_hex(" 0Xdeadbeef ").unwrap(), 0xDEADBEEF);
        assert_eq!(parse_rva_hex("401001").unwrap(), 0x401001);
        assert!(parse_rva_hex("").is_err());
        assert!(parse_rva_hex("0xZZ").is_err());
    }
}
