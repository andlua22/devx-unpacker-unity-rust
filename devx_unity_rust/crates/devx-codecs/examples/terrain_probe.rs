//! Inspect shader/layer references for a selected streamed terrain bundle.
use devx_codecs::material::decode_material;
use devx_unity_io::{
    WorkspaceLoadOptions, load_path_with_options, read_object, unity_value_to_json,
};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let input = args.get(1).ok_or("expected input APK path")?;
    let filter = args.get(2).ok_or("expected bundle filter")?;
    let mut workspace = load_path_with_options(Path::new(input), WorkspaceLoadOptions::default())?;
    for dependency_filter in args.iter().skip(3) {
        workspace.load_deferred_bundles_filtered(Some(dependency_filter))?;
    }
    let (_, report) = workspace.load_deferred_bundles_with_deps(Some(filter))?;
    println!("dependencies: {report:?}");
    for loaded in &workspace.files {
        if args.iter().skip(3).any(|f| {
            loaded
                .virtual_path
                .to_ascii_lowercase()
                .contains(&f.to_ascii_lowercase())
        }) {
            println!("DEPENDENCY {}", loaded.virtual_path);
            for info in &loaded.file.objects {
                if let Ok(obj) = read_object(loaded, info)
                    && (obj.class_id() == 28 || obj.class_id() == 48)
                {
                    println!(
                        "DEPENDENCY OBJECT {}:{} {} ({})",
                        loaded.file_index,
                        info.path_id,
                        obj.name().unwrap_or_default(),
                        obj.class_id()
                    );
                }
            }
        }
        if !loaded
            .virtual_path
            .to_ascii_lowercase()
            .contains(&filter.to_ascii_lowercase())
        {
            continue;
        }
        println!("FILE {}", loaded.virtual_path);
        for (i, ext) in loaded.file.externals.iter().enumerate() {
            println!(
                "external {}: {:?} / {:?}, resolved {:?}",
                i + 1,
                ext.path,
                ext.temp_empty,
                workspace.resolve_external_file(loaded.file_index, (i + 1) as i32)
            );
        }
        for info in &loaded.file.objects {
            if info.type_id != 21 {
                continue;
            }
            let obj = read_object(loaded, info)?;
            if obj.class_id() != 21 {
                continue;
            }
            let material = decode_material(&obj)?;
            println!("MATERIAL {}", serde_json::to_string_pretty(&material)?);
            for (fid, pid) in material
                .textures
                .iter()
                .map(|s| (s.texture_file_id, s.texture_path_id))
                .chain([(material.shader_file_id, material.shader_path_id)])
            {
                let fi = workspace
                    .resolve_external_file(loaded.file_index, fid)
                    .or_else(|| {
                        workspace
                            .files
                            .iter()
                            .position(|f| f.file.find_object(pid).is_some())
                    });
                let Some(fi) = fi else {
                    continue;
                };
                let Some((source, target)) = workspace.object_info(fi, pid) else {
                    continue;
                };
                let target = read_object(source, target)?;
                println!(
                    "TARGET {fid}:{pid} {} ({}) in {}",
                    target.name().unwrap_or_default(),
                    target.class_id(),
                    source.virtual_path
                );
                if target.class_id() == 48 {
                    println!(
                        "SHADER {}",
                        serde_json::to_string_pretty(&unity_value_to_json(
                            &devx_unity_io::UnityValue::Object(target.class.properties().clone()),
                            Some(12)
                        ))?
                    );
                }
            }
        }
    }
    Ok(())
}
