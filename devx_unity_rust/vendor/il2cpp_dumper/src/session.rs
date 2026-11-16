//! Non-interactive in-memory library session for DevX (`open_il2cpp_from_bytes` + exports).

use crate::config::{Config, MethodBodyMode};
use crate::disassembler::{Architecture, Disassembler, DisassemblyContext};
use crate::error::{Error, Result};
use crate::executor::Il2CppExecutor;
use crate::formats::elf::Elf;
use crate::formats::macho::{MachO, extract_fat_slice, parse_fat};
use crate::formats::nso::Nso;
use crate::formats::pe::Pe;
use crate::formats::wasm::Wasm;
use crate::il2cpp::base::{Il2Cpp, VaSegment};
use crate::il2cpp::metadata::{METADATA_MAGIC, Metadata, MetadataVariant};
use crate::output::decompiler::Il2CppDecompiler;
use crate::output::dummy_assembly_generator::generate_dummy_dlls;
use crate::output::embedded_scripts::write_scripts;
use crate::output::generics::dump_generics;
use crate::output::static_field_exporter::{StaticFieldCatalog, StaticFieldExporter};
use crate::output::struct_generator::StructGenerator;
use std::path::Path;

const MAGIC_ELF: u32 = 0x464C_457F;
const MAGIC_PE: u16 = 0x5A4D;
const MAGIC_MACHO32: u32 = 0xFEED_FACE;
const MAGIC_MACHO64: u32 = 0xFEED_FACF;
const MAGIC_MACHOFAT: u32 = 0xBEBA_FECA;
const MAGIC_NSO: u32 = 0x304F_534E;
const MAGIC_WASM: u32 = 0x6D73_6100;

/// Native disassembler formatting options shared across UI, CLI, and full dumps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Il2CppDisasmOptions {
    pub hex_bytes: bool,
    pub field_names: bool,
    pub annotations: bool,
    pub cfg: bool,
    pub max_instructions: usize,
}

impl Default for Il2CppDisasmOptions {
    fn default() -> Self {
        Self {
            hex_bytes: true,
            field_names: true,
            annotations: true,
            cfg: true,
            max_instructions: 512,
        }
    }
}

/// Summary of a single method declared on an IL2CPP `TypeDef`.
#[derive(Debug, Clone)]
pub struct Il2CppMethodEntry {
    pub method_def_index: usize,
    pub name: String,
    pub signature: String,
    pub rva: u64,
    pub va: u64,
    pub file_offset: u64,
    pub body_size: usize,
    pub is_abstract: bool,
    pub is_static: bool,
    pub thumb: bool,
}

/// Standalone native disassembly output for a method or arbitrary RVA.
#[derive(Debug, Clone)]
pub struct DisassembledMethodReport {
    pub rva: u64,
    pub clean_rva: u64,
    pub va: u64,
    pub file_offset: u64,
    pub arch: String,
    pub thumb: bool,
    pub method_name: Option<String>,
    pub instruction_count: usize,
    pub byte_size: usize,
    pub text: String,
}

/// Options controlling what artifacts `LoadedIl2CppSession::write_dump_to_dir` generates.
#[derive(Debug, Clone)]
pub struct Il2CppDumpWriteOptions {
    pub write_dump_cs: bool,
    pub write_diffable_cs: bool,
    pub write_script_json_and_headers: bool,
    pub write_dummy_dlls: bool,
    pub write_generics_dump: bool,
    pub write_static_metadata: bool,
    pub body_mode: MethodBodyMode,
    pub fold_ifix_wrappers: bool,
    pub max_method_bytes: Option<usize>,
    pub disasm: Il2CppDisasmOptions,
    pub include_assemblies: Vec<String>,
    pub include_types: Vec<String>,
    /// When true (default), `include_assemblies` / `include_types` also filter `dump.cs` /
    /// `DiffableCs/` headers when no body mode is active.
    pub filter_headers: bool,
    /// When true, emits all type/method headers and restricts only bodies to the filter selection.
    pub full_dump: bool,
}

impl Default for Il2CppDumpWriteOptions {
    fn default() -> Self {
        Self {
            write_dump_cs: true,
            write_diffable_cs: true,
            write_script_json_and_headers: true,
            write_dummy_dlls: true,
            write_generics_dump: false,
            write_static_metadata: false,
            body_mode: MethodBodyMode::Off,
            fold_ifix_wrappers: false,
            max_method_bytes: None,
            disasm: Il2CppDisasmOptions::default(),
            include_assemblies: Vec::new(),
            include_types: Vec::new(),
            filter_headers: true,
            full_dump: false,
        }
    }
}

/// High-level telemetry returned after opening an IL2CPP binary + `global-metadata.dat`.
#[derive(Debug, Clone)]
pub struct Il2CppSessionInfo {
    pub binary_format: String,
    pub metadata_version: f64,
    pub unity_version: Option<String>,
    pub metadata_decryption: Option<String>,
    pub image_count: usize,
    pub type_def_count: usize,
    pub method_def_count: usize,
    pub field_def_count: usize,
    pub string_literal_count: usize,
}

/// Holds parsed IL2CPP `Metadata`, mapped `Il2Cpp` runtime tables, and caching `Il2CppExecutor`.
pub struct LoadedIl2CppSession {
    pub metadata: Metadata,
    pub il2cpp: Il2Cpp,
    pub executor: Il2CppExecutor,
    pub config: Config,
    pub info: Il2CppSessionInfo,
    disasm_cache: Option<Option<(Disassembler, Vec<u64>)>>,
}

impl LoadedIl2CppSession {
    /// Opens and resolves an IL2CPP session purely from in-memory binary + `global-metadata.dat` bytes.
    pub fn open_from_bytes(
        binary_bytes: Vec<u8>,
        mut metadata_bytes: Vec<u8>,
        runtime_base_override: Option<u64>,
        mut config: Config,
    ) -> Result<Self> {
        config.reconcile_method_body_mode();
        let unity_version = detect_unity_version(&binary_bytes);

        let mut metadata_decryption = None;
        if read_magic_u32(&metadata_bytes) != METADATA_MAGIC {
            metadata_decryption = try_decrypt_metadata(&mut metadata_bytes);
            if metadata_decryption.is_none() {
                let magic = read_magic_u32(&metadata_bytes);
                return Err(Error::InvalidMetadata(format!(
                    "Wrong metadata magic 0x{magic:08X} (expected AF 1B B1 FA)"
                )));
            }
        }

        let mut metadata = Metadata::new_with_options(
            metadata_bytes,
            unity_version.as_deref(),
            config.codm,
        )?;

        let format = detect_format(&binary_bytes);
        let mut il2cpp = match format {
            "elf" => init_elf(binary_bytes, &metadata, &config, runtime_base_override)?,
            "pe" => init_pe(binary_bytes, &metadata, &config)?,
            "macho" => init_macho(binary_bytes, &metadata, &config)?,
            "macho_fat" => init_macho_fat(binary_bytes, &metadata, &config)?,
            "nso" => init_nso(binary_bytes, &metadata, &config)?,
            "wasm" => init_wasm(binary_bytes, &metadata, &config)?,
            _ => {
                let magic = read_magic_u32(&binary_bytes);
                return Err(Error::Other(format!(
                    "Unsupported binary format (magic: 0x{magic:08X})"
                )));
            }
        };

        metadata.apply_v108_tables(&mut il2cpp);
        il2cpp.detect_and_normalize_metadata_handles(&metadata);
        let executor = Il2CppExecutor::new(&metadata, &mut il2cpp)?;

        let info = Il2CppSessionInfo {
            binary_format: format.to_owned(),
            metadata_version: metadata.version,
            unity_version,
            metadata_decryption,
            image_count: metadata.image_defs.len(),
            type_def_count: metadata.type_defs.len(),
            method_def_count: metadata.method_defs.len(),
            field_def_count: metadata.field_defs.len(),
            string_literal_count: metadata.string_literals.len(),
        };

        Ok(Self {
            metadata,
            il2cpp,
            executor,
            config,
            info,
            disasm_cache: None,
        })
    }

    /// Returns the detected target instruction set architecture of this IL2CPP binary.
    pub fn arch(&self) -> Architecture {
        self.il2cpp.detect_architecture()
    }

    fn resolve_type_image(&self, type_def_index: usize) -> (usize, String) {
        let image_index = self
            .metadata
            .image_defs
            .iter()
            .position(|img| {
                let start = img.type_start as usize;
                let end = start.saturating_add(img.type_count as usize);
                type_def_index >= start && type_def_index < end
            })
            .unwrap_or(0);
        let image_name = self
            .metadata
            .image_defs
            .get(image_index)
            .and_then(|img| self.metadata.get_string_from_index(img.name_index).ok())
            .unwrap_or_else(|| "Assembly-CSharp.dll".to_owned());
        (image_index, image_name)
    }

    fn apply_disasm_options(cfg: &mut Config, opts: &Il2CppDisasmOptions) {
        cfg.dump_disassembly_hex_bytes = opts.hex_bytes;
        cfg.dump_disassembly_field_names = opts.field_names;
        cfg.dump_disassembly_annotations = opts.annotations;
        cfg.dump_disassembly_cfg = opts.cfg;
        if opts.max_instructions > 0 {
            cfg.max_disassembly_instructions = opts.max_instructions;
        }
    }

    fn ensure_disassembler(&mut self) -> Result<Option<&(Disassembler, Vec<u64>)>> {
        if self.disasm_cache.is_none() {
            let built = Il2CppDecompiler::build_disassembler(
                &mut self.executor,
                &self.metadata,
                &self.il2cpp,
                &self.config,
                |_| {},
            )?;
            self.disasm_cache = Some(built);
        }
        Ok(self.disasm_cache.as_ref().and_then(|opt| opt.as_ref()))
    }

    fn resolve_pseudo_c_max_body_size(max_bytes: Option<usize>) -> usize {
        match max_bytes {
            Some(0) => usize::MAX,
            Some(n) => n,
            None => 0,
        }
    }

    /// Overrides the Pseudo-C decompiler method-size ceiling (`Some(0)` removes the cap).
    pub fn set_pseudo_c_max_body_size(&mut self, max_bytes: Option<usize>) {
        self.config.dump_pseudo_c_max_body_size = Self::resolve_pseudo_c_max_body_size(max_bytes);
    }

    /// Enables or disables experimental IFIX wrapper folding on the session config.
    pub fn set_fold_ifix_wrappers(&mut self, fold: bool) {
        self.config.fold_ifix_wrappers = fold;
    }

    /// Finds `(type_def_index, method_def_index)` owning `rva`, if any managed method matches.
    pub fn find_method_owner_by_rva(&self, rva: u64) -> Option<(usize, usize)> {
        let arch = self.il2cpp.detect_architecture();
        let target_clean = if arch == Architecture::Arm32 {
            rva & !1
        } else {
            rva
        };
        for img in &self.metadata.image_defs {
            let image_name = self
                .metadata
                .get_string_from_index(img.name_index)
                .unwrap_or_default();
            let type_start = img.type_start.max(0) as usize;
            let type_end = type_start.saturating_add(img.type_count as usize);
            for td_idx in type_start..type_end {
                let Some(td) = self.metadata.type_defs.get(td_idx) else {
                    continue;
                };
                if td.method_start < 0 || td.method_count <= 0 {
                    continue;
                }
                let method_start = td.method_start as usize;
                let method_end = method_start.saturating_add(td.method_count as usize);
                for mi in method_start..method_end {
                    let Some(md) = self.metadata.method_defs.get(mi) else {
                        continue;
                    };
                    if (md.flags as u32 & crate::il2cpp::enums::method_attributes::ABSTRACT) != 0 {
                        continue;
                    }
                    let ptr = self.il2cpp.get_method_pointer(&image_name, md);
                    if ptr == 0 {
                        continue;
                    }
                    let mrva = self.il2cpp.get_rva(ptr);
                    let mclean = if arch == Architecture::Arm32 {
                        mrva & !1
                    } else {
                        mrva
                    };
                    if mrva == rva || (target_clean != 0 && mclean == target_clean) {
                        return Some((td_idx, mi));
                    }
                }
            }
        }
        None
    }

    /// Formats a single `type_def_index` to readable C# text (fields/offsets, properties, methods/RVAs).
    pub fn format_type_cs(&mut self, type_def_index: usize, include_pseudo_c: bool) -> Result<String> {
        let mode = MethodBodyMode::from_flags(false, include_pseudo_c);
        self.format_type_with_mode(type_def_index, mode, &Il2CppDisasmOptions::default())
    }

    /// Formats a single `type_def_index` with configurable native disassembly and/or Pseudo-C bodies.
    pub fn format_type_with_mode(
        &mut self,
        type_def_index: usize,
        body_mode: MethodBodyMode,
        disasm_opts: &Il2CppDisasmOptions,
    ) -> Result<String> {
        let (image_index, image_name) = self.resolve_type_image(type_def_index);

        let mut cfg = self.config.clone();
        cfg.set_method_body_mode(body_mode);
        Self::apply_disasm_options(&mut cfg, disasm_opts);

        if cfg.dump_disassembly {
            let _ = self.ensure_disassembler()?;
        }
        let cached_disasm = if cfg.dump_disassembly {
            self.disasm_cache.as_ref().and_then(|opt| opt.as_ref())
        } else {
            None
        };

        Il2CppDecompiler::dump_type_to_string(
            &mut self.executor,
            &self.metadata,
            &self.il2cpp,
            &cfg,
            type_def_index,
            image_index,
            &image_name,
            cfg.dump_pseudo_c,
            cached_disasm,
        )
    }

    /// Formats a batch of `TypeDef` indices with Pseudo-C method bodies in a single recovery pass.
    pub fn format_types_pseudo_c(&mut self, type_def_indices: &[usize]) -> Result<Vec<(usize, String)>> {
        if type_def_indices.is_empty() {
            return Ok(Vec::new());
        }
        let mut cfg = self.config.clone();
        cfg.set_method_body_mode(MethodBodyMode::Csharp);
        let resolved: Vec<(usize, usize, String)> = type_def_indices
            .iter()
            .map(|&td| {
                let (img_idx, img_name) = self.resolve_type_image(td);
                (td, img_idx, img_name)
            })
            .collect();
        let target_refs: Vec<(usize, usize, &str)> = resolved
            .iter()
            .map(|(td, img_idx, img_name)| (*td, *img_idx, img_name.as_str()))
            .collect();
        Il2CppDecompiler::dump_types_to_strings(
            &mut self.executor,
            &self.metadata,
            &self.il2cpp,
            &cfg,
            &target_refs,
            cfg.dump_pseudo_c,
            None,
        )
    }

    /// Lists all methods declared on `type_def_index` with RVAs, VAs, file offsets, and byte bounds.
    pub fn list_type_methods(&mut self, type_def_index: usize) -> Result<Vec<Il2CppMethodEntry>> {
        let td = self
            .metadata
            .type_defs
            .get(type_def_index)
            .cloned()
            .ok_or_else(|| Error::Other(format!("TypeDef #{type_def_index} out of bounds")))?;
        let (_img_idx, image_name) = self.resolve_type_image(type_def_index);
        let sorted_addrs = self
            .disasm_cache
            .as_ref()
            .and_then(|o| o.as_ref())
            .map(|(_, addrs)| addrs.clone())
            .unwrap_or_else(|| self.il2cpp.build_sorted_method_addresses());
        let arch = self.il2cpp.detect_architecture();
        let method_end = td.method_start as usize + td.method_count as usize;
        let mut out = Vec::with_capacity(td.method_count.max(0) as usize);

        for mi in td.method_start as usize..method_end {
            let Some(md) = self.metadata.method_defs.get(mi).cloned() else {
                continue;
            };
            let name = self
                .metadata
                .get_string_from_index(md.name_index as i32)
                .unwrap_or_else(|_| format!("method_{mi}"));
            let is_abstract = (md.flags as u32 & crate::il2cpp::enums::method_attributes::ABSTRACT) != 0;
            let is_static = (md.flags as u32 & crate::il2cpp::enums::method_attributes::STATIC) != 0;
            let ptr = if is_abstract {
                0
            } else {
                self.il2cpp.get_method_pointer(&image_name, &md)
            };
            let rva = if ptr > 0 { self.il2cpp.get_rva(ptr) } else { 0 };
            let thumb = arch == Architecture::Arm32 && (rva & 1) != 0;
            let clean_rva = if arch == Architecture::Arm32 {
                rva & !1
            } else {
                rva
            };
            let offset = if ptr > 0 {
                let lookup_ptr = if arch == Architecture::Arm32 {
                    ptr & !1
                } else {
                    ptr
                };
                self.il2cpp.map_vatr(lookup_ptr).unwrap_or(clean_rva)
            } else {
                0
            };
            let body_size = if rva > 0 {
                self.il2cpp.get_method_body_size(rva, &sorted_addrs) + (rva - clean_rva) as usize
            } else {
                0
            };
            let signature = self.format_method_signature(&name, &md);
            out.push(Il2CppMethodEntry {
                method_def_index: mi,
                name,
                signature,
                rva,
                va: ptr,
                file_offset: offset,
                body_size,
                is_abstract,
                is_static,
                thumb,
            });
        }
        Ok(out)
    }

    fn format_method_signature(
        &mut self,
        name: &str,
        md: &crate::il2cpp::structures::Il2CppMethodDefinition,
    ) -> String {
        let ret_name = self
            .il2cpp
            .types
            .get(md.return_type as usize)
            .cloned()
            .map(|t| {
                self.executor
                    .get_type_name(&t, &self.metadata, &self.il2cpp, false, false)
            })
            .unwrap_or_else(|| "void".to_owned());

        let mut params = Vec::with_capacity(md.parameter_count.max(0) as usize);
        for pi in 0..md.parameter_count.max(0) as usize {
            let Some(pd) = self
                .metadata
                .parameter_defs
                .get(md.parameter_start as usize + pi)
                .cloned()
            else {
                continue;
            };
            let pname = self
                .metadata
                .get_string_from_index(pd.name_index)
                .unwrap_or_else(|_| format!("arg{pi}"));
            let ptype = self
                .il2cpp
                .types
                .get(pd.type_index as usize)
                .cloned()
                .map(|t| {
                    self.executor
                        .get_type_name(&t, &self.metadata, &self.il2cpp, false, false)
                })
                .unwrap_or_else(|| "object".to_owned());
            params.push(format!("{ptype} {pname}"));
        }
        format!("{ret_name} {name}({})", params.join(", "))
    }

    /// Disassembles a single method or arbitrary RVA using cached IL2CPP annotations and CFG.
    pub fn disassemble_rva(
        &mut self,
        rva: u64,
        type_def_hint: Option<usize>,
        method_def_hint: Option<usize>,
        options: &Il2CppDisasmOptions,
    ) -> Result<DisassembledMethodReport> {
        let arch = self.il2cpp.detect_architecture();
        if !arch.has_native_code() {
            return Err(Error::Other(format!(
                "{arch} contains bytecode rather than native machine instructions"
            )));
        }

        let _ = self.ensure_disassembler()?;
        let (disassembler, sorted_addrs) = self
            .disasm_cache
            .as_ref()
            .and_then(|o| o.as_ref())
            .ok_or_else(|| Error::Other(format!("Disassembler unavailable for {arch}")))?;

        let thumb = arch == Architecture::Arm32 && (rva & 1) != 0;
        let clean_rva = if arch == Architecture::Arm32 {
            rva & !1
        } else {
            rva
        };

        let body_size = if sorted_addrs.binary_search(&rva).is_ok() {
            self.il2cpp.get_method_body_size(rva, sorted_addrs) + (rva - clean_rva) as usize
        } else {
            let mask = if arch == Architecture::Arm32 { !1 } else { !0 };
            let next_idx = sorted_addrs.partition_point(|&a| (a & mask) <= clean_rva);
            if let Some(&next) = sorted_addrs.get(next_idx) {
                ((next & mask).saturating_sub(clean_rva) as usize).clamp(4, 0x10000)
            } else {
                0x4000
            }
        };

        let va = if self.il2cpp.image_base > 0 {
            self.il2cpp.image_base.wrapping_add(clean_rva)
        } else {
            clean_rva
        };
        let file_offset = self.il2cpp.map_vatr(va).unwrap_or(clean_rva);
        let bytes = self
            .il2cpp
            .read_bytes_at_rva(clean_rva, body_size)
            .ok_or_else(|| Error::Other(format!("RVA 0x{rva:X} is not mapped to readable bytes")))?;

        let is_arm = matches!(arch, Architecture::Arm64 | Architecture::Arm32);
        let is_arm64 = matches!(arch, Architecture::Arm64);

        let field_ctx = type_def_hint.and_then(|td_idx| {
            self.metadata.type_defs.get(td_idx).cloned().map(|td| {
                Il2CppDecompiler::build_type_disasm_context(
                    &mut self.executor,
                    &self.metadata,
                    &self.il2cpp,
                    &td,
                    td_idx,
                )
            })
        });
        let method_ctx = method_def_hint.and_then(|mi| {
            self.metadata.method_defs.get(mi).and_then(|md| {
                Il2CppDecompiler::build_method_disasm_ctx(
                    field_ctx.as_ref(),
                    md,
                    &self.metadata,
                    is_arm,
                    is_arm64,
                )
            })
        });
        let effective_ctx = method_ctx.or(field_ctx);

        let method_name = method_def_hint
            .and_then(|mi| self.metadata.method_defs.get(mi))
            .and_then(|md| {
                let mn = self
                    .metadata
                    .get_string_from_index(md.name_index as i32)
                    .ok()?;
                let tn = self
                    .metadata
                    .type_defs
                    .get(md.declaring_type as usize)
                    .and_then(|td| self.metadata.get_string_from_index(td.name_index).ok());
                Some(match tn {
                    Some(t) if !t.is_empty() => format!("{t}.{mn}"),
                    _ => mn,
                })
            })
            .or_else(|| {
                disassembler
                    .method_name_at(rva)
                    .or_else(|| disassembler.method_name_at(clean_rva))
                    .map(str::to_owned)
            });

        let max_insns = options.max_instructions.max(1);
        let (text, instruction_count, byte_size) = disassembler.format_method_body_with_stats(
            &bytes,
            rva,
            max_insns,
            "",
            effective_ctx.as_ref(),
            options.hex_bytes,
            options.field_names,
            options.annotations,
            options.cfg,
        );

        Ok(DisassembledMethodReport {
            rva,
            clean_rva,
            va,
            file_offset,
            arch: arch.to_string(),
            thumb,
            method_name,
            instruction_count,
            byte_size,
            text,
        })
    }

    /// Writes `dump.cs`, `DiffableCs/`, `script.json`, `il2cpp.h`, `stringliteral.json`, and `DummyDll/` into `out_dir`.
    pub fn write_dump_to_dir<L: FnMut(&str)>(
        &mut self,
        binary_label: &str,
        out_dir: &Path,
        opts: &Il2CppDumpWriteOptions,
        mut logger: L,
    ) -> Result<Vec<String>> {
        std::fs::create_dir_all(out_dir)?;
        let out_str = out_dir.to_string_lossy().to_string();

        let mut cfg = self.config.clone();
        cfg.split_dump_per_type = opts.write_diffable_cs;
        cfg.generate_dummy_dll = opts.write_dummy_dlls;
        cfg.generate_struct = opts.write_script_json_and_headers;
        cfg.generate_generics_dump = opts.write_generics_dump;
        cfg.dump_static_field_metadata = opts.write_static_metadata;
        cfg.dump_pseudo_c_include_assemblies = opts.include_assemblies.clone();
        cfg.dump_pseudo_c_include_types = opts.include_types.clone();
        cfg.dump_filter_headers = opts.filter_headers;
        cfg.dump_pseudo_c_full_dump = opts.full_dump;
        cfg.fold_ifix_wrappers = opts.fold_ifix_wrappers;
        if let Some(max_bytes) = opts.max_method_bytes {
            cfg.dump_pseudo_c_max_body_size =
                Self::resolve_pseudo_c_max_body_size(Some(max_bytes));
        }
        cfg.set_method_body_mode(opts.body_mode);
        Self::apply_disasm_options(&mut cfg, &opts.disasm);

        let mut generated = Vec::new();
        if opts.write_dump_cs || opts.write_diffable_cs {
            Il2CppDecompiler::decompile(
                &mut self.executor,
                &self.metadata,
                &self.il2cpp,
                &cfg,
                binary_label,
                &out_str,
                &mut logger,
            )?;
            if opts.write_dump_cs {
                generated.push("dump.cs".to_owned());
            }
            if opts.write_diffable_cs {
                generated.push("DiffableCs/".to_owned());
            }
        }

        let static_catalog = if cfg.dump_static_field_metadata {
            let cat = StaticFieldCatalog::collect(
                &mut self.executor,
                &mut self.metadata,
                &mut self.il2cpp,
                &cfg,
            )?;
            StaticFieldExporter::write_document(&cat, &self.il2cpp, &out_str)?;
            generated.push("static_metadata.json".to_owned());
            Some(cat)
        } else {
            None
        };

        if cfg.generate_struct {
            StructGenerator::write_all(
                &mut self.executor,
                &mut self.metadata,
                &mut self.il2cpp,
                &cfg,
                &out_str,
                static_catalog.as_ref(),
            )?;
            write_scripts(out_dir)?;
            generated.extend([
                "script.json".to_owned(),
                "stringliteral.json".to_owned(),
                "il2cpp.h".to_owned(),
            ]);
        }

        if cfg.generate_dummy_dll {
            generate_dummy_dlls(
                &mut self.executor,
                &mut self.metadata,
                &mut self.il2cpp,
                &cfg,
                &out_str,
            )?;
            generated.push("DummyDll/".to_owned());
        }

        if cfg.generate_generics_dump {
            let generics_path = out_dir.join("generics_dump.txt");
            let generics_str = generics_path.to_string_lossy();
            if dump_generics(
                &generics_str,
                &mut self.metadata,
                &mut self.il2cpp,
                &mut self.executor,
                &cfg,
            )
            .is_ok()
            {
                generated.push("generics_dump.txt".to_owned());
            }
        }

        Ok(generated)
    }
}

fn read_magic_u32(data: &[u8]) -> u32 {
    data.get(..4)
        .and_then(|s| s.try_into().ok())
        .map(u32::from_le_bytes)
        .unwrap_or(0)
}

fn read_magic_u16(data: &[u8]) -> u16 {
    data.get(..2)
        .and_then(|s| s.try_into().ok())
        .map(u16::from_le_bytes)
        .unwrap_or(0)
}

fn detect_format(data: &[u8]) -> &'static str {
    let magic32 = read_magic_u32(data);
    let magic16 = read_magic_u16(data);
    match magic32 {
        MAGIC_ELF => "elf",
        MAGIC_MACHO32 | MAGIC_MACHO64 => "macho",
        MAGIC_MACHOFAT => "macho_fat",
        MAGIC_NSO => "nso",
        MAGIC_WASM => "wasm",
        _ if magic16 == MAGIC_PE => "pe",
        _ => "unknown",
    }
}

fn is_valid_metadata_version(data: &[u8]) -> bool {
    if data.len() < 8 {
        return false;
    }
    let ver = i32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    ver > 0 && ver < 200
}

fn try_decrypt_metadata(data: &mut Vec<u8>) -> Option<String> {
    if data.len() < 16 {
        return None;
    }
    let magic = METADATA_MAGIC.to_le_bytes();

    let k1 = magic[0] ^ data[0];
    if k1 != 0 && (1..4).all(|i| (magic[i] ^ data[i]) == k1) {
        let mut test = data.clone();
        for b in &mut test {
            *b ^= k1;
        }
        if is_valid_metadata_version(&test) {
            *data = test;
            return Some(format!("Single-byte XOR (0x{k1:02X})"));
        }
    }

    let key4: [u8; 4] = std::array::from_fn(|i| magic[i] ^ data[i]);
    if key4 != [0u8; 4] && !key4.windows(2).all(|w| w[0] == w[1]) {
        let mut test = data.clone();
        for (i, b) in test.iter_mut().enumerate() {
            *b ^= key4[i % 4];
        }
        if is_valid_metadata_version(&test) {
            *data = test;
            return Some(format!(
                "4-byte XOR ({:02X}{:02X}{:02X}{:02X})",
                key4[0], key4[1], key4[2], key4[3]
            ));
        }
    }

    let header_size = 256usize.min(data.len());
    let mut test = data.clone();
    for (i, b) in test[..header_size].iter_mut().enumerate() {
        *b ^= key4[i % 4];
    }
    if test[..4] == magic && is_valid_metadata_version(&test) {
        *data = test;
        return Some(format!("Header-only XOR ({header_size}B)"));
    }

    None
}

fn detect_unity_version(data: &[u8]) -> Option<String> {
    let mut best: Option<String> = None;
    let mut i = 0;
    while i + 12 < data.len() {
        let looks_like = (data[i] == b'2'
            && data[i + 1] == b'0'
            && data[i + 2].is_ascii_digit()
            && data[i + 3].is_ascii_digit()
            && data[i + 4] == b'.'
            && data[i + 5].is_ascii_digit())
            || (data[i] == b'6'
                && data[i + 1] == b'0'
                && data[i + 2] == b'0'
                && data[i + 3] == b'0'
                && data[i + 4] == b'.'
                && data[i + 5].is_ascii_digit());
        if looks_like {
            let max_len = std::cmp::min(24, data.len() - i);
            let end = data[i..i + max_len]
                .iter()
                .position(|&b| !b.is_ascii_alphanumeric() && b != b'.')
                .unwrap_or(max_len);
            let cand = &data[i..i + end];
            if cand.len() >= 8
                && cand.len() <= 20
                && let Ok(s) = std::str::from_utf8(cand)
                && s.chars().filter(|c| *c == '.').count() == 2
                && (s.contains('f') || s.contains('b') || s.contains('a') || s.contains('p'))
                && s.ends_with(|c: char| c.is_ascii_digit())
                && best.as_ref().is_none_or(|prev| s > prev.as_str())
            {
                best = Some(s.to_owned());
            }
            i += end.max(1);
        } else {
            i += 1;
        }
    }
    best
}

fn resolve_codm(config: &Config, metadata: &Metadata) -> bool {
    config.codm || metadata.variant == MetadataVariant::Codm
}

fn init_elf(
    data: Vec<u8>,
    metadata: &Metadata,
    config: &Config,
    runtime_base: Option<u64>,
) -> Result<Il2Cpp> {
    let is_64 = data.len() > 4 && data[4] == 2;
    let mut elf = if metadata.variant == MetadataVariant::Codm {
        Elf::new_with_codm_diag(data, !is_64, true)?
    } else {
        Elf::new(data, !is_64)?
    };
    let version = if config.force_il2cpp_version {
        config.force_version
    } else {
        metadata.version
    };
    elf.set_properties(version, metadata.metadata_usages_count as u64);

    if let Some(base) = runtime_base.filter(|&b| b != 0)
        && config.force_dump
    {
        elf.stream.image_base = base;
        elf.is_dumped = true;
        if !config.no_redirected_pointer {
            elf.load()?;
        }
    }

    let method_count = metadata
        .method_defs
        .iter()
        .filter(|m| m.method_index >= 0)
        .count();
    let type_count = metadata.type_defs.len();
    let image_count = metadata.image_defs.len();

    let mut helper = elf.get_section_helper(method_count, type_count, image_count);
    let code_reg = helper.find_code_registration();
    let metadata_reg = if metadata.variant == MetadataVariant::Codm {
        helper
            .find_metadata_registration_codm()
            .or_else(|| helper.find_metadata_registration())
    } else {
        helper.find_metadata_registration()
    };

    let mut found = elf.auto_plus_init(code_reg, metadata_reg).unwrap_or(false);
    if !found
        && let Ok(Some((cr, mr))) = elf.symbol_search()
        && elf.init_with_auto_plus(cr, mr).is_ok()
    {
        found = true;
    }
    if !found
        && let Some((cr, mr)) = elf.search_arm32(version)
        && elf.init_with_auto_plus(cr, mr).is_ok()
    {
        found = true;
    }

    let elf_exports = elf.list_exported_symbols().unwrap_or_default();
    let mut il2cpp = Il2Cpp::from_elf(&elf);
    il2cpp.image_names = metadata
        .image_defs
        .iter()
        .filter_map(|image| metadata.get_string_from_index(image.name_index).ok())
        .collect();

    if let Some(base) = runtime_base.filter(|&b| b != 0) {
        if il2cpp.image_base == 0 {
            il2cpp.image_base = base;
        }
        il2cpp.is_dumped = true;
    }
    if il2cpp.image_base != 0 && il2cpp.code_gen_module_method_pointers.is_empty() {
        let names = il2cpp.image_names.clone();
        il2cpp.locate_code_gen_modules_in_dump(&names);
    }
    if !found && il2cpp.code_gen_module_method_pointers.is_empty() {
        return Err(Error::Other(
            "Could not locate CodeRegistration / MetadataRegistration in ELF binary".to_owned(),
        ));
    }

    il2cpp.codm = il2cpp.codm || resolve_codm(config, metadata);
    il2cpp.exported_symbols = elf_exports.iter().map(|(n, _)| n.clone()).collect();
    for (name, addr) in elf_exports {
        let rva = il2cpp.get_rva(addr);
        if name.starts_with("il2cpp_") || name.starts_with("mono_") {
            il2cpp.api_export_rvas.insert(name, rva);
        } else if name.starts_with("__cxa") || name.starts_with("___cxa") {
            il2cpp.cxa_func_rvas.insert(rva, name);
        }
    }
    for (slot_va, name) in &elf.bind_symbols {
        let rva = il2cpp.get_rva(*slot_va);
        if rva != 0 {
            il2cpp.bind_symbols.insert(rva, name.clone());
        }
    }
    Ok(il2cpp)
}

fn init_pe(data: Vec<u8>, metadata: &Metadata, config: &Config) -> Result<Il2Cpp> {
    let mut pe = Pe::new(data)?;
    let version = if config.force_il2cpp_version {
        config.force_version
    } else {
        metadata.version
    };
    pe.stream.version = version;
    pe.stream.is_32bit = pe.is_32bit;

    let method_count = metadata
        .method_defs
        .iter()
        .filter(|m| m.method_index >= 0)
        .count();
    let type_count = metadata.type_defs.len();
    let image_count = metadata.image_defs.len();
    let mu_count = metadata.metadata_usages_count;

    let (mut cr_addr, mut mr_addr) = pe.symbol_search().ok().flatten().unwrap_or((0, 0));
    if cr_addr == 0 || mr_addr == 0 {
        let mut helper =
            pe.get_section_helper(method_count, type_count, mu_count, image_count, version);
        if let (Some(cr), Some(mr)) = (
            helper.find_code_registration(),
            helper.find_metadata_registration(),
        ) {
            cr_addr = cr;
            mr_addr = mr;
        }
    }
    if cr_addr == 0 || mr_addr == 0 {
        return Err(Error::Other(
            "Could not locate CodeRegistration / MetadataRegistration in PE binary".to_owned(),
        ));
    }

    let pe_image_base = pe.image_base();
    let va_segments: Vec<VaSegment> = pe
        .sections
        .iter()
        .map(|s| VaSegment {
            vaddr: s.virtual_address as u64 + pe_image_base,
            memsz: s.virtual_size as u64,
            offset: s.pointer_to_raw_data as u64,
        })
        .collect();

    let mut il2cpp = Il2Cpp::new(pe.stream.clone(), version, pe.is_32bit);
    il2cpp.va_segments = va_segments;
    il2cpp.readonly_va_ranges = pe
        .sections
        .iter()
        .filter(|s| (s.characteristics & 0x8000_0000) == 0 && s.size_of_raw_data > 0)
        .map(|s| {
            let start = s.virtual_address as u64 + pe_image_base;
            let sz = (s.size_of_raw_data as u64).min(s.virtual_size as u64);
            (start, start.saturating_add(sz))
        })
        .collect();
    il2cpp.image_base = pe_image_base;
    il2cpp.is_pe = true;
    il2cpp.codm = resolve_codm(config, metadata);
    il2cpp.arch = Some(if pe.is_32bit {
        Architecture::X86
    } else {
        Architecture::X64
    });
    il2cpp.init_with_auto_plus(cr_addr, mr_addr, &|addr| pe.map_vatr(addr), false)?;
    il2cpp.data_sections = pe.data_search_sections();
    Ok(il2cpp)
}

fn init_macho_fat(data: Vec<u8>, metadata: &Metadata, config: &Config) -> Result<Il2Cpp> {
    let arches = parse_fat(&data)?;
    let chosen = arches
        .iter()
        .find(|a| a.magic == crate::formats::macho::MH_MAGIC_64)
        .or_else(|| arches.first())
        .ok_or_else(|| Error::InvalidFormat("Empty fat Mach-O".to_owned()))?;
    let slice = extract_fat_slice(&data, chosen)?;
    init_macho(slice, metadata, config)
}

fn init_macho(data: Vec<u8>, metadata: &Metadata, config: &Config) -> Result<Il2Cpp> {
    let is_64 = read_magic_u32(&data) == MAGIC_MACHO64;
    let mut macho = if metadata.variant == MetadataVariant::Codm {
        MachO::new_with_codm_fixups(data, !is_64, true)?
    } else {
        MachO::new(data, !is_64)?
    };
    let version = if config.force_il2cpp_version {
        config.force_version
    } else {
        metadata.version
    };
    macho.stream.version = version;
    macho.stream.is_32bit = macho.is_32bit;

    let method_count = metadata
        .method_defs
        .iter()
        .filter(|m| m.method_index >= 0)
        .count();
    let type_count = metadata.type_defs.len();
    let image_count = metadata.image_defs.len();
    let mu_count = metadata.metadata_usages_count;

    let (mut cr_addr, mut mr_addr) = macho
        .symbol_search()
        .or_else(|| macho.search_mod_init_func(version))
        .unwrap_or((0, 0));
    if cr_addr == 0 || mr_addr == 0 {
        let mut helper =
            macho.get_section_helper(method_count, type_count, mu_count, image_count, version);
        let code_reg = helper.find_code_registration();
        let metadata_reg = if metadata.variant == MetadataVariant::Codm {
            helper
                .find_metadata_registration_codm()
                .or_else(|| helper.find_metadata_registration())
        } else {
            helper.find_metadata_registration()
        };
        if let (Some(cr), Some(mr)) = (code_reg, metadata_reg) {
            cr_addr = cr;
            mr_addr = mr;
        }
    }
    if cr_addr == 0 || mr_addr == 0 {
        return Err(Error::Other(
            "Could not locate CodeRegistration / MetadataRegistration in Mach-O binary".to_owned(),
        ));
    }

    let va_segments: Vec<VaSegment> = macho
        .segments
        .iter()
        .map(|s| VaSegment {
            vaddr: s.vmaddr,
            memsz: s.vmsize,
            offset: s.fileoff,
        })
        .collect();
    let mut il2cpp = Il2Cpp::new(macho.stream.clone(), version, macho.is_32bit);
    il2cpp.va_segments = va_segments;
    il2cpp.image_base = macho.image_base();
    il2cpp.codm = resolve_codm(config, metadata);
    il2cpp.arch = Some(if macho.is_32bit {
        Architecture::Arm32
    } else {
        Architecture::Arm64
    });
    il2cpp.init_with_auto_plus(cr_addr, mr_addr, &|addr| macho.map_vatr(addr), false)?;
    il2cpp.data_sections = macho.data_search_sections();
    Ok(il2cpp)
}

fn init_nso(data: Vec<u8>, metadata: &Metadata, config: &Config) -> Result<Il2Cpp> {
    let mut nso = Nso::new(data)?;
    let version = if config.force_il2cpp_version {
        config.force_version
    } else {
        metadata.version
    };
    nso.stream.version = version;
    nso.stream.is_32bit = nso.is_32bit;

    let method_count = metadata
        .method_defs
        .iter()
        .filter(|m| m.method_index >= 0)
        .count();
    let type_count = metadata.type_defs.len();
    let image_count = metadata.image_defs.len();
    let mu_count = metadata.metadata_usages_count;

    let mut helper =
        nso.get_section_helper(method_count, type_count, mu_count, image_count, version);
    let (cr_addr, mr_addr) = helper
        .find_code_registration()
        .zip(helper.find_metadata_registration())
        .ok_or_else(|| {
            Error::Other(
                "Could not locate CodeRegistration / MetadataRegistration in NSO binary".to_owned(),
            )
        })?;

    let stream_len = nso.stream.data().len() as u64;
    let mut il2cpp = Il2Cpp::new(nso.stream.clone(), version, nso.is_32bit);
    il2cpp.va_segments = vec![VaSegment {
        vaddr: 0,
        memsz: stream_len,
        offset: 0,
    }];
    il2cpp.arch = Some(Architecture::from_bitness(nso.is_32bit, false));
    il2cpp.codm = resolve_codm(config, metadata);
    il2cpp.init_with_auto_plus(cr_addr, mr_addr, &|addr| nso.map_vatr(addr), false)?;
    il2cpp.data_sections = nso.data_search_sections();
    Ok(il2cpp)
}

fn init_wasm(data: Vec<u8>, metadata: &Metadata, config: &Config) -> Result<Il2Cpp> {
    let mut wasm = Wasm::new(data)?;
    let version = if config.force_il2cpp_version {
        config.force_version
    } else {
        metadata.version
    };
    wasm.stream.version = version;
    wasm.stream.is_32bit = wasm.is_32bit;

    let method_count = metadata
        .method_defs
        .iter()
        .filter(|m| m.method_index >= 0)
        .count();
    let type_count = metadata.type_defs.len();
    let image_count = metadata.image_defs.len();
    let mu_count = metadata.metadata_usages_count;

    let mut helper =
        wasm.get_section_helper(method_count, type_count, mu_count, image_count, version);
    let (cr_addr, mr_addr) = helper
        .find_code_registration()
        .zip(helper.find_metadata_registration())
        .ok_or_else(|| {
            Error::Other(
                "Could not locate CodeRegistration / MetadataRegistration in WASM binary"
                    .to_owned(),
            )
        })?;

    let stream_len = wasm.stream.data().len() as u64;
    let mut il2cpp = Il2Cpp::new(wasm.stream.clone(), version, wasm.is_32bit);
    il2cpp.va_segments = vec![VaSegment {
        vaddr: 0,
        memsz: stream_len,
        offset: 0,
    }];
    il2cpp.arch = Some(Architecture::Wasm);
    il2cpp.codm = resolve_codm(config, metadata);
    il2cpp.init_with_auto_plus(cr_addr, mr_addr, &|addr| wasm.map_vatr(addr), true)?;
    il2cpp.data_sections = wasm.data_search_sections();
    Ok(il2cpp)
}
