//! CLI tool for fast `.ab` bundle extraction, workspace inspection, batch unpacking
//! (including `Texture2D` ASTC/ETC/PVRTC/BCn), and ShaderLab / CG / SPIR-V / DXBC decompilation.

use devx_codecs::gltf_scene::GltfSceneOptions;
use devx_core::AssetCatalog;
use devx_exporter::bevy_export::{BevyExportOptions, export_bevy_project};
use devx_exporter::hierarchy::{
    HierarchyExportFormat, collect_loaded_animation_clips, derive_bundle_stem,
    discover_hierarchy_groups, export_all_hierarchies_with_format, export_hierarchy_with_format,
};
use devx_exporter::prefab::{
    PrefabExportOptions, export_prefab_to_dir, export_prefab_unitypackage,
};
use devx_exporter::unity_project::{UnityProjectExportOptions, export_unity_project};
use devx_exporter::{
    ExportAssetOptions, decompile_shaders_to_dir, export_batch_directory, extract_ab_bundles,
};
use devx_plugin_api::{AssetRef, InMemoryHostContext};
use devx_plugins_builtin::default_plugin_registry;
use devx_scripts::{
    ScriptsIndex,
    il2cpp::{
        Il2CppDisasmOptions, Il2CppDumpReport, Il2CppExportConfig, Il2CppMethodEntry,
        Il2CppRecoveredType, parse_rva_hex,
    },
};
use devx_unity_io::{WorkspaceLoadOptions, load_path_with_options, read_object, unity_value_to_json};
use std::env;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

fn print_usage() {
    eprintln!(
        "\
Usage:
  devx_cli inspect <input> [--filter <substr|a|b>]... [--dump <name_or_pathid>] [--list-bundles] [--limit <N>] [--with-deps] [--all-bundles]
  devx_cli extract-ab <input> <out_dir> [--filter <substr>]
  devx_cli unpack <input> <out_dir> [--class <ClassName>] [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--tex png|jpg|webp|bmp|tga|dds] [--mesh obj|fbx|glb|dae|ply|stl] [--limit <N>]
  devx_cli decompile-shaders <input> <out_dir> [--bundle-filter <substr>] [--all-bundles]
  devx_cli export-glb <input> <out.glb> [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--max-tex <px>]
  devx_cli export-fbx <input> <out.fbx> [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--max-tex <px>]
  devx_cli export-gltf <input> <out.gltf> [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--max-tex <px>]
  devx_cli export-dae <input> <out.dae> [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--max-tex <px>]
  devx_cli export-hierarchy <input> <out_dir> [--format glb|gltf|fbx|dae|obj] [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--limit <N>] [--max-tex <px>]
  devx_cli export-prefab <input> <out_dir_or_pkg> [--name <stem>] [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--unitypackage] [--no-companions]
  devx_cli export-scene <input> <out_dir_or_pkg> [--name <stem>] [--bundle-filter <substr>] [--with-deps] [--all-bundles] [--unitypackage] [--no-companions]
  devx_cli export-bevy <input> <out_dir> [--name <pkg>] [--scene <substr>]... [--char <substr>]... [--prop <substr>]... [--anim <substr>]... [--audio <substr>]... [--ui-sprite <substr>]... [--ui-panel <substr>]... [--max-scenes <N>] [--max-chars <N>] [--max-props <N>] [--max-audio <N>] [--max-fonts <N>] [--max-ui-sprites <N>] [--max-ui-panels <N>] [--max-tex <px>] [--assets-only]
  devx_cli export-unity-project <input> <out_dir> [--bundle-filter <substr>] [--all-bundles] [--max-scenes <N>] [--max-prefabs <N>] [--no-scripts]
  devx_cli dump-il2cpp <input> <out_dir> [--disasm] [--pseudo-c] [--fold-ifix] [--full-dump] [--body-mode <off|assembly|csharp|both>] [--max-method-bytes <N>] [--dummy-dll] [--max-insns <N>] [--no-hex] [--no-cfg] [--no-annotations] [--no-field-names] [--filter-asm <name>]... [--filter-type <name>]...
  devx_cli disasm-il2cpp <input> (--rva <0xHEX> | --type <TypeName>... | --search <substr>) [--method <MethodName>] [--asm <Assembly>] [--all] [--limit <N>] [--disasm] [--pseudo-c] [--fold-ifix] [--body-mode <off|assembly|csharp|both>] [--max-method-bytes <N>] [--max-insns <N>] [--no-hex] [--no-cfg] [--no-annotations] [--no-field-names]
  devx_cli decode-oc <input> <out_dir> [--filter <substr>]... [--raw <substr>]... [--compact] [--summary] [--strict]"
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        print_usage();
        return ExitCode::SUCCESS;
    }

    let res = match args[0].as_str() {
        "inspect" => cmd_inspect(&args[1..]),
        "extract-ab" => cmd_extract_ab(&args[1..]),
        "unpack" => cmd_unpack(&args[1..]),
        "decompile-shaders" => cmd_decompile_shaders(&args[1..]),
        "export-glb" => cmd_export_single_hierarchy(&args[1..], HierarchyExportFormat::Glb),
        "export-fbx" => cmd_export_single_hierarchy(&args[1..], HierarchyExportFormat::Fbx),
        "export-gltf" => cmd_export_single_hierarchy(&args[1..], HierarchyExportFormat::Gltf),
        "export-dae" => cmd_export_single_hierarchy(&args[1..], HierarchyExportFormat::Dae),
        "export-hierarchy" => cmd_export_hierarchy(&args[1..]),
        "export-prefab" => cmd_export_prefab_or_scene(&args[1..], false),
        "export-scene" => cmd_export_prefab_or_scene(&args[1..], true),
        "export-bevy" => cmd_export_bevy(&args[1..]),
        "export-unity-project" => cmd_export_unity_project(&args[1..]),
        "dump-il2cpp" => cmd_dump_il2cpp(&args[1..]),
        "disasm-il2cpp" => cmd_disasm_il2cpp(&args[1..]),
        "decode-oc" => cmd_decode_oc(&args[1..]),
        other => Err(format!("unknown subcommand `{other}`")),
    };

    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn flag_value(args: &[String], flag: &str) -> Result<Option<String>, String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            let val = args
                .get(i + 1)
                .ok_or_else(|| format!("missing value for `{flag}`"))?;
            return Ok(Some(val.clone()));
        }
        i += 1;
    }
    Ok(None)
}

fn flag_values(args: &[String], flag: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            let val = args
                .get(i + 1)
                .ok_or_else(|| format!("missing value for `{flag}`"))?;
            out.push(val.clone());
            i += 2;
            continue;
        }
        i += 1;
    }
    Ok(out)
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn positional_args(args: &[String], value_flags: &[&str], bool_flags: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if value_flags.contains(&a) {
            i += 2;
            continue;
        }
        if bool_flags.contains(&a) {
            i += 1;
            continue;
        }
        out.push(args[i].clone());
        i += 1;
    }
    out
}

fn matches_pipe_filter(haystack: &str, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }
    let low = haystack.to_ascii_lowercase();
    filters.iter().any(|f| {
        f.split('|')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .any(|alt| low.contains(&alt))
    })
}

fn expand_pipe_filters(filters: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for f in filters {
        for part in f.split('|') {
            let trimmed = part.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_owned());
            }
        }
    }
    out
}

fn cmd_inspect(args: &[String]) -> Result<(), String> {
    let pos = positional_args(
        args,
        &["--filter", "--dump", "--limit"],
        &["--all-bundles", "--with-deps", "--list-bundles"],
    );
    let input = pos.first().ok_or("inspect requires <input>")?;
    let filters = flag_values(args, "--filter")?;
    let dump_query = flag_value(args, "--dump")?;
    let all_bundles = has_flag(args, "--all-bundles");
    let with_deps = has_flag(args, "--with-deps");
    let list_bundles = has_flag(args, "--list-bundles");
    let list_limit = match flag_value(args, "--limit")? {
        Some(raw) => raw
            .parse::<usize>()
            .map_err(|_| format!("invalid --limit value `{raw}`"))?
            .max(1),
        None => usize::MAX,
    };

    let t0 = Instant::now();
    let mut ws = load_path_with_options(input, WorkspaceLoadOptions::default())
        .map_err(|e| format!("failed to open `{input}`: {e}"))?;

    if list_bundles && !all_bundles && !with_deps && dump_query.is_none() {
        let mut matching_deferred: Vec<_> = ws
            .deferred_bundles
            .iter()
            .filter(|d| matches_pipe_filter(&d.virtual_path, &filters))
            .collect();
        matching_deferred.sort_by(|a, b| a.virtual_path.cmp(&b.virtual_path));
        let matching_raw: Vec<_> = ws
            .raw_artifacts
            .iter()
            .filter(|r| matches_pipe_filter(&r.virtual_path, &filters))
            .collect();
        println!(
            "Opened `{input}` in {:.2?} | deferred bundles matching: {} (of {}) | raw artifacts matching: {} (of {})",
            t0.elapsed(),
            matching_deferred.len(),
            ws.deferred_bundles.len(),
            matching_raw.len(),
            ws.raw_artifacts.len(),
        );
        for d in matching_deferred.iter().take(list_limit) {
            println!(
                "  [ab] {} (compressed: {} B, uncompressed: {} B, loaded: {})",
                d.virtual_path, d.compressed_size, d.uncompressed_size, d.loaded
            );
        }
        if matching_deferred.len() > list_limit {
            println!("  ... {} more deferred bundle(s) omitted (pass larger --limit)", matching_deferred.len() - list_limit);
        }
        for r in matching_raw.iter().take(list_limit) {
            println!("  [raw] {} ({} B)", r.virtual_path, r.bytes.len());
        }
        return Ok(());
    }

    let expanded = expand_pipe_filters(&filters);
    if all_bundles {
        if expanded.is_empty() {
            let loaded = ws.load_deferred_bundles_filtered(None).map_err(|e| e.to_string())?;
            println!("Loaded {loaded} deferred bundle(s)");
        } else {
            let mut total_loaded = 0usize;
            for f in &expanded {
                total_loaded += ws
                    .load_deferred_bundles_filtered(Some(f))
                    .map_err(|e| e.to_string())?;
            }
            println!("Loaded {total_loaded} deferred bundle(s)");
        }
    } else if !expanded.is_empty() && !ws.deferred_bundles.is_empty() {
        if with_deps {
            let mut primary_sum = 0usize;
            let mut deps_sum = 0usize;
            for f in &expanded {
                let (primary, rep) = ws
                    .load_deferred_bundles_with_deps(Some(f))
                    .map_err(|e| e.to_string())?;
                primary_sum += primary;
                deps_sum += rep.newly_loaded_bundles;
            }
            println!(
                "Loaded {primary_sum} bundle(s) matching {:?} + {deps_sum} dependency bundle(s)",
                filters
            );
        } else {
            let mut total_loaded = 0usize;
            for f in &expanded {
                total_loaded += ws
                    .load_deferred_bundles_filtered(Some(f))
                    .map_err(|e| e.to_string())?;
            }
            println!("Loaded {total_loaded} deferred bundle(s) matching {:?}", filters);
        }
    }

    let catalog = AssetCatalog::from_workspace(Arc::new(ws));
    let deferred_count = catalog.workspace.deferred_bundles.len();
    let unloaded_deferred: Vec<_> = catalog
        .workspace
        .deferred_bundles
        .iter()
        .filter(|d| !d.loaded)
        .filter(|d| matches_pipe_filter(&d.virtual_path, &filters))
        .collect();
    println!("Opened `{input}` in {:.2?}", t0.elapsed());
    println!(
        "Loaded SerializedFiles: {} | Streams: {} | Scripts: {} | Indexed deferred .ab: {} (unloaded matching: {})",
        catalog.workspace.files.len(),
        catalog.workspace.streams.len(),
        catalog.workspace.script_artifacts.len(),
        deferred_count,
        unloaded_deferred.len(),
    );
    println!("Total indexed objects: {}", catalog.entries.len());
    for ((cid, cname), count) in &catalog.class_counts {
        println!("  class {cid:>4} {cname:<22} : {count}");
    }
    if !unloaded_deferred.is_empty() {
        let cap = if list_bundles { list_limit } else { 15.min(list_limit) };
        println!("Deferred bundles matching (up to {cap}):");
        for d in unloaded_deferred.iter().take(cap) {
            println!(
                "  - {} ({} bytes uncompressed)",
                d.virtual_path, d.uncompressed_size
            );
        }
    }
    if let Some(ref q) = dump_query {
        let q_lower = q.to_ascii_lowercase();
        let mut shown = 0usize;
        for entry in &catalog.entries {
            let pid_str = entry.asset_ref.path_id.to_string();
            let matches = pid_str == *q
                || entry.summary.name.to_ascii_lowercase().contains(&q_lower)
                || entry.summary.class_name.eq_ignore_ascii_case(q);
            if !matches {
                continue;
            }
            if let Some((loaded, info)) = catalog
                .workspace
                .object_info(entry.asset_ref.file_index, entry.asset_ref.path_id)
                && let Ok(obj) = read_object(loaded, info)
            {
                let json = unity_value_to_json(
                    &unity_asset_core::UnityValue::Object(obj.class.properties().clone()),
                    Some(256),
                );
                println!(
                    "\n=== {} ({}) #{}:{} ===",
                    entry.summary.name,
                    entry.summary.class_name,
                    entry.asset_ref.file_index,
                    entry.asset_ref.path_id
                );
                if let Ok(pretty) = serde_json::to_string_pretty(&json) {
                    println!("{pretty}");
                }
                shown += 1;
                if shown >= 5 {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn cmd_extract_ab(args: &[String]) -> Result<(), String> {
    let pos = positional_args(args, &["--filter"], &[]);
    let input = pos.first().ok_or("extract-ab requires <input> <out_dir>")?;
    let out_dir = pos.get(1).ok_or("extract-ab requires <out_dir>")?;
    let filter = flag_value(args, "--filter")?;

    let t0 = Instant::now();
    let report = extract_ab_bundles(Path::new(input), Path::new(out_dir), filter.as_deref())
        .map_err(|e| e.to_string())?;
    let secs = t0.elapsed().as_secs_f64().max(0.0001);
    let mb = report.total_bytes as f64 / (1024.0 * 1024.0);
    println!(
        "Extracted {} bundle(s) ({:.2} MiB) to `{out_dir}` in {:.2?} ({:.1} MiB/s)",
        report.extracted_files,
        mb,
        t0.elapsed(),
        mb / secs
    );
    for p in report.output_paths.iter().take(10) {
        println!("  -> {}", p.display());
    }
    Ok(())
}

fn cmd_unpack(args: &[String]) -> Result<(), String> {
    let pos = positional_args(
        args,
        &["--class", "--bundle-filter", "--tex", "--mesh", "--limit"],
        &["--all-bundles", "--with-deps"],
    );
    let input = pos.first().ok_or("unpack requires <input> <out_dir>")?;
    let out_dir = pos.get(1).ok_or("unpack requires <out_dir>")?;
    let class_filter = flag_value(args, "--class")?;
    let bundle_filter = flag_value(args, "--bundle-filter")?;
    let tex_fmt = flag_value(args, "--tex")?.unwrap_or_else(|| "png".to_owned());
    let mesh_fmt = flag_value(args, "--mesh")?.unwrap_or_else(|| "obj".to_owned());
    let limit = flag_value(args, "--limit")?
        .map(|s| s.parse::<usize>().map_err(|e| e.to_string()))
        .transpose()?;
    let all_bundles = has_flag(args, "--all-bundles");
    let with_deps = has_flag(args, "--with-deps");

    let mut ws = load_path_with_options(input, WorkspaceLoadOptions::default())
        .map_err(|e| e.to_string())?;
    if !ws.deferred_bundles.is_empty() {
        if all_bundles {
            ws.load_deferred_bundles_filtered(None)
                .map_err(|e| e.to_string())?;
        } else if let Some(ref bf) = bundle_filter {
            if with_deps {
                let (n, rep) = ws
                    .load_deferred_bundles_with_deps(Some(bf))
                    .map_err(|e| e.to_string())?;
                println!(
                    "Loaded {n} deferred bundle(s) matching `{bf}` (+ {} dependency bundles)",
                    rep.newly_loaded_bundles
                );
            } else {
                let n = ws
                    .load_deferred_bundles_filtered(Some(bf))
                    .map_err(|e| e.to_string())?;
                println!("Loaded {n} deferred bundle(s) matching `{bf}`");
            }
        }
    }

    let catalog = AssetCatalog::from_workspace(Arc::new(ws));
    let refs: Vec<AssetRef> = catalog
        .entries
        .iter()
        .filter(|e| {
            class_filter
                .as_deref()
                .is_none_or(|c| e.summary.class_name.eq_ignore_ascii_case(c))
        })
        .take(limit.unwrap_or(usize::MAX))
        .map(|e| e.asset_ref)
        .collect();

    let opts = ExportAssetOptions {
        texture_exporter_id: match tex_fmt.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => "builtin.jpg".to_owned(),
            "webp" => "builtin.webp".to_owned(),
            "bmp" => "builtin.bmp".to_owned(),
            "dds" => "builtin.dds".to_owned(),
            "tga" => "builtin.tga".to_owned(),
            _ => "builtin.png".to_owned(),
        },
        mesh_exporter_id: match mesh_fmt.to_ascii_lowercase().as_str() {
            "fbx" => "builtin.fbx".to_owned(),
            "glb" => "builtin.glb".to_owned(),
            "dae" | "collada" => "builtin.dae".to_owned(),
            "ply" => "builtin.ply".to_owned(),
            "stl" => "builtin.stl".to_owned(),
            _ => "builtin.obj".to_owned(),
        },
        ..Default::default()
    };

    let registry = default_plugin_registry();
    let mut host = InMemoryHostContext::with_assets(
        catalog.entries.iter().map(|e| e.summary.clone()).collect(),
    );
    let t0 = Instant::now();
    let report = export_batch_directory(
        &catalog,
        &refs,
        Path::new(out_dir),
        &registry,
        &mut host,
        &opts,
        |_, _| {},
    )
    .map_err(|e| e.to_string())?;

    println!(
        "Exported {} asset(s), skipped {} (errors: {}) in {:.2?}",
        report.exported_files.len(),
        report.skipped,
        report.errors.len(),
        t0.elapsed()
    );
    for p in report.exported_files.iter().take(12) {
        println!("  -> {}", p.display());
    }
    for err in report.errors.iter().take(8) {
        eprintln!("  skip: {err}");
    }
    Ok(())
}

fn cmd_decompile_shaders(args: &[String]) -> Result<(), String> {
    let pos = positional_args(args, &["--bundle-filter"], &["--all-bundles"]);
    let input = pos
        .first()
        .ok_or("decompile-shaders requires <input> <out_dir>")?;
    let out_dir = pos
        .get(1)
        .ok_or("decompile-shaders requires <out_dir>")?;
    let bundle_filter = flag_value(args, "--bundle-filter")?;
    let all_bundles = has_flag(args, "--all-bundles");

    let mut ws = load_path_with_options(input, WorkspaceLoadOptions::default())
        .map_err(|e| e.to_string())?;
    if !ws.deferred_bundles.is_empty() {
        if all_bundles {
            ws.load_deferred_bundles_filtered(None)
                .map_err(|e| e.to_string())?;
        } else if let Some(ref bf) = bundle_filter {
            ws.load_deferred_bundles_filtered(Some(bf))
                .map_err(|e| e.to_string())?;
        } else {
            let shader_bundles = ws
                .deferred_bundles
                .iter()
                .filter(|d| d.virtual_path.to_ascii_lowercase().contains("shader"))
                .count();
            if shader_bundles > 0 {
                let n = ws
                    .load_deferred_bundles_filtered(Some("shader"))
                    .map_err(|e| e.to_string())?;
                println!("Auto-loaded {n} deferred shader bundle(s)");
            }
        }
    }

    let catalog = AssetCatalog::from_workspace(Arc::new(ws));
    let t0 = Instant::now();
    let (written, subprograms, errors) =
        decompile_shaders_to_dir(&catalog, Path::new(out_dir)).map_err(|e| e.to_string())?;
    println!(
        "Decompiled {written} Shader asset(s) ({subprograms} subprogram(s), {} error(s)) to `{out_dir}` in {:.2?}",
        errors.len(),
        t0.elapsed()
    );
    for err in errors.iter().take(8) {
        eprintln!("  err: {err}");
    }
    Ok(())
}

fn load_workspace_for_hierarchy(
    input: &str,
    bundle_filter: Option<&str>,
    all_bundles: bool,
    with_deps: bool,
) -> Result<devx_unity_io::UnityWorkspaceData, String> {
    let mut ws = load_path_with_options(input, WorkspaceLoadOptions::default())
        .map_err(|e| e.to_string())?;
    if !ws.deferred_bundles.is_empty() {
        if all_bundles {
            ws.load_deferred_bundles_filtered(None)
                .map_err(|e| e.to_string())?;
        } else if let Some(bf) = bundle_filter {
            if with_deps {
                let (n, rep) = ws
                    .load_deferred_bundles_with_deps(Some(bf))
                    .map_err(|e| e.to_string())?;
                println!(
                    "Loaded {n} bundle(s) matching `{bf}` + {} transitive dependency bundle(s)",
                    rep.newly_loaded_bundles
                );
                if !rep.unresolved_dependencies.is_empty() {
                    eprintln!(
                        "  note: {} unresolved dep(s): {:?}",
                        rep.unresolved_dependencies.len(),
                        &rep.unresolved_dependencies[..rep.unresolved_dependencies.len().min(5)]
                    );
                }
            } else {
                let n = ws
                    .load_deferred_bundles_filtered(Some(bf))
                    .map_err(|e| e.to_string())?;
                println!("Loaded {n} bundle(s) matching `{bf}`");
            }
        }
    }
    Ok(ws)
}

fn cmd_export_single_hierarchy(args: &[String], format: HierarchyExportFormat) -> Result<(), String> {
    let pos = positional_args(
        args,
        &["--bundle-filter", "--max-tex"],
        &["--all-bundles", "--with-deps"],
    );
    let ext = format.extension();
    let input = pos
        .first()
        .ok_or_else(|| format!("export-{ext} requires <input> <out.{ext}>"))?;
    let out_file = pos
        .get(1)
        .ok_or_else(|| format!("export-{ext} requires <out.{ext}>"))?;
    let bundle_filter = flag_value(args, "--bundle-filter")?;
    let max_tex = flag_value(args, "--max-tex")?
        .map(|s| s.parse::<u32>().map_err(|e| e.to_string()))
        .transpose()?;
    let all_bundles = has_flag(args, "--all-bundles");
    let with_deps = has_flag(args, "--with-deps") || !all_bundles;

    let t0 = Instant::now();
    let ws = load_workspace_for_hierarchy(
        input,
        bundle_filter.as_deref(),
        all_bundles,
        with_deps,
    )?;

    let groups = discover_hierarchy_groups(&ws, bundle_filter.as_deref());
    if groups.is_empty() {
        return Err(
            "no GameObject/Transform/Mesh hierarchy found matching selection (try --bundle-filter <substr>)"
                .to_owned(),
        );
    }

    let roots: Vec<AssetRef> = groups.into_iter().flat_map(|g| g.roots).collect();
    let extra_clips = collect_loaded_animation_clips(&ws);
    let opts = GltfSceneOptions {
        max_texture_dim: max_tex,
        ..Default::default()
    };

    let rep = export_hierarchy_with_format(
        &ws,
        &roots,
        &extra_clips,
        Path::new(out_file),
        format,
        &opts,
    )
    .map_err(|e| e.to_string())?;
    let s = &rep.summary;
    println!(
        "Exported {} `{}` in {:.2?}: {} nodes, {} meshes, {} tris, {} materials, {} textures, {} skins, {} animations",
        format.label(),
        rep.output_path.display(),
        t0.elapsed(),
        s.node_count,
        s.mesh_count,
        s.triangle_count,
        s.material_count,
        s.texture_count,
        s.skin_count,
        s.animation_count,
    );
    if !s.bone_names.is_empty() {
        println!(
            "  bones ({}): {:?}",
            s.bone_names.len(),
            &s.bone_names[..s.bone_names.len().min(10)]
        );
    }
    if !s.animation_names.is_empty() {
        println!("  animations: {:?}", s.animation_names);
    }
    Ok(())
}

fn cmd_export_hierarchy(args: &[String]) -> Result<(), String> {
    let pos = positional_args(
        args,
        &["--format", "--bundle-filter", "--limit", "--max-tex"],
        &["--all-bundles", "--with-deps"],
    );
    let input = pos
        .first()
        .ok_or("export-hierarchy requires <input> <out_dir>")?;
    let out_dir = pos.get(1).ok_or("export-hierarchy requires <out_dir>")?;
    let format_str = flag_value(args, "--format")?.unwrap_or_else(|| "glb".to_owned());
    let format = HierarchyExportFormat::parse(&format_str)
        .ok_or_else(|| format!("unknown hierarchy format `{format_str}` (expected glb|gltf|fbx|dae|obj)"))?;
    let bundle_filter = flag_value(args, "--bundle-filter")?;
    let limit = flag_value(args, "--limit")?
        .map(|s| s.parse::<usize>().map_err(|e| e.to_string()))
        .transpose()?;
    let max_tex = flag_value(args, "--max-tex")?
        .map(|s| s.parse::<u32>().map_err(|e| e.to_string()))
        .transpose()?;
    let all_bundles = has_flag(args, "--all-bundles");
    let with_deps = has_flag(args, "--with-deps") || !all_bundles;

    let t0 = Instant::now();
    let ws = load_workspace_for_hierarchy(
        input,
        bundle_filter.as_deref(),
        all_bundles,
        with_deps,
    )?;

    let opts = GltfSceneOptions {
        max_texture_dim: max_tex,
        ..Default::default()
    };
    let reports = export_all_hierarchies_with_format(
        &ws,
        Path::new(out_dir),
        format,
        bundle_filter.as_deref(),
        limit,
        true,
        &opts,
    )
    .map_err(|e| e.to_string())?;
    println!(
        "Exported {} hierarchy {} target(s) to `{out_dir}` in {:.2?}",
        reports.len(),
        format.label(),
        t0.elapsed()
    );
    for r in reports.iter().take(15) {
        println!(
            "  -> {} ({} tris, {} mats, {} skins, {} anims)",
            r.output_path.display(),
            r.summary.triangle_count,
            r.summary.material_count,
            r.summary.skin_count,
            r.summary.animation_count
        );
    }
    Ok(())
}

fn cmd_export_prefab_or_scene(args: &[String], is_scene: bool) -> Result<(), String> {
    let subcmd = if is_scene { "export-scene" } else { "export-prefab" };
    let pos = positional_args(
        args,
        &["--name", "--bundle-filter"],
        &["--all-bundles", "--with-deps", "--unitypackage", "--no-companions"],
    );
    let input = pos
        .first()
        .ok_or_else(|| format!("{subcmd} requires <input> <out_dir_or_pkg>"))?;
    let out_target = pos
        .get(1)
        .ok_or_else(|| format!("{subcmd} requires <out_dir_or_pkg>"))?;
    let custom_name = flag_value(args, "--name")?;
    let bundle_filter = flag_value(args, "--bundle-filter")?;
    let all_bundles = has_flag(args, "--all-bundles");
    let with_deps = has_flag(args, "--with-deps") || !all_bundles;
    let as_unitypackage = has_flag(args, "--unitypackage") || out_target.ends_with(".unitypackage");
    let include_companions = !has_flag(args, "--no-companions");

    let t0 = Instant::now();
    let ws = load_workspace_for_hierarchy(
        input,
        bundle_filter.as_deref(),
        all_bundles,
        with_deps,
    )?;

    let groups = discover_hierarchy_groups(&ws, bundle_filter.as_deref());
    let mut roots: Vec<AssetRef> = groups.iter().flat_map(|g| g.roots.iter().copied()).collect();
    if roots.is_empty() {
        // Fallback: look for root Transform/RectTransform objects even without Meshes (UI/logic prefabs)
        for (fi, f) in ws.files.iter().enumerate() {
            for info in &f.file.objects {
                if matches!(info.type_id, 4 | 224)
                    && let Ok(obj) = read_object(f, info)
                    && obj
                        .class
                        .properties()
                        .get("m_Father")
                        .and_then(|v| v.as_object())
                        .and_then(|m| m.get("m_PathID"))
                        .and_then(devx_unity_io::extract_i64_field)
                        .unwrap_or(0)
                        == 0
                {
                    roots.push(AssetRef {
                        file_index: fi,
                        path_id: info.path_id,
                    });
                }
            }
        }
    }
    if roots.is_empty() {
        return Err("no root GameObject/Transform hierarchy found for prefab/scene export".to_owned());
    }

    let stem = custom_name.unwrap_or_else(|| {
        groups
            .first()
            .map(|g| g.stem.clone())
            .unwrap_or_else(|| {
                let src = roots
                    .first()
                    .and_then(|r| ws.get_file(r.file_index))
                    .map(|f| f.virtual_path.as_str())
                    .unwrap_or("Exported");
                derive_bundle_stem(src, 0)
            })
    });

    let opts = PrefabExportOptions {
        is_scene,
        include_companion_assets: include_companions,
        ..Default::default()
    };
    let out_path = Path::new(out_target);
    let rep = if as_unitypackage {
        let pkg_path = if out_path.extension().is_some() {
            out_path.to_path_buf()
        } else {
            out_path.with_extension("unitypackage")
        };
        let b = export_prefab_unitypackage(&ws, &roots, &stem, &pkg_path, &opts)
            .map_err(|e| e.to_string())?;
        println!(
            "Exported `.unitypackage` `{}` ({} YAML objects, {} GameObjects, {} materials, {} textures, {} meshes) in {:.2?}",
            pkg_path.display(),
            b.object_count,
            b.game_object_count,
            b.material_count,
            b.texture_count,
            b.mesh_count,
            t0.elapsed(),
        );
        b
    } else {
        let b = export_prefab_to_dir(&ws, &roots, &stem, out_path, &opts)
            .map_err(|e| e.to_string())?;
        println!(
            "Exported `{}` to `{}` ({} YAML objects, {} GameObjects, {} materials, {} textures, {} meshes) in {:.2?}",
            b.root_file_name,
            out_path.display(),
            b.object_count,
            b.game_object_count,
            b.material_count,
            b.texture_count,
            b.mesh_count,
            t0.elapsed(),
        );
        b
    };
    let _ = rep;
    Ok(())
}

fn cmd_export_bevy(args: &[String]) -> Result<(), String> {
    let pos = positional_args(
        args,
        &[
            "--name",
            "--scene",
            "--char",
            "--prop",
            "--anim",
            "--audio",
            "--ui-sprite",
            "--ui-panel",
            "--max-scenes",
            "--max-chars",
            "--max-props",
            "--max-audio",
            "--max-fonts",
            "--max-ui-sprites",
            "--max-ui-panels",
            "--max-tex",
        ],
        &["--assets-only"],
    );
    let input = pos.first().ok_or("export-bevy requires <input> <out_dir>")?;
    let out_dir = pos.get(1).ok_or("export-bevy requires <out_dir>")?;

    let mut opts = BevyExportOptions::default();
    if let Some(name) = flag_value(args, "--name")? {
        opts.project_name = name;
    }
    opts.scene_filters = flag_values(args, "--scene")?;
    opts.character_filters = flag_values(args, "--char")?;
    opts.prop_filters = flag_values(args, "--prop")?;
    opts.anim_filters = flag_values(args, "--anim")?;
    opts.audio_filters = flag_values(args, "--audio")?;
    opts.ui_sprite_filters = flag_values(args, "--ui-sprite")?;
    opts.ui_panel_filters = flag_values(args, "--ui-panel")?;
    opts.assets_only = has_flag(args, "--assets-only");
    if let Some(v) = flag_value(args, "--max-scenes")? {
        opts.max_scenes = v.parse::<usize>().map_err(|e| e.to_string())?;
    }
    if let Some(v) = flag_value(args, "--max-chars")? {
        opts.max_characters = v.parse::<usize>().map_err(|e| e.to_string())?;
    }
    if let Some(v) = flag_value(args, "--max-props")? {
        opts.max_props = v.parse::<usize>().map_err(|e| e.to_string())?;
    }
    if let Some(v) = flag_value(args, "--max-audio")? {
        opts.max_audio = v.parse::<usize>().map_err(|e| e.to_string())?;
    }
    if let Some(v) = flag_value(args, "--max-fonts")? {
        opts.max_fonts = v.parse::<usize>().map_err(|e| e.to_string())?;
    }
    if let Some(v) = flag_value(args, "--max-ui-sprites")? {
        opts.max_ui_sprites = v.parse::<usize>().map_err(|e| e.to_string())?;
    }
    if let Some(v) = flag_value(args, "--max-ui-panels")? {
        opts.max_ui_panels = v.parse::<usize>().map_err(|e| e.to_string())?;
    }
    if let Some(v) = flag_value(args, "--max-tex")? {
        opts.max_texture_dim = Some(v.parse::<u32>().map_err(|e| e.to_string())?);
    }

    let t0 = Instant::now();
    let mut ws = load_path_with_options(input, WorkspaceLoadOptions::default())
        .map_err(|e| e.to_string())?;
    let rep = export_bevy_project(&mut ws, Path::new(out_dir), &opts).map_err(|e| e.to_string())?;
    let m = &rep.manifest;
    println!(
        "Generated Bevy 0.20 project `{}` at `{}` in {:.2?}",
        m.project_name,
        rep.output_dir.display(),
        t0.elapsed()
    );
    println!(
        "  scenes: {} | characters: {} | props: {} | audio: {} | fonts: {} | ui_sprites: {} | ui_panels: {}",
        m.scenes.len(),
        m.characters.len(),
        m.props.len(),
        m.audio.len(),
        m.fonts.len(),
        m.ui_sprites.len(),
        m.ui_panels.len(),
    );
    for w in rep.warnings.iter().take(10) {
        eprintln!("  warn: {w}");
    }
    Ok(())
}

const DISASM_VALUE_FLAGS: &[&str] = &["--max-insns"];
const DISASM_BOOL_FLAGS: &[&str] = &[
    "--no-hex",
    "--no-cfg",
    "--no-annotations",
    "--no-field-names",
];

fn il2cpp_report_from_workspace(
    input: &str,
    ws: &devx_unity_io::UnityWorkspaceData,
) -> Result<Il2CppDumpReport, String> {
    let scripts = ScriptsIndex::from_workspace(ws);
    if let Some(report) = scripts.il2cpp_recovered {
        return Ok(report);
    }
    let reason = scripts
        .il2cpp_error
        .as_deref()
        .unwrap_or("no global-metadata.dat + IL2CPP native binary found in workspace");
    Err(format!("IL2CPP recovery failed for `{input}`: {reason}"))
}

fn load_il2cpp_report(input: &str) -> Result<Il2CppDumpReport, String> {
    let ws = load_path_with_options(input, WorkspaceLoadOptions::default())
        .map_err(|e| format!("failed to open `{input}`: {e}"))?;
    il2cpp_report_from_workspace(input, &ws)
}

fn parse_disasm_options(args: &[String]) -> Result<Il2CppDisasmOptions, String> {
    let mut opts = Il2CppDisasmOptions {
        hex_bytes: !has_flag(args, "--no-hex"),
        cfg: !has_flag(args, "--no-cfg"),
        annotations: !has_flag(args, "--no-annotations"),
        field_names: !has_flag(args, "--no-field-names"),
        ..Default::default()
    };
    if let Some(raw) = flag_value(args, "--max-insns")? {
        let parsed = raw
            .parse::<usize>()
            .map_err(|_| format!("invalid --max-insns value `{raw}`"))?;
        if parsed == 0 {
            return Err("--max-insns must be >= 1".to_owned());
        }
        opts.max_instructions = parsed;
    }
    Ok(opts)
}

fn parse_body_mode_flags(args: &[String]) -> Result<(bool, bool), String> {
    let (mut disasm, mut pseudo_c) = if let Some(raw) = flag_value(args, "--body-mode")? {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => (false, false),
            "assembly" | "asm" | "disasm" => (true, false),
            "csharp" | "cs" | "pseudo-c" | "pseudoc" => (false, true),
            "both" | "all" => (true, true),
            other => {
                return Err(format!(
                    "invalid --body-mode `{other}` (expected off, assembly, csharp, or both)"
                ));
            }
        }
    } else {
        (false, false)
    };
    if has_flag(args, "--disasm") || has_flag(args, "--assembly") {
        disasm = true;
    }
    if has_flag(args, "--pseudo-c") {
        pseudo_c = true;
    }
    Ok((disasm, pseudo_c))
}

fn has_explicit_body_mode(args: &[String]) -> bool {
    args.iter().any(|a| {
        matches!(
            a.as_str(),
            "--body-mode" | "--pseudo-c" | "--disasm" | "--assembly"
        )
    })
}

fn parse_max_method_bytes(args: &[String]) -> Result<Option<usize>, String> {
    match flag_value(args, "--max-method-bytes")? {
        Some(raw) => raw
            .parse::<usize>()
            .map(Some)
            .map_err(|_| format!("invalid --max-method-bytes value `{raw}`")),
        None => Ok(None),
    }
}

/// Extracts method declaration + body blocks matching any target RVA from a
/// formatted `TypeDef` C# listing (`// RVA: 0xHEX ...`).
fn extract_method_blocks_by_rva(src: &str, target_rvas: &[u64]) -> Vec<String> {
    if target_rvas.is_empty() {
        return Vec::new();
    }
    let tags: Vec<String> = target_rvas
        .iter()
        .filter(|&&r| r != 0)
        .map(|r| format!("// RVA: 0x{r:X} "))
        .collect();
    if tags.is_empty() {
        return Vec::new();
    }

    let lines: Vec<&str> = src.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if !tags.iter().any(|t| lines[i].contains(t.as_str())) {
            i += 1;
            continue;
        }
        // Pull preceding attribute lines (`\t[Attr]`) immediately attached to this method.
        let mut start = i;
        while start > 0 && lines[start - 1].trim_start().starts_with('[') {
            start -= 1;
        }
        let mut j = i + 1;
        let mut depth: i32 = 0;
        let mut seen_open = false;
        while j < lines.len() {
            let trimmed = lines[j].trim();
            if !seen_open && trimmed.ends_with(';') && !trimmed.starts_with("//") {
                j += 1;
                break;
            }
            for ch in lines[j].chars() {
                if ch == '{' {
                    depth += 1;
                    seen_open = true;
                } else if ch == '}' {
                    depth -= 1;
                }
            }
            j += 1;
            if seen_open && depth <= 0 {
                break;
            }
        }
        blocks.push(lines[start..j].join("\n"));
        i = j;
    }
    blocks
}

fn cmd_dump_il2cpp(args: &[String]) -> Result<(), String> {
    let mut value_flags = vec![
        "--body-mode",
        "--max-method-bytes",
        "--filter-asm",
        "--filter-type",
    ];
    value_flags.extend_from_slice(DISASM_VALUE_FLAGS);
    let mut bool_flags = vec!["--pseudo-c", "--fold-ifix", "--full-dump", "--dummy-dll", "--disasm", "--assembly"];
    bool_flags.extend_from_slice(DISASM_BOOL_FLAGS);

    let pos = positional_args(args, &value_flags, &bool_flags);
    let input = pos.first().ok_or("dump-il2cpp requires <input> <out_dir>")?;
    let out_dir = pos.get(1).ok_or("dump-il2cpp requires <out_dir>")?;

    let (include_disassembly, include_pseudo_c) = parse_body_mode_flags(args)?;
    let max_method_bytes = parse_max_method_bytes(args)?;
    let disasm = parse_disasm_options(args)?;
    let include_assemblies = flag_values(args, "--filter-asm")?;
    let raw_type_filters = flag_values(args, "--filter-type")?;

    let t0 = Instant::now();
    let report = load_il2cpp_report(input)?;
    if include_disassembly && !report.arch_has_native_code {
        eprintln!(
            "  note: {} does not expose native code; skipping disassembly",
            report.arch
        );
    }

    let include_types = if raw_type_filters.is_empty() {
        Vec::new()
    } else {
        resolve_il2cpp_type_filters(&report, &include_assemblies, &raw_type_filters)?
    };
    let full_dump = has_flag(args, "--full-dump");

    let cfg = Il2CppExportConfig {
        write_dummy_dlls: has_flag(args, "--dummy-dll"),
        include_pseudo_c,
        fold_ifix_wrappers: has_flag(args, "--fold-ifix"),
        max_method_bytes,
        include_disassembly,
        disasm,
        include_assemblies,
        include_types,
        filter_headers: !full_dump,
        full_dump,
        ..Il2CppExportConfig::default()
    };
    let written = report
        .write_full_dump(Path::new(out_dir), &cfg, |msg| println!("  {msg}"))
        .map_err(|e| e.to_string())?;
    println!(
        "Dumped IL2CPP ({} | metadata v{} | {} images, {} types, {} methods) to `{}` ({} artifacts) in {:.2?}",
        report.binary_format,
        report.metadata_version,
        report.images.len(),
        report.types.len(),
        report.total_method_count,
        out_dir,
        written.len(),
        t0.elapsed(),
    );
    Ok(())
}

fn cmd_export_unity_project(args: &[String]) -> Result<(), String> {
    let pos = positional_args(
        args,
        &["--bundle-filter", "--max-scenes", "--max-prefabs"],
        &["--all-bundles", "--no-scripts"],
    );
    let input = pos
        .first()
        .ok_or("export-unity-project requires <input> <out_dir>")?;
    let out_dir = pos
        .get(1)
        .ok_or("export-unity-project requires <out_dir>")?;

    let mut opts = UnityProjectExportOptions {
        bundle_filter: flag_value(args, "--bundle-filter")?,
        load_all_bundles: has_flag(args, "--all-bundles"),
        include_scripts: !has_flag(args, "--no-scripts"),
        ..Default::default()
    };
    if let Some(v) = flag_value(args, "--max-scenes")? {
        opts.max_scenes = v.parse::<usize>().map_err(|e| e.to_string())?;
    }
    if let Some(v) = flag_value(args, "--max-prefabs")? {
        opts.max_prefabs = v.parse::<usize>().map_err(|e| e.to_string())?;
    }

    let t0 = Instant::now();
    let mut ws = load_path_with_options(input, WorkspaceLoadOptions::default())
        .map_err(|e| e.to_string())?;
    let report = export_unity_project(&mut ws, Path::new(out_dir), &opts)
        .map_err(|e| e.to_string())?;
    println!("Exported {} to `{out_dir}` in {:.2?}", report.summary_line(), t0.elapsed());
    for w in report.warnings.iter().take(10) {
        println!("  warn: {w}");
    }
    Ok(())
}

fn resolve_il2cpp_type_filters(
    report: &Il2CppDumpReport,
    asm_filters: &[String],
    raw_patterns: &[String],
) -> Result<Vec<String>, String> {
    let patterns = expand_pipe_filters(raw_patterns);
    let mut resolved = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for pat in &patterns {
        let p_low = pat.to_ascii_lowercase();
        let mut matched_for_pat = Vec::new();
        for td in &report.types {
            if !asm_filters.is_empty() && !matches_pipe_filter(&td.assembly_name, asm_filters) {
                continue;
            }
            if td.full_name.eq_ignore_ascii_case(pat) || td.name.eq_ignore_ascii_case(pat) {
                matched_for_pat.push(td.full_name.clone());
            }
        }
        if matched_for_pat.is_empty() {
            for td in &report.types {
                if !asm_filters.is_empty() && !matches_pipe_filter(&td.assembly_name, asm_filters) {
                    continue;
                }
                if td.full_name.to_ascii_lowercase().contains(&p_low)
                    || td.name.to_ascii_lowercase().contains(&p_low)
                {
                    matched_for_pat.push(td.full_name.clone());
                }
            }
        }
        if matched_for_pat.is_empty() {
            eprintln!("  note: --filter-type `{pat}` matched 0 types");
        }
        for full in matched_for_pat {
            if seen.insert(full.clone()) {
                resolved.push(full);
            }
        }
    }
    if resolved.is_empty() {
        return Err(format!(
            "no IL2CPP types matched --filter-type {:?}; use `disasm-il2cpp --search <substr>` to inspect type names",
            raw_patterns
        ));
    }
    eprintln!(
        "  filter-type resolved {} matching type(s): {:?}",
        resolved.len(),
        resolved.iter().take(10).collect::<Vec<_>>()
    );
    Ok(resolved)
}

fn find_il2cpp_type<'a>(
    report: &'a Il2CppDumpReport,
    asm_filter: Option<&str>,
    query: &str,
) -> Result<&'a Il2CppRecoveredType, String> {
    let asm = asm_filter.unwrap_or("");
    if let Some((ns, cls)) = query.rsplit_once('.')
        && let Some(td) = report.find_type(asm, ns, cls)
    {
        return Ok(td);
    }
    if let Some(td) = report.find_type(asm, "", query) {
        return Ok(td);
    }

    let q_lower = query.to_ascii_lowercase();
    let asm_lower = asm_filter.map(|a| a.to_ascii_lowercase());
    let matches_asm = |td: &Il2CppRecoveredType| {
        asm_lower
            .as_deref()
            .is_none_or(|a| td.assembly_name.to_ascii_lowercase().contains(a))
    };

    let exact: Vec<&Il2CppRecoveredType> = report
        .types
        .iter()
        .filter(|td| {
            matches_asm(td)
                && (td.full_name.to_ascii_lowercase() == q_lower
                    || td.name.to_ascii_lowercase() == q_lower)
        })
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0]);
    }
    if !exact.is_empty() {
        return Ok(exact[0]);
    }

    let partial: Vec<&Il2CppRecoveredType> = report
        .types
        .iter()
        .filter(|td| {
            matches_asm(td)
                && (td.full_name.to_ascii_lowercase().contains(&q_lower)
                    || td.name.to_ascii_lowercase().contains(&q_lower))
        })
        .collect();
    match partial.as_slice() {
        [single] => Ok(single),
        [] => Err(format!("no IL2CPP type matching `{query}`")),
        many => {
            let preview: Vec<String> = many
                .iter()
                .take(8)
                .map(|t| format!("{} ({})", t.full_name, t.assembly_name))
                .collect();
            eprintln!(
                "  note: `{query}` matched {} types, picking `{}` (candidates: {})",
                many.len(),
                many[0].full_name,
                preview.join(", ")
            );
            Ok(many[0])
        }
    }
}

fn print_disassembly_report(
    rep: &devx_scripts::il2cpp::DisassembledMethodReport,
    header_hint: Option<&str>,
) {
    let label = rep
        .method_name
        .as_deref()
        .or(header_hint)
        .unwrap_or("<subroutine>");
    let thumb_note = if rep.thumb { " [Thumb]" } else { "" };
    println!(
        "=== {label} | {}{} | RVA 0x{:X} (clean 0x{:X}) | VA 0x{:X} | File 0x{:X} | {} insns / {} bytes ===",
        rep.arch,
        thumb_note,
        rep.rva,
        rep.clean_rva,
        rep.va,
        rep.file_offset,
        rep.instruction_count,
        rep.byte_size
    );
    println!("{}", rep.text);
}

fn format_method_flags(m: &Il2CppMethodEntry) -> String {
    let mut tags = Vec::new();
    if m.is_static {
        tags.push("static");
    }
    if m.is_abstract {
        tags.push("abstract");
    }
    if m.thumb {
        tags.push("thumb");
    }
    if tags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", tags.join(","))
    }
}

fn cmd_disasm_il2cpp(args: &[String]) -> Result<(), String> {
    let mut value_flags = vec![
        "--type",
        "--search",
        "--method",
        "--rva",
        "--asm",
        "--limit",
        "--body-mode",
        "--max-method-bytes",
    ];
    value_flags.extend_from_slice(DISASM_VALUE_FLAGS);
    let mut bool_flags = vec!["--all", "--pseudo-c", "--fold-ifix", "--disasm", "--assembly"];
    bool_flags.extend_from_slice(DISASM_BOOL_FLAGS);

    let pos = positional_args(args, &value_flags, &bool_flags);
    let input = pos.first().ok_or("disasm-il2cpp requires <input>")?;

    let rva_flag = flag_value(args, "--rva")?;
    let type_flags = expand_pipe_filters(&flag_values(args, "--type")?);
    let search_flags = expand_pipe_filters(&flag_values(args, "--search")?);
    let method_flag = flag_value(args, "--method")?;
    let asm_flag = flag_value(args, "--asm")?;
    let disasm_all = has_flag(args, "--all");
    let fold_ifix = has_flag(args, "--fold-ifix");
    let max_method_bytes = parse_max_method_bytes(args)?;
    let explicit_body_mode = has_explicit_body_mode(args);
    let (want_disasm, want_pseudo) = if explicit_body_mode {
        parse_body_mode_flags(args)?
    } else {
        (true, false)
    };
    let explicit_limit = flag_value(args, "--limit")?;
    let limit = match explicit_limit {
        Some(ref raw) => raw
            .parse::<usize>()
            .map_err(|_| format!("invalid --limit value `{raw}`"))?
            .max(1),
        None => 20,
    };

    if rva_flag.is_none() && type_flags.is_empty() && search_flags.is_empty() {
        return Err("disasm-il2cpp requires --rva <0xHEX>, --type <TypeName>, or --search <substr>".to_owned());
    }

    let disasm_opts = parse_disasm_options(args)?;
    let report = load_il2cpp_report(input)?;

    if !search_flags.is_empty() && rva_flag.is_none() && type_flags.is_empty() {
        let search_limit = if explicit_limit.is_some() {
            limit
        } else if disasm_all {
            usize::MAX
        } else {
            100
        };
        let mut matches: Vec<&Il2CppRecoveredType> = report
            .types
            .iter()
            .filter(|td| {
                asm_flag
                    .as_deref()
                    .is_none_or(|a| td.assembly_name.to_ascii_lowercase().contains(&a.to_ascii_lowercase()))
                    && matches_pipe_filter(&td.full_name, &search_flags)
            })
            .collect();
        matches.sort_by(|a, b| a.full_name.cmp(&b.full_name));
        println!(
            "IL2CPP types matching {:?}: {} hit(s) (showing up to {search_limit})",
            search_flags,
            matches.len()
        );
        for td in matches.iter().take(search_limit) {
            println!(
                "  #{} {} {} ({}, {}f, {}m)",
                td.type_def_index, td.kind, td.full_name, td.assembly_name, td.fields.len(), td.method_count
            );
        }
        return Ok(());
    }

    if !report.arch_has_native_code {
        return Err(format!(
            "{} ({}) has no native machine code to disassemble",
            report.binary_format, report.arch
        ));
    }
    report
        .set_max_method_bytes(max_method_bytes)
        .map_err(|e| e.to_string())?;
    report
        .set_fold_ifix_wrappers(fold_ifix)
        .map_err(|e| e.to_string())?;

    let first_type_query = type_flags.first().map(String::as_str);
    let resolved_type = first_type_query
        .map(|q| find_il2cpp_type(&report, asm_flag.as_deref(), q))
        .transpose()?;

    if let Some(rva_raw) = rva_flag {
        let rva = parse_rva_hex(&rva_raw).map_err(|e| e.to_string())?;
        let mut type_hint = resolved_type.map(|t| t.type_def_index);
        let mut method_hint = if let Some(td) = resolved_type {
            report
                .list_type_methods(td.type_def_index)
                .ok()
                .and_then(|ms| {
                    ms.into_iter()
                        .find(|m| m.rva == rva || (m.rva & !1) == (rva & !1))
                })
                .map(|m| m.method_def_index)
        } else {
            None
        };
        if let Some((owner_td, owner_md)) = (type_hint.is_none() || method_hint.is_none())
            .then(|| report.find_method_owner_by_rva(rva).ok().flatten())
            .flatten()
        {
            type_hint.get_or_insert(owner_td);
            method_hint.get_or_insert(owner_md);
        }
        if want_disasm || !want_pseudo {
            let disasm = report
                .disassemble_rva(rva, type_hint, method_hint, &disasm_opts)
                .map_err(|e| e.to_string())?;
            print_disassembly_report(&disasm, resolved_type.map(|t| t.full_name.as_str()));
        }
        if want_pseudo {
            if let Some(td_idx) = type_hint {
                let src = report
                    .format_type_source(td_idx, false, true, &disasm_opts)
                    .map_err(|e| e.to_string())?;
                let blocks = extract_method_blocks_by_rva(&src, &[rva]);
                if blocks.is_empty() {
                    println!("{src}");
                } else {
                    for b in blocks {
                        println!("{b}");
                    }
                }
            } else {
                eprintln!(
                    "note: RVA 0x{rva:X} does not match a managed method start; skipping Pseudo-C output"
                );
            }
        }
        return Ok(());
    }

    let mut target_types: Vec<&Il2CppRecoveredType> = Vec::with_capacity(type_flags.len());
    for q in &type_flags {
        target_types.push(find_il2cpp_type(&report, asm_flag.as_deref(), q)?);
    }

    let method_alts: Vec<String> = method_flag
        .as_deref()
        .map(|s| {
            s.split('|')
                .map(|p| p.trim().to_ascii_lowercase())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default();

    for (ti, td) in target_types.iter().enumerate() {
        if ti > 0 {
            println!();
        }
        let methods = report
            .list_type_methods(td.type_def_index)
            .map_err(|e| e.to_string())?;
        println!(
            "Type {} ({}, {}, {} methods):",
            td.full_name,
            td.assembly_name,
            report.arch,
            methods.len()
        );

        let list_only = method_flag.is_none()
            && !disasm_all
            && !want_pseudo
            && !(explicit_body_mode && want_disasm);
        if list_only {
            for (i, m) in methods.iter().enumerate() {
                println!(
                    "  [{i:>2}] RVA 0x{:<8X} | VA 0x{:<10X} | Size {:>5}{} | {}",
                    m.rva,
                    m.va,
                    m.body_size,
                    format_method_flags(m),
                    m.signature
                );
            }
            continue;
        }

        let selected: Vec<&Il2CppMethodEntry> = if !method_alts.is_empty() {
            let exact: Vec<&Il2CppMethodEntry> = methods
                .iter()
                .filter(|m| {
                    let ml = m.name.to_ascii_lowercase();
                    method_alts.contains(&ml)
                })
                .collect();
            if !exact.is_empty() {
                exact
            } else {
                methods
                    .iter()
                    .filter(|m| {
                        let nl = m.name.to_ascii_lowercase();
                        let sl = m.signature.to_ascii_lowercase();
                        method_alts
                            .iter()
                            .any(|alt| nl.contains(alt) || sl.contains(alt))
                    })
                    .collect()
            }
        } else {
            methods.iter().collect()
        };

        if selected.is_empty() {
            return Err(format!(
                "no methods on `{}` matching `{}`",
                td.full_name,
                method_flag.as_deref().unwrap_or("")
            ));
        }

        if want_pseudo {
            let src = report
                .format_type_source(td.type_def_index, want_disasm, true, &disasm_opts)
                .map_err(|e| e.to_string())?;
            if method_flag.is_some() {
                let target_rvas: Vec<u64> = selected.iter().take(limit).map(|m| m.rva).collect();
                let blocks = extract_method_blocks_by_rva(&src, &target_rvas);
                if blocks.is_empty() {
                    println!("{src}");
                } else {
                    for b in blocks {
                        println!("{b}\n");
                    }
                }
            } else {
                println!("{src}");
            }
            continue;
        }

        let mut shown = 0usize;
        for m in selected {
            if m.rva == 0 || m.is_abstract {
                println!(
                    "=== {} :: {} (no native body: RVA 0x{:X}{}) ===",
                    td.full_name,
                    m.signature,
                    m.rva,
                    format_method_flags(m)
                );
                continue;
            }
            if shown >= limit {
                println!("... reached --limit {limit}; pass a larger --limit or --method filter");
                break;
            }
            let disasm = report
                .disassemble_rva(
                    m.rva,
                    Some(td.type_def_index),
                    Some(m.method_def_index),
                    &disasm_opts,
                )
                .map_err(|e| e.to_string())?;
            print_disassembly_report(&disasm, Some(&m.signature));
            shown += 1;
        }
    }
    Ok(())
}

fn cmd_decode_oc(args: &[String]) -> Result<(), String> {
    let pos = positional_args(
        args,
        &["--filter", "--raw"],
        &["--summary", "--strict", "--compact"],
    );
    let input = pos.first().ok_or("decode-oc requires <input> <out_dir>")?;
    let out_dir = pos.get(1).ok_or("decode-oc requires <out_dir>")?;
    let filters: Vec<String> = expand_pipe_filters(&flag_values(args, "--filter")?)
        .into_iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let raw_filters: Vec<String> = expand_pipe_filters(&flag_values(args, "--raw")?)
        .into_iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let print_summary = has_flag(args, "--summary");
    let strict = has_flag(args, "--strict");
    let compact = has_flag(args, "--compact");

    let t0 = Instant::now();
    let ws = load_path_with_options(input, WorkspaceLoadOptions::default())
        .map_err(|e| format!("failed to open `{input}`: {e}"))?;
    let report = il2cpp_report_from_workspace(input, &ws)?;

    let mut oc_items: Vec<&devx_unity_io::RawArtifact> = ws
        .raw_artifacts
        .iter()
        .filter(|art| {
            let low = art.virtual_path.to_ascii_lowercase();
            low.ends_with(".oc")
                && (filters.is_empty() || filters.iter().any(|f| low.contains(f)))
        })
        .collect();
    oc_items.sort_by(|a, b| a.virtual_path.cmp(&b.virtual_path));

    let oc_inputs: Vec<devx_scripts::oc::OcFileInput<'_>> = oc_items
        .iter()
        .map(|art| devx_scripts::oc::OcFileInput {
            virtual_path: &art.virtual_path,
            bytes: &art.bytes,
        })
        .collect();

    let decoded = devx_scripts::oc::decode_oc_batch(
        &report,
        &oc_inputs,
        devx_scripts::oc::OcDecodeOptions {
            strict_pipe_values: strict,
        },
    )
    .map_err(|e| e.to_string())?;

    let out_root = Path::new(out_dir);
    std::fs::create_dir_all(out_root)
        .map_err(|e| format!("failed to create `{out_dir}`: {e}"))?;

    let mut total_records = 0usize;
    let mut dirty_tables = 0usize;
    let mut table_summaries = Vec::with_capacity(decoded.tables.len());
    for table in &decoded.tables {
        total_records += table.records.len();
        if table.dirty_pipe_cells > 0 {
            dirty_tables += 1;
        }
        let payload = serde_json::json!({
            "type": table.type_full_name,
            "source_path": table.source_path,
            "singleton": table.is_singleton,
            "count": table.records.len(),
            "pipe_fields": table.pipe_fields,
            "dirty_pipe_cells": table.dirty_pipe_cells,
            "records": table.records,
        });
        let dest = out_root.join(format!("{}.json", table.file_stem));
        let json_bytes = if compact {
            serde_json::to_vec(&payload)
        } else {
            serde_json::to_vec_pretty(&payload)
        }
        .map_err(|e| e.to_string())?;
        std::fs::write(&dest, json_bytes)
            .map_err(|e| format!("failed to write `{}`: {e}", dest.display()))?;

        if print_summary {
            println!(
                "  {:<36} | {:>5} rec(s) | {:>7} B | pipe={:?}",
                table.file_stem,
                table.records.len(),
                table.byte_len,
                table.pipe_fields
            );
        }
        table_summaries.push(serde_json::json!({
            "file_stem": table.file_stem,
            "type": table.type_full_name,
            "count": table.records.len(),
            "byte_len": table.byte_len,
            "singleton": table.is_singleton,
            "pipe_fields": table.pipe_fields,
            "dirty_pipe_cells": table.dirty_pipe_cells,
        }));
    }

    let summary_payload = serde_json::json!({
        "decoded_count": decoded.tables.len(),
        "total_records": total_records,
        "skipped_unmatched": decoded.skipped_unmatched,
        "errors": decoded.errors,
        "tables": table_summaries,
    });
    std::fs::write(
        out_root.join("_summary.json"),
        serde_json::to_vec_pretty(&summary_payload).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    let mut copied_raw = 0usize;
    if !raw_filters.is_empty() {
        for art in &ws.raw_artifacts {
            let low = art.virtual_path.to_ascii_lowercase();
            if !low.ends_with(".oc") && raw_filters.iter().any(|f| low.contains(f)) {
                let leaf = art.virtual_path.rsplit('/').next().unwrap_or("raw.bin");
                if leaf.eq_ignore_ascii_case("scenecell.obj")
                    && let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&art.bytes)
                    && let Some(list) = parsed.get("sceneList").and_then(|v| v.as_array())
                {
                    let non_empty: Vec<&serde_json::Value> = list
                        .iter()
                        .filter(|c| {
                            c.get("smallSceneList")
                                .and_then(|s| s.as_array())
                                .is_some_and(|a| !a.is_empty())
                        })
                        .collect();
                    let compact_json = serde_json::json!({ "cells": non_empty });
                    if let Ok(bytes) = serde_json::to_vec(&compact_json) {
                        let _ = std::fs::write(out_root.join("scenecell.json"), bytes);
                    }
                }
                let dest = out_root.join(leaf);
                std::fs::write(&dest, &*art.bytes)
                    .map_err(|e| format!("failed to write raw `{}`: {e}", dest.display()))?;
                copied_raw += 1;
            }
        }
    }

    println!(
        "Decoded {} .oc table(s) ({} records, {} raw file(s), {} skipped non-cfg, {} error(s), {} table(s) with non-numeric pipe cells) to `{}` in {:.2?}",
        decoded.tables.len(),
        total_records,
        copied_raw,
        decoded.skipped_unmatched.len(),
        decoded.errors.len(),
        dirty_tables,
        out_dir,
        t0.elapsed()
    );
    for skip in decoded.skipped_unmatched.iter().take(10) {
        eprintln!("  skip (no cfg.* schema): {skip}");
    }
    for err in decoded.errors.iter().take(10) {
        eprintln!("  error: {}: {}", err.source_path, err.message);
    }
    if strict && !decoded.errors.is_empty() {
        return Err(format!("decode-oc failed with {} error(s) in --strict mode", decoded.errors.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{expand_pipe_filters, extract_method_blocks_by_rva, matches_pipe_filter};

    #[test]
    fn extract_method_blocks_by_rva_selects_only_target_method() {
        let src = "\
public class Main : MonoBehaviour
{
\t// RVA: 0x1DE053C Offset: 0x1DD053C VA: 0x1DE053C
\tprivate void Awake()
\t{
\t\tif (true) { }
\t}

\t[CompilerGenerated]
\t// RVA: 0x1DE0900 Offset: 0x1DD0900 VA: 0x1DE0900
\tpublic void OnUserLogOut()
\t{
\t\treturn;
\t}
}";
        let blocks = extract_method_blocks_by_rva(src, &[0x1DE0900]);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains("[CompilerGenerated]"));
        assert!(blocks[0].contains("OnUserLogOut"));
        assert!(!blocks[0].contains("Awake"));
    }

    #[test]
    fn pipe_filter_expands_and_matches_case_insensitively() {
        let raw = vec!["man_hair | woman_hair".to_owned(), "head1".to_owned()];
        assert_eq!(
            expand_pipe_filters(&raw),
            vec!["man_hair", "woman_hair", "head1"]
        );
        assert!(matches_pipe_filter("assets/role/WOMAN_HAIR_02.ab", &raw));
        assert!(matches_pipe_filter("icon/head1/man_tou_1_1.ab", &raw));
        assert!(!matches_pipe_filter("sound/bgm.ab", &raw));
    }
}
