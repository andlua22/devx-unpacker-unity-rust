//! Inspect source clip names, controller references and decoded skin influences.
use devx_codecs::mesh::decode_mesh_full;
use devx_unity_io::{WorkspaceLoadOptions, load_path_with_options, read_object, unity_value_to_json};
use serde_json::json;
use std::collections::BTreeMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let input = args.get(1).ok_or("expected input path")?;
    let filter = args.get(2).map(String::as_str);
    let mut workspace = load_path_with_options(input, WorkspaceLoadOptions::default())?;
    if let Some(filter) = filter {
        workspace.load_deferred_bundles_with_deps(Some(filter))?;
    }
    let mut records = Vec::new();
    for loaded in &workspace.files {
        if filter.is_some_and(|f| !loaded.virtual_path.contains(f)) {
            continue;
        }
        for info in &loaded.file.objects {
            if !matches!(info.type_id, 1 | 43 | 74 | 91 | 95 | 137) {
                continue;
            }
            let object = read_object(loaded, info)?;
            let mut record = json!({"source": loaded.virtual_path, "path_id": info.path_id.to_string(),
                "class_id": info.type_id, "name": object.name().unwrap_or_default()});
            if info.type_id == 43 {
                let decoded = decode_mesh_full(&object, &loaded.file.unity_version, &workspace.streams)?;
                let mut influences = BTreeMap::<u16, usize>::new();
                if let Some(skin) = &decoded.skin {
                    for (joints, weights) in skin.joints.iter().zip(&skin.weights) {
                        for (&joint, &weight) in joints.iter().zip(weights) {
                            if weight > 0.0 {
                                *influences.entry(joint).or_default() += 1;
                            }
                        }
                    }
                    record["skin"] = json!({"joints_count": skin.joints.len(), "weights_count": skin.weights.len(),
                        "bind_poses": skin.bind_poses_row_major, "bone_name_hashes": skin.bone_name_hashes,
                        "positive_influence_counts": influences,
                        "all_weights_unit": skin.weights.iter().all(|w| *w == [1.0, 0.0, 0.0, 0.0])});
                }
                record["vertex_count"] = json!(decoded.base.positions.len());
            } else if matches!(info.type_id, 91 | 95 | 137) {
                record["properties"] = unity_value_to_json(
                    &devx_unity_io::UnityValue::Object(object.class.properties().clone()), None);
            }
            records.push(record);
        }
    }
    println!("{}", serde_json::to_string_pretty(&records)?);
    Ok(())
}
