//! Binary `.oc` configuration table decoder driven by IL2CPP `cfg.*` schemas.
//!
//! `Share.Octets` files stack fixed-order records back-to-back until EOF with little-endian
//! numeric primitives, `i32`-length UTF-8 strings, `i32`-counted lists/dictionaries, nested
//! `cfg.*` structs, singleton static tables (e.g. `cfg.Consts`, `cfg.ConstsBs`), and
//! pipe-delimited (`'|'`) string-encoded primitive lists declared in `Unmarshal(Octets)`.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::il2cpp::{Il2CppDumpReport, Il2CppRecoveredType};
use crate::{ScriptError, ScriptResult};

const MAX_CONTAINER_ITEMS: usize = 500_000;
const MAX_NESTED_DEPTH: usize = 12;

/// Primitive binary field encodings supported by `Share.Octets`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OcPrimitive {
    Bool,
    Byte,
    SByte,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    Float,
    Double,
    String,
}

impl OcPrimitive {
    /// Maps a recovered IL2CPP C# primitive name to its `Share.Octets` reader kind.
    pub fn from_cs_type(name: &str) -> Option<Self> {
        match name.trim() {
            "bool" | "Boolean" | "System.Boolean" => Some(Self::Bool),
            "byte" | "Byte" | "System.Byte" => Some(Self::Byte),
            "sbyte" | "SByte" | "System.SByte" => Some(Self::SByte),
            "short" | "Int16" | "System.Int16" => Some(Self::Short),
            "ushort" | "UInt16" | "System.UInt16" => Some(Self::UShort),
            "int" | "Int32" | "System.Int32" => Some(Self::Int),
            "uint" | "UInt32" | "System.UInt32" => Some(Self::UInt),
            "long" | "Int64" | "System.Int64" => Some(Self::Long),
            "ulong" | "UInt64" | "System.UInt64" => Some(Self::ULong),
            "float" | "Single" | "System.Single" => Some(Self::Float),
            "double" | "Double" | "System.Double" => Some(Self::Double),
            "string" | "String" | "System.String" => Some(Self::String),
            _ => None,
        }
    }

    fn is_numeric(self) -> bool {
        !matches!(self, Self::String | Self::Bool)
    }
}

/// Recursive `Share.Octets` field type resolved from IL2CPP metadata.
#[derive(Debug, Clone)]
pub enum OcFieldType {
    Primitive(OcPrimitive),
    List(Box<OcFieldType>),
    Dictionary(Box<OcFieldType>, Box<OcFieldType>),
    Struct {
        type_def_index: usize,
        type_full_name: String,
        fields: Vec<OcField>,
    },
}

/// One serializable field inside a `cfg.*` class or nested helper struct.
#[derive(Debug, Clone)]
pub struct OcField {
    pub name: String,
    pub ty: OcFieldType,
}

/// Complete schema extracted from an IL2CPP `cfg.*` class for `.oc` table decoding.
#[derive(Debug, Clone)]
pub struct OcTableSchema {
    pub type_def_index: usize,
    pub type_full_name: String,
    pub single_record: bool,
    pub fields: Vec<OcField>,
}

/// One raw `.oc` file input paired with its virtual path or filename.
#[derive(Debug, Clone)]
pub struct OcFileInput<'a> {
    pub virtual_path: &'a str,
    pub bytes: &'a [u8],
}

/// Controls validation strictness when decoding `.oc` tables.
#[derive(Debug, Clone, Copy, Default)]
pub struct OcDecodeOptions {
    /// Reject pipe-delimited numeric lists that contain non-numeric legacy strings instead of
    /// preserving those cells as JSON strings.
    pub strict_pipe_values: bool,
}

/// Decoded JSON payload and schema diagnostics for a single `.oc` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedOcTable {
    pub file_stem: String,
    pub source_path: String,
    pub type_full_name: String,
    pub is_singleton: bool,
    pub byte_len: usize,
    pub pipe_fields: Vec<String>,
    pub dirty_pipe_cells: usize,
    pub records: Vec<Value>,
}

/// Per-file failure encountered while batch-decoding `.oc` tables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcDecodeError {
    pub source_path: String,
    pub message: String,
}

/// Batch summary across a workspace or directory of `.oc` files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcDecodeReport {
    pub tables: Vec<DecodedOcTable>,
    pub skipped_unmatched: Vec<String>,
    pub errors: Vec<OcDecodeError>,
}

/// Streaming cursor over a little-endian `Share.Octets` buffer.
pub struct OctetsReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> OctetsReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| format!("offset overflow at {}/{}", self.pos, self.bytes.len()))?;
        if end > self.bytes.len() {
            return Err(format!(
                "unexpected EOF at {}/{} (need {len} bytes)",
                self.pos,
                self.bytes.len()
            ));
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    pub fn pop_byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    pub fn pop_sbyte(&mut self) -> Result<i8, String> {
        Ok(self.pop_byte()? as i8)
    }

    pub fn pop_bool(&mut self) -> Result<bool, String> {
        let at = self.pos;
        match self.pop_byte()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(format!("invalid boolean byte {other:#04x} at {at}")),
        }
    }

    pub fn pop_short(&mut self) -> Result<i16, String> {
        let b: [u8; 2] = self.take(2)?.try_into().map_err(|_| "short slice")?;
        Ok(i16::from_le_bytes(b))
    }

    pub fn pop_ushort(&mut self) -> Result<u16, String> {
        let b: [u8; 2] = self.take(2)?.try_into().map_err(|_| "ushort slice")?;
        Ok(u16::from_le_bytes(b))
    }

    pub fn pop_int(&mut self) -> Result<i32, String> {
        let b: [u8; 4] = self.take(4)?.try_into().map_err(|_| "int slice")?;
        Ok(i32::from_le_bytes(b))
    }

    pub fn pop_uint(&mut self) -> Result<u32, String> {
        let b: [u8; 4] = self.take(4)?.try_into().map_err(|_| "uint slice")?;
        Ok(u32::from_le_bytes(b))
    }

    pub fn pop_long(&mut self) -> Result<i64, String> {
        let b: [u8; 8] = self.take(8)?.try_into().map_err(|_| "long slice")?;
        Ok(i64::from_le_bytes(b))
    }

    pub fn pop_ulong(&mut self) -> Result<u64, String> {
        let b: [u8; 8] = self.take(8)?.try_into().map_err(|_| "ulong slice")?;
        Ok(u64::from_le_bytes(b))
    }

    pub fn pop_float(&mut self) -> Result<f32, String> {
        let b: [u8; 4] = self.take(4)?.try_into().map_err(|_| "float slice")?;
        Ok(f32::from_le_bytes(b))
    }

    pub fn pop_double(&mut self) -> Result<f64, String> {
        let b: [u8; 8] = self.take(8)?.try_into().map_err(|_| "double slice")?;
        Ok(f64::from_le_bytes(b))
    }

    pub fn pop_string(&mut self) -> Result<String, String> {
        let at = self.pos;
        let len = self.pop_int()?;
        if len < 0 {
            return Err(format!("negative string length {len} at {at}"));
        }
        let len_usize = len as usize;
        if len_usize > self.remaining() {
            return Err(format!(
                "string length {len_usize} at {at} exceeds remaining {} bytes",
                self.remaining()
            ));
        }
        let raw = self.take(len_usize)?;
        std::str::from_utf8(raw)
            .map(ToOwned::to_owned)
            .map_err(|e| format!("invalid UTF-8 string at {at} (len {len_usize}): {e}"))
    }

    pub fn pop_count(&mut self, label: &str) -> Result<usize, String> {
        let at = self.pos;
        let count = self.pop_int()?;
        if count < 0 || (count as usize) > MAX_CONTAINER_ITEMS {
            return Err(format!("invalid {label} count {count} at {at}"));
        }
        Ok(count as usize)
    }
}

/// Extracts the base `.oc` filename stem without `.oc` (e.g. `cfg.CloseWeaponCfg`).
pub fn oc_file_stem(virtual_path: &str) -> &str {
    let normalized = virtual_path.rsplit('/').next().unwrap_or(virtual_path);
    let normalized = normalized.rsplit('\\').next().unwrap_or(normalized);
    normalized
        .strip_suffix(".oc")
        .or_else(|| normalized.strip_suffix(".OC"))
        .unwrap_or(normalized)
}

/// Resolves an IL2CPP `cfg.*` type matching an `.oc` file stem like `cfg.CloseWeaponCfg.oc`.
pub fn resolve_cfg_type_for_path<'a>(
    report: &'a Il2CppDumpReport,
    virtual_path: &str,
) -> Option<&'a Il2CppRecoveredType> {
    let stem = oc_file_stem(virtual_path);
    if let Some((ns, name)) = stem.split_once('.')
        && ns.eq_ignore_ascii_case("cfg")
    {
        return report.find_type("Assembly-CSharp", "cfg", name);
    }
    report.find_type("Assembly-CSharp", "cfg", stem)
}

/// Builds an [`OcTableSchema`] from IL2CPP type metadata for `ty`.
pub fn build_oc_schema(
    report: &Il2CppDumpReport,
    ty: &Il2CppRecoveredType,
) -> Result<OcTableSchema, String> {
    let mut visiting = HashSet::new();
    build_oc_schema_inner(report, ty, &mut visiting)
}

fn build_oc_schema_inner(
    report: &Il2CppDumpReport,
    ty: &Il2CppRecoveredType,
    visiting: &mut HashSet<usize>,
) -> Result<OcTableSchema, String> {
    let instance_fields: Vec<_> = ty
        .fields
        .iter()
        .filter(|f| !f.is_static && !f.is_const)
        .collect();

    let (raw_fields, single_record) = if !instance_fields.is_empty() {
        (instance_fields, false)
    } else {
        // Singleton config tables (e.g. `cfg.Consts`, `cfg.ConstsBs`) load all static non-cache
        // fields once sequentially in `Load(byte[])`.
        let static_fields: Vec<_> = ty
            .fields
            .iter()
            .filter(|f| f.is_static && !f.is_const && f.name != "all" && f.name != "allList")
            .collect();
        (static_fields, true)
    };

    if raw_fields.is_empty() {
        return Err(format!("type `{}` exposes no config fields", ty.full_name));
    }

    visiting.insert(ty.type_def_index);
    let mut fields = Vec::with_capacity(raw_fields.len());
    for field in raw_fields {
        let clean_type = field.type_name.trim_start_matches("readonly ").trim();
        if let Some(parsed) = resolve_field_type(report, ty, clean_type, visiting, 0)? {
            fields.push(OcField {
                name: field.name.clone(),
                ty: parsed,
            });
        }
    }
    visiting.remove(&ty.type_def_index);

    if fields.is_empty() {
        return Err(format!(
            "type `{}` has no serializable .oc fields",
            ty.full_name
        ));
    }

    Ok(OcTableSchema {
        type_def_index: ty.type_def_index,
        type_full_name: ty.full_name.clone(),
        single_record,
        fields,
    })
}

fn resolve_field_type(
    report: &Il2CppDumpReport,
    owner: &Il2CppRecoveredType,
    type_name: &str,
    visiting: &mut HashSet<usize>,
    depth: usize,
) -> Result<Option<OcFieldType>, String> {
    if depth > MAX_NESTED_DEPTH {
        return Err(format!("nested type depth exceeded on `{type_name}`"));
    }
    let trimmed = type_name.trim();
    if let Some(prim) = OcPrimitive::from_cs_type(trimmed) {
        return Ok(Some(OcFieldType::Primitive(prim)));
    }
    if let Some(inner) = strip_generic(trimmed, "List") {
        let Some(inner_ty) = resolve_field_type(report, owner, inner, visiting, depth + 1)?
        else {
            return Ok(None);
        };
        return Ok(Some(OcFieldType::List(Box::new(inner_ty))));
    }
    if let Some(kv_str) = strip_generic(trimmed, "Dictionary") {
        let Some((k_raw, v_raw)) = split_top_level_comma(kv_str) else {
            return Err(format!("malformed Dictionary generic `{trimmed}`"));
        };
        let (Some(kt), Some(vt)) = (
            resolve_field_type(report, owner, k_raw, visiting, depth + 1)?,
            resolve_field_type(report, owner, v_raw, visiting, depth + 1)?,
        ) else {
            return Ok(None);
        };
        return Ok(Some(OcFieldType::Dictionary(Box::new(kt), Box::new(vt))));
    }

    // Look up nested config structs inside the owning `cfg` assembly/namespace first so runtime
    // MonoBehaviours with colliding short names (e.g. `EffectInfo`) never shadow `cfg.*`.
    if let Some(nested) = find_nested_cfg_type(report, owner, trimmed) {
        if nested.kind == "enum" {
            return Ok(Some(OcFieldType::Primitive(OcPrimitive::Int)));
        }
        if visiting.contains(&nested.type_def_index) {
            return Err(format!(
                "cyclic nested config struct `{}`",
                nested.full_name
            ));
        }
        let sub = build_oc_schema_inner(report, nested, visiting)?;
        return Ok(Some(OcFieldType::Struct {
            type_def_index: sub.type_def_index,
            type_full_name: sub.type_full_name,
            fields: sub.fields,
        }));
    }

    // Skip non-serializable Unity/runtime fields (e.g., `ParticleSystem`, `GameObject`).
    Ok(None)
}

fn find_nested_cfg_type<'a>(
    report: &'a Il2CppDumpReport,
    owner: &Il2CppRecoveredType,
    raw_name: &str,
) -> Option<&'a Il2CppRecoveredType> {
    let short = raw_name.rsplit('.').next().unwrap_or(raw_name);
    let candidate = if let Some((ns, name)) = raw_name.rsplit_once('.') {
        report.find_type(&owner.assembly_name, ns, name)
    } else {
        report.find_type(&owner.assembly_name, &owner.namespace, short)
    }?;
    let same_asm = candidate
        .assembly_name
        .eq_ignore_ascii_case(&owner.assembly_name);
    let is_cfg_ns = candidate.namespace == owner.namespace
        || candidate.namespace == "cfg"
        || candidate.namespace.starts_with("cfg.");
    if same_asm && is_cfg_ns {
        Some(candidate)
    } else {
        None
    }
}

fn strip_generic<'a>(ty: &'a str, wrapper: &str) -> Option<&'a str> {
    let rest = ty
        .strip_prefix(wrapper)
        .or_else(|| ty.strip_prefix(&format!("System.Collections.Generic.{wrapper}")))?;
    rest.strip_prefix('<')?.strip_suffix('>')
}

fn split_top_level_comma(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (idx, ch) in s.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                return Some((s[..idx].trim(), s[idx + 1..].trim()));
            }
            _ => {}
        }
    }
    None
}

/// Collects all `TypeDefIndex` values referenced by a schema (including nested helper structs).
pub fn collect_schema_type_indices(schema: &OcTableSchema, out: &mut HashSet<usize>) {
    out.insert(schema.type_def_index);
    for f in &schema.fields {
        collect_field_type_indices(&f.ty, out);
    }
}

fn collect_field_type_indices(ft: &OcFieldType, out: &mut HashSet<usize>) {
    match ft {
        OcFieldType::Primitive(_) => {}
        OcFieldType::List(inner) => collect_field_type_indices(inner, out),
        OcFieldType::Dictionary(k, v) => {
            collect_field_type_indices(k, out);
            collect_field_type_indices(v, out);
        }
        OcFieldType::Struct {
            type_def_index,
            fields,
            ..
        } => {
            out.insert(*type_def_index);
            for f in fields {
                collect_field_type_indices(&f.ty, out);
            }
        }
    }
}

/// Identifies `(type_full_name, field_name)` pairs that `Unmarshal(Octets)` parses via
/// `oc.pop_string().Split('|')` (`Split(124, ...)` in IL2CPP pseudo-C).
pub fn extract_pipe_split_fields_from_pseudo_c(
    pseudo_c_source: &str,
    list_primitive_fields: &HashSet<&str>,
) -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(body) = extract_unmarshal_method_body(pseudo_c_source) else {
        return out;
    };

    let mut pending_split = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.contains(".Split(124") || trimmed.contains(".Split('|'") {
            pending_split = true;
            continue;
        }
        if !pending_split {
            continue;
        }
        if let Some(field) = find_referenced_this_field(trimmed)
            && list_primitive_fields.contains(field)
        {
            out.insert(field.to_owned());
            pending_split = false;
            continue;
        }
        // An unrelated `oc.pop_*` read for another field clears a previous split window.
        if trimmed.contains("= oc.pop_") && !trimmed.contains(".Split(") {
            pending_split = false;
        }
    }
    out
}

fn extract_unmarshal_method_body(src: &str) -> Option<&str> {
    let sig_pos = src.find("void Unmarshal(")?;
    let open_rel = src[sig_pos..].find('{')?;
    let start = sig_pos + open_rel;
    let mut depth = 0i32;
    for (idx, ch) in src[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&src[start..=start + idx]);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_referenced_this_field(line: &str) -> Option<&str> {
    let rest = line.split("this.").nth(1)?;
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    let ident = &rest[..end];
    if ident.is_empty() {
        None
    } else {
        Some(ident)
    }
}

/// Strategy describing which `List<primitive>` fields use pipe-delimited strings instead of
/// binary `[count: i32, elem...]`.
#[derive(Debug, Clone, Default)]
pub enum PipeFieldPolicy<'a> {
    /// Assume all lists are binary `[count: i32, elements...]`.
    #[default]
    None,
    /// Assume every `List<primitive>` field uses pipe-delimited strings.
    AllPrimitiveLists,
    /// Exact `(type_full_name, field_name)` set recovered from `Unmarshal(Octets)`.
    Exact(&'a HashSet<(String, String)>),
}

impl PipeFieldPolicy<'_> {
    fn uses_pipe(&self, owner_type: &str, field_name: &str, ty: &OcFieldType) -> bool {
        let OcFieldType::List(inner) = ty else {
            return false;
        };
        if !matches!(inner.as_ref(), OcFieldType::Primitive(_)) {
            return false;
        }
        match self {
            Self::None => false,
            Self::AllPrimitiveLists => true,
            Self::Exact(set) => {
                set.contains(&(owner_type.to_owned(), field_name.to_owned()))
            }
        }
    }
}

/// Decodes an entire `.oc` byte slice using `schema` and `policy`, requiring exact EOF.
pub fn decode_oc_records(
    schema: &OcTableSchema,
    bytes: &[u8],
    policy: &PipeFieldPolicy<'_>,
    options: OcDecodeOptions,
) -> Result<(Vec<Value>, usize), String> {
    let mut reader = OctetsReader::new(bytes);
    let mut records = Vec::new();
    let mut dirty_pipe_cells = 0usize;

    while !reader.is_empty() {
        let start_pos = reader.pos();
        let mut obj = Map::with_capacity(schema.fields.len());
        for field in &schema.fields {
            let as_pipe = policy.uses_pipe(&schema.type_full_name, &field.name, &field.ty);
            let val = read_field_value(
                &mut reader,
                &field.ty,
                as_pipe,
                policy,
                options,
                &mut dirty_pipe_cells,
            )
            .map_err(|e| format!("{}.{}: {e}", schema.type_full_name, field.name))?;
            obj.insert(field.name.clone(), val);
        }
        if reader.pos() <= start_pos {
            return Err(format!("zero-byte record on `{}`", schema.type_full_name));
        }
        records.push(Value::Object(obj));
        if schema.single_record {
            break;
        }
    }

    if !reader.is_empty() {
        return Err(format!(
            "trailing {} bytes after {} record(s) on `{}` ({}/{})",
            reader.remaining(),
            records.len(),
            schema.type_full_name,
            reader.pos(),
            bytes.len()
        ));
    }

    Ok((records, dirty_pipe_cells))
}

fn read_field_value(
    reader: &mut OctetsReader<'_>,
    ty: &OcFieldType,
    as_pipe: bool,
    policy: &PipeFieldPolicy<'_>,
    options: OcDecodeOptions,
    dirty_pipe_cells: &mut usize,
) -> Result<Value, String> {
    match ty {
        OcFieldType::Primitive(p) => read_primitive(reader, *p),
        OcFieldType::List(inner) => {
            if as_pipe && let OcFieldType::Primitive(prim) = inner.as_ref() {
                let raw = reader.pop_string()?;
                return parse_pipe_string(&raw, *prim, options, dirty_pipe_cells);
            }
            let count = reader.pop_count("List")?;
            let mut items = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                items.push(read_field_value(
                    reader,
                    inner,
                    false,
                    policy,
                    options,
                    dirty_pipe_cells,
                )?);
            }
            Ok(Value::Array(items))
        }
        OcFieldType::Dictionary(k_ty, v_ty) => {
            let count = reader.pop_count("Dictionary")?;
            let mut map = Map::with_capacity(count.min(1024));
            for _ in 0..count {
                let k = read_field_value(reader, k_ty, false, policy, options, dirty_pipe_cells)?;
                let v = read_field_value(reader, v_ty, false, policy, options, dirty_pipe_cells)?;
                map.insert(json_key_string(&k), v);
            }
            Ok(Value::Object(map))
        }
        OcFieldType::Struct {
            type_full_name,
            fields,
            ..
        } => {
            let mut obj = Map::with_capacity(fields.len());
            for field in fields {
                let sub_pipe = policy.uses_pipe(type_full_name, &field.name, &field.ty);
                let v = read_field_value(
                    reader,
                    &field.ty,
                    sub_pipe,
                    policy,
                    options,
                    dirty_pipe_cells,
                )?;
                obj.insert(field.name.clone(), v);
            }
            Ok(Value::Object(obj))
        }
    }
}

fn parse_pipe_string(
    raw: &str,
    prim: OcPrimitive,
    options: OcDecodeOptions,
    dirty_pipe_cells: &mut usize,
) -> Result<Value, String> {
    if raw.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    let mut items = Vec::new();
    for part in raw.split('|') {
        if part.is_empty() && prim.is_numeric() {
            continue;
        }
        let parsed = match prim {
            OcPrimitive::String => Ok(Value::String(part.to_owned())),
            OcPrimitive::Bool => match part.trim() {
                "0" | "false" | "False" | "FALSE" => Ok(Value::Bool(false)),
                "1" | "true" | "True" | "TRUE" => Ok(Value::Bool(true)),
                _ => Err(format!("invalid pipe bool `{part}`")),
            },
            OcPrimitive::Byte | OcPrimitive::UShort | OcPrimitive::UInt | OcPrimitive::ULong => part
                .trim()
                .parse::<u64>()
                .map(|n| Value::Number(n.into()))
                .map_err(|_| format!("invalid unsigned pipe int `{part}`")),
            OcPrimitive::SByte | OcPrimitive::Short | OcPrimitive::Int | OcPrimitive::Long => part
                .trim()
                .parse::<i64>()
                .map(|n| Value::Number(n.into()))
                .map_err(|_| format!("invalid signed pipe int `{part}`")),
            OcPrimitive::Float | OcPrimitive::Double => part
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(Number::from_f64)
                .map(Value::Number)
                .ok_or_else(|| format!("invalid pipe float `{part}`")),
        };
        match parsed {
            Ok(v) => items.push(v),
            Err(err) => {
                if options.strict_pipe_values {
                    return Err(err);
                }
                *dirty_pipe_cells += 1;
                items.push(Value::String(part.to_owned()));
            }
        }
    }
    Ok(Value::Array(items))
}

fn read_primitive(reader: &mut OctetsReader<'_>, prim: OcPrimitive) -> Result<Value, String> {
    Ok(match prim {
        OcPrimitive::Bool => Value::Bool(reader.pop_bool()?),
        OcPrimitive::Byte => Value::Number(u64::from(reader.pop_byte()?).into()),
        OcPrimitive::SByte => Value::Number(i64::from(reader.pop_sbyte()?).into()),
        OcPrimitive::Short => Value::Number(i64::from(reader.pop_short()?).into()),
        OcPrimitive::UShort => Value::Number(u64::from(reader.pop_ushort()?).into()),
        OcPrimitive::Int => Value::Number(i64::from(reader.pop_int()?).into()),
        OcPrimitive::UInt => Value::Number(u64::from(reader.pop_uint()?).into()),
        OcPrimitive::Long => Value::Number(reader.pop_long()?.into()),
        OcPrimitive::ULong => Value::Number(reader.pop_ulong()?.into()),
        OcPrimitive::Float => Number::from_f64(f64::from(reader.pop_float()?))
            .map(Value::Number)
            .unwrap_or(Value::Null),
        OcPrimitive::Double => Number::from_f64(reader.pop_double()?)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        OcPrimitive::String => Value::String(reader.pop_string()?),
    })
}

fn json_key_string(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

fn collect_list_primitive_field_names<'a>(
    schema: &'a OcTableSchema,
    out: &mut HashMap<usize, HashSet<&'a str>>,
) {
    for f in &schema.fields {
        collect_field_list_names(&f.ty, &f.name, schema.type_def_index, out);
    }
}

fn collect_field_list_names<'a>(
    ft: &'a OcFieldType,
    field_name: &'a str,
    owner_td: usize,
    all: &mut HashMap<usize, HashSet<&'a str>>,
) {
    match ft {
        OcFieldType::Primitive(_) => {}
        OcFieldType::List(inner) => {
            if matches!(inner.as_ref(), OcFieldType::Primitive(_)) {
                all.entry(owner_td).or_default().insert(field_name);
            } else {
                collect_field_list_names(inner, field_name, owner_td, all);
            }
        }
        OcFieldType::Dictionary(k, v) => {
            collect_field_list_names(k, field_name, owner_td, all);
            collect_field_list_names(v, field_name, owner_td, all);
        }
        OcFieldType::Struct {
            type_def_index,
            fields,
            ..
        } => {
            for sf in fields {
                collect_field_list_names(&sf.ty, &sf.name, *type_def_index, all);
            }
        }
    }
}

/// Decodes a collection of `.oc` buffers against schemas recovered in `report`.
///
/// Fast path validates pure-binary tables first; only tables failing the binary pass trigger a
/// single batched `Unmarshal(Octets)` Pseudo-C pass (when native code is present) so mixed
/// binary/pipe-delimited fields decode deterministically without heuristic guesswork.
pub fn decode_oc_batch(
    report: &Il2CppDumpReport,
    inputs: &[OcFileInput<'_>],
    options: OcDecodeOptions,
) -> ScriptResult<OcDecodeReport> {
    let mut skipped_unmatched = Vec::new();
    let mut pending: Vec<(usize, OcTableSchema)> = Vec::new();
    let mut errors = Vec::new();

    for (idx, item) in inputs.iter().enumerate() {
        let Some(ty) = resolve_cfg_type_for_path(report, item.virtual_path) else {
            skipped_unmatched.push(item.virtual_path.to_owned());
            continue;
        };
        match build_oc_schema(report, ty) {
            Ok(schema) => pending.push((idx, schema)),
            Err(msg) => errors.push(OcDecodeError {
                source_path: item.virtual_path.to_owned(),
                message: msg,
            }),
        }
    }

    let mut tables = Vec::with_capacity(pending.len());
    let mut need_retry: Vec<(usize, OcTableSchema, String)> = Vec::new();

    for (idx, schema) in pending {
        let item = &inputs[idx];
        match decode_oc_records(&schema, item.bytes, &PipeFieldPolicy::None, options) {
            Ok((records, dirty)) => {
                tables.push(DecodedOcTable {
                    file_stem: oc_file_stem(item.virtual_path).to_owned(),
                    source_path: item.virtual_path.to_owned(),
                    type_full_name: schema.type_full_name,
                    is_singleton: schema.single_record,
                    byte_len: item.bytes.len(),
                    pipe_fields: Vec::new(),
                    dirty_pipe_cells: dirty,
                    records,
                });
            }
            Err(bin_err) => need_retry.push((idx, schema, bin_err)),
        }
    }

    if !need_retry.is_empty() {
        let exact_pipe_map = resolve_pipe_fields_for_retries(report, &need_retry)?;
        for (idx, schema, bin_err) in need_retry {
            let item = &inputs[idx];
            let exact_policy = PipeFieldPolicy::Exact(&exact_pipe_map);
            let retry_res = if !exact_pipe_map.is_empty() {
                decode_oc_records(&schema, item.bytes, &exact_policy, options).or_else(|e_exact| {
                    decode_oc_records(&schema, item.bytes, &PipeFieldPolicy::AllPrimitiveLists, options)
                        .map_err(|e_all| format!("{e_exact}; all-pipe fallback: {e_all}"))
                })
            } else {
                // Metadata-only inputs without native code fall back cleanly to uniform pipe lists.
                decode_oc_records(
                    &schema,
                    item.bytes,
                    &PipeFieldPolicy::AllPrimitiveLists,
                    OcDecodeOptions {
                        strict_pipe_values: true,
                    },
                )
            };

            match retry_res {
                Ok((records, dirty)) => {
                    let pipe_fields = collect_used_pipe_fields(&schema, &exact_pipe_map);
                    tables.push(DecodedOcTable {
                        file_stem: oc_file_stem(item.virtual_path).to_owned(),
                        source_path: item.virtual_path.to_owned(),
                        type_full_name: schema.type_full_name,
                        is_singleton: schema.single_record,
                        byte_len: item.bytes.len(),
                        pipe_fields,
                        dirty_pipe_cells: dirty,
                        records,
                    });
                }
                Err(retry_err) => errors.push(OcDecodeError {
                    source_path: item.virtual_path.to_owned(),
                    message: format!("binary failed ({bin_err}); pipe retry failed ({retry_err})"),
                }),
            }
        }
    }

    Ok(OcDecodeReport {
        tables,
        skipped_unmatched,
        errors,
    })
}

fn resolve_pipe_fields_for_retries(
    report: &Il2CppDumpReport,
    retries: &[(usize, OcTableSchema, String)],
) -> ScriptResult<HashSet<(String, String)>> {
    let mut exact = HashSet::new();
    if !report.arch_has_native_code {
        return Ok(exact);
    }

    let mut list_fields_by_td: HashMap<usize, HashSet<&str>> = HashMap::new();
    let mut td_indices = HashSet::new();
    for (_, schema, _) in retries {
        collect_schema_type_indices(schema, &mut td_indices);
        collect_list_primitive_field_names(schema, &mut list_fields_by_td);
    }

    let mut sorted_tds: Vec<usize> = td_indices.into_iter().collect();
    sorted_tds.sort_unstable();
    let td_full_names: BTreeMap<usize, &str> = sorted_tds
        .iter()
        .filter_map(|&td| report.types.iter().find(|t| t.type_def_index == td).map(|t| (td, t.full_name.as_str())))
        .collect();

    let sources = report.format_types_pseudo_c(&sorted_tds)?;
    for (td_idx, src) in sources {
        let Some(full_name) = td_full_names.get(&td_idx) else {
            continue;
        };
        let Some(allowed_fields) = list_fields_by_td.get(&td_idx) else {
            continue;
        };
        for fname in extract_pipe_split_fields_from_pseudo_c(&src, allowed_fields) {
            exact.insert(((*full_name).to_owned(), fname));
        }
    }

    Ok(exact)
}

fn collect_used_pipe_fields(
    schema: &OcTableSchema,
    exact_map: &HashSet<(String, String)>,
) -> Vec<String> {
    let mut out = Vec::new();
    collect_used_pipe_fields_inner(&schema.type_full_name, &schema.fields, exact_map, "", &mut out);
    if out.is_empty() && exact_map.is_empty() {
        for f in &schema.fields {
            if let OcFieldType::List(inner) = &f.ty
                && matches!(inner.as_ref(), OcFieldType::Primitive(_))
            {
                out.push(f.name.clone());
            }
        }
    }
    out
}

fn collect_used_pipe_fields_inner(
    owner_type: &str,
    fields: &[OcField],
    exact_map: &HashSet<(String, String)>,
    prefix: &str,
    out: &mut Vec<String>,
) {
    for f in fields {
        let qualified = if prefix.is_empty() {
            f.name.clone()
        } else {
            format!("{prefix}.{}", f.name)
        };
        if exact_map.contains(&(owner_type.to_owned(), f.name.clone())) {
            out.push(qualified.clone());
        }
        match &f.ty {
            OcFieldType::Struct {
                type_full_name,
                fields,
                ..
            } => collect_used_pipe_fields_inner(type_full_name, fields, exact_map, &qualified, out),
            OcFieldType::List(inner) => {
                if let OcFieldType::Struct {
                    type_full_name,
                    fields,
                    ..
                } = inner.as_ref()
                {
                    collect_used_pipe_fields_inner(
                        type_full_name,
                        fields,
                        exact_map,
                        &qualified,
                        out,
                    );
                }
            }
            _ => {}
        }
    }
}

#[allow(dead_code)]
fn _assert_script_error_unused(_: ScriptError) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_oc_string(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(&(s.len() as i32).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    }

    #[test]
    fn decodes_records_with_dict_nested_struct_and_exact_pipe_field() {
        let schema = OcTableSchema {
            type_def_index: 1,
            type_full_name: "cfg.SampleCfg".to_owned(),
            single_record: false,
            fields: vec![
                OcField {
                    name: "id".to_owned(),
                    ty: OcFieldType::Primitive(OcPrimitive::Int),
                },
                OcField {
                    name: "flag".to_owned(),
                    ty: OcFieldType::Primitive(OcPrimitive::Bool),
                },
                OcField {
                    name: "btns".to_owned(),
                    ty: OcFieldType::List(Box::new(OcFieldType::Primitive(OcPrimitive::Int))),
                },
                OcField {
                    name: "useSounds".to_owned(),
                    ty: OcFieldType::List(Box::new(OcFieldType::Primitive(OcPrimitive::Int))),
                },
                OcField {
                    name: "cost".to_owned(),
                    ty: OcFieldType::Dictionary(
                        Box::new(OcFieldType::Primitive(OcPrimitive::Int)),
                        Box::new(OcFieldType::Primitive(OcPrimitive::Int)),
                    ),
                },
            ],
        };

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&51001_i32.to_le_bytes());
        bytes.push(1);
        push_oc_string(&mut bytes, "12|34|56");
        bytes.extend_from_slice(&2_i32.to_le_bytes());
        bytes.extend_from_slice(&7_i32.to_le_bytes());
        bytes.extend_from_slice(&9_i32.to_le_bytes());
        bytes.extend_from_slice(&1_i32.to_le_bytes());
        bytes.extend_from_slice(&50008_i32.to_le_bytes());
        bytes.extend_from_slice(&3_i32.to_le_bytes());

        let mut exact = HashSet::new();
        exact.insert(("cfg.SampleCfg".to_owned(), "btns".to_owned()));
        let policy = PipeFieldPolicy::Exact(&exact);

        let (records, dirty) =
            decode_oc_records(&schema, &bytes, &policy, OcDecodeOptions::default()).unwrap();
        assert_eq!(dirty, 0);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["id"], 51001);
        assert_eq!(records[0]["flag"], true);
        assert_eq!(records[0]["btns"], serde_json::json!([12, 34, 56]));
        assert_eq!(records[0]["useSounds"], serde_json::json!([7, 9]));
        assert_eq!(records[0]["cost"]["50008"], 3);
    }

    #[test]
    fn extracts_only_list_pipe_fields_from_unmarshal_pseudo_c() {
        let pseudo_c = r#"
        public BuildPart Assign(BuildPart _o_) {
            if (_o_.ignoreField != null) { }
        }
        private void Unmarshal(Octets oc) {
            this.id = oc.pop_int();
            var split = oc.pop_string().Split(124, 0);
            if (split.Length >= 1) {
                this.btns._version = this.btns._version + 1;
            }
            var pop_int = oc.pop_int();
            var split2 = oc.pop_string().Split(124, 0);
            if (split2.Length < 1) {
                this.bulletModelPath = bulletModelPath2;
                return;
            }
            this.disableChangePartType._version = this.disableChangePartType._version + 1;
        }
        "#;
        let allowed: HashSet<&str> = ["btns", "useSounds", "disableChangePartType"]
            .into_iter()
            .collect();
        let found = extract_pipe_split_fields_from_pseudo_c(pseudo_c, &allowed);
        assert!(found.contains("btns"));
        assert!(found.contains("disableChangePartType"));
        assert!(!found.contains("useSounds"));
        assert!(!found.contains("bulletModelPath"));
    }

    #[test]
    fn rejects_invalid_boolean_and_truncated_string() {
        let mut reader = OctetsReader::new(&[2_u8]);
        assert!(reader.pop_bool().is_err());

        let mut bad_str = Vec::new();
        bad_str.extend_from_slice(&50_i32.to_le_bytes());
        bad_str.extend_from_slice(b"short");
        let mut str_reader = OctetsReader::new(&bad_str);
        assert!(str_reader.pop_string().is_err());
    }
}
