//! Clean-room Mono PE/CLI metadata (`Managed/*.dll`) & IL2CPP (`global-metadata.dat`)
//! inspector plus Unity `MonoScript` (`class_id == 115`) catalog correlation.

pub mod il2cpp;
pub mod mono_pe;
pub mod oc;

use devx_unity_io::{
    UnityWorkspaceData, extract_i32_field, extract_i64_field, read_object,
};
use il2cpp::{
    Il2CppDumpReport, Il2CppMetadataSummary, Il2CppRecoveredType, parse_global_metadata,
};
use mono_pe::{ManagedAssemblyInfo, parse_managed_pe};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScriptError {
    #[error("invalid PE / ECMA-335 image: {0}")]
    InvalidPe(String),

    #[error("binary is native PE without a .NET CLR metadata directory")]
    NotDotNet,

    #[error("invalid IL2CPP global-metadata.dat: {0}")]
    InvalidIl2Cpp(String),
}

pub type ScriptResult<T> = Result<T, ScriptError>;

/// Metadata extracted from a Unity `MonoScript` (`class_id == 115`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonoScriptInfo {
    pub file_index: usize,
    pub path_id: i64,
    pub class_name: String,
    pub namespace: String,
    pub assembly_name: String,
    pub execution_order: i32,
}

impl MonoScriptInfo {
    pub fn full_name(&self) -> String {
        if self.namespace.is_empty() {
            self.class_name.clone()
        } else {
            format!("{}.{}", self.namespace, self.class_name)
        }
    }
}

/// Native/WASM script binary summary (e.g. `GameAssembly.dll`, `libil2cpp.so`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeScriptBinaryInfo {
    pub virtual_path: String,
    pub byte_len: usize,
    pub kind: String,
}

/// Consolidated script index built from workspace `MonoScript` objects and `ScriptArtifact`s.
#[derive(Debug, Clone, Default)]
pub struct ScriptsIndex {
    pub mono_scripts: Vec<MonoScriptInfo>,
    pub mono_script_lookup: HashMap<(usize, i64), usize>,
    pub assemblies: Vec<ManagedAssemblyInfo>,
    pub il2cpp: Option<Il2CppMetadataSummary>,
    pub il2cpp_recovered: Option<Il2CppDumpReport>,
    pub il2cpp_error: Option<String>,
    pub native_binaries: Vec<NativeScriptBinaryInfo>,
}

impl ScriptsIndex {
    pub fn from_workspace(workspace: &UnityWorkspaceData) -> Self {
        let mut mono_scripts = Vec::new();
        let mut mono_script_lookup = HashMap::new();

        for (file_index, loaded) in workspace.files.iter().enumerate() {
            for info in &loaded.file.objects {
                if info.type_id != 115 {
                    continue;
                }
                let Ok(obj) = read_object(loaded, info) else {
                    continue;
                };
                let props = obj.class.properties();
                let class_name = props
                    .get("m_ClassName")
                    .and_then(|v| v.as_str())
                    .or_else(|| props.get("m_Name").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .to_owned();
                let namespace = props
                    .get("m_Namespace")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                let assembly_name = props
                    .get("m_AssemblyName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                let execution_order = props
                    .get("m_ExecutionOrder")
                    .and_then(extract_i32_field)
                    .unwrap_or(0);

                let idx = mono_scripts.len();
                mono_script_lookup.insert((file_index, info.path_id), idx);
                mono_scripts.push(MonoScriptInfo {
                    file_index,
                    path_id: info.path_id,
                    class_name,
                    namespace,
                    assembly_name,
                    execution_order,
                });
            }
        }

        let mut assemblies = Vec::new();
        let mut il2cpp = None;
        let mut native_binaries = Vec::new();
        let mut metadata_artifact: Option<&devx_unity_io::ScriptArtifact> = None;
        let mut il2cpp_bin_candidates: Vec<(&str, &devx_unity_io::ScriptArtifact)> = Vec::new();

        for art in &workspace.script_artifacts {
            let lower = art.virtual_path.to_ascii_lowercase();
            if lower.ends_with("global-metadata.dat") {
                metadata_artifact = Some(art);
                if let Ok(parsed) = parse_global_metadata(&art.virtual_path, &art.bytes, 2500) {
                    il2cpp = Some(parsed);
                }
            } else if lower.ends_with(".dll") {
                match parse_managed_pe(&art.virtual_path, &art.bytes) {
                    Ok(asm) => assemblies.push(asm),
                    Err(ScriptError::NotDotNet) => {
                        let is_il2cpp_pe = lower.ends_with("gameassembly.dll")
                            || lower.ends_with("userassembly.dll");
                        if is_il2cpp_pe {
                            il2cpp_bin_candidates.push((&art.virtual_path, art));
                        }
                        native_binaries.push(NativeScriptBinaryInfo {
                            virtual_path: art.virtual_path.clone(),
                            byte_len: art.bytes.len(),
                            kind: "Native PE/DLL".to_owned(),
                        });
                    }
                    Err(_) => {}
                }
            } else {
                let kind = if lower.ends_with(".so") {
                    if lower.contains("il2cpp")
                        || lower.contains("gameassembly")
                        || lower.contains("userassembly")
                    {
                        il2cpp_bin_candidates.push((&art.virtual_path, art));
                    }
                    "ELF Shared Object"
                } else if lower.ends_with(".wasm") {
                    il2cpp_bin_candidates.push((&art.virtual_path, art));
                    "WebAssembly"
                } else if lower.ends_with("unityframework") {
                    il2cpp_bin_candidates.push((&art.virtual_path, art));
                    "Mach-O Framework"
                } else {
                    "Script Artifact"
                };
                native_binaries.push(NativeScriptBinaryInfo {
                    virtual_path: art.virtual_path.clone(),
                    byte_len: art.bytes.len(),
                    kind: kind.to_owned(),
                });
            }
        }

        // Prefer 64-bit native binary (arm64-v8a / GameAssembly.dll) when multiple ABIs coexist in APKs.
        il2cpp_bin_candidates.sort_by_key(|(vpath, _)| {
            let l = vpath.to_ascii_lowercase();
            if l.contains("arm64-v8a") || l.ends_with("gameassembly.dll") || l.contains("x86_64") {
                0u8
            } else if l.contains("armeabi-v7a") || l.contains("x86") {
                1u8
            } else {
                2u8
            }
        });

        let mut il2cpp_recovered = None;
        let mut il2cpp_error = None;
        if let (Some(meta), Some((_, bin))) = (metadata_artifact, il2cpp_bin_candidates.first()) {
            match Il2CppDumpReport::recover_from_bytes(
                &bin.virtual_path,
                bin.bytes.to_vec(),
                &meta.virtual_path,
                meta.bytes.to_vec(),
            ) {
                Ok(report) => {
                    if il2cpp.is_none() {
                        il2cpp = Some(Il2CppMetadataSummary {
                            virtual_path: meta.virtual_path.clone(),
                            format_version: report.metadata_version as i32,
                            string_literal_count: report.string_literal_count,
                            string_table_bytes: meta.bytes.len(),
                            total_size_bytes: meta.bytes.len(),
                            string_literals_preview: Vec::new(),
                            symbol_strings_preview: Vec::new(),
                        });
                    }
                    il2cpp_recovered = Some(report);
                }
                Err(err) => {
                    il2cpp_error = Some(err.to_string());
                }
            }
        }

        Self {
            mono_scripts,
            mono_script_lookup,
            assemblies,
            il2cpp,
            il2cpp_recovered,
            il2cpp_error,
            native_binaries,
        }
    }

    /// Resolves which [`MonoScriptInfo`] drives a given `MonoBehaviour` object (via `m_Script` PPtr).
    pub fn resolve_behaviour_script<'a>(
        &'a self,
        workspace: &UnityWorkspaceData,
        behaviour_file_index: usize,
        behaviour_obj: &unity_asset_binary::object::UnityObject,
    ) -> Option<&'a MonoScriptInfo> {
        let script_pptr = behaviour_obj.class.properties().get("m_Script")?.as_object()?;
        let fid = script_pptr.get("m_FileID").and_then(extract_i32_field)?;
        let pid = script_pptr.get("m_PathID").and_then(extract_i64_field)?;
        if pid == 0 {
            return None;
        }
        let target_file = workspace.resolve_external_file(behaviour_file_index, fid)?;
        let idx = *self.mono_script_lookup.get(&(target_file, pid))?;
        self.mono_scripts.get(idx)
    }

    /// Looks up full recovered IL2CPP type information matching a [`MonoScriptInfo`].
    pub fn resolve_il2cpp_type(&self, script: &MonoScriptInfo) -> Option<&Il2CppRecoveredType> {
        self.il2cpp_recovered.as_ref()?.find_type(
            &script.assembly_name,
            &script.namespace,
            &script.class_name,
        )
    }

    /// Builds a C# stub string for a [`MonoScriptInfo`], backed by IL2CPP recovered fields when present.
    pub fn generate_csharp_stub_for_script(&self, script: &MonoScriptInfo) -> String {
        if let Some(ref rec) = self.il2cpp_recovered
            && let Some(stub) = rec.generate_unity_csharp_stub(
                &script.assembly_name,
                &script.namespace,
                &script.class_name,
            )
        {
            return stub;
        }
        let clean = script.class_name.split('`').next().unwrap_or(&script.class_name);
        let indent = if script.namespace.is_empty() {
            ""
        } else {
            "    "
        };
        let mut buf = format!("// Fallback MonoScript stub ({})\nusing UnityEngine;\n\n", script.assembly_name);
        if !script.namespace.is_empty() {
            buf.push_str(&format!("namespace {}\n{{\n", script.namespace));
        }
        buf.push_str(&format!(
            "{indent}public class {clean} : MonoBehaviour\n{indent}{{\n{indent}}}\n"
        ));
        if !script.namespace.is_empty() {
            buf.push_str("}\n");
        }
        buf
    }
}
