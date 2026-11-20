//! Native TerrainMeshBase splat rendering from DevX glTF material extras.
//! Keeps the APK's RGBA control map, tiled albedo layers and smoothness channels distinct.
use std::collections::HashMap;

use bevy::gltf::GltfMaterialExtras;
use bevy::material::OpaqueRendererMethod;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;
use serde::Deserialize;

use crate::battle::BattleLoadingSet;

pub struct TerrainMaterialPlugin;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct NativeTerrainReadinessSet;

impl Plugin for TerrainMaterialPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<TerrainMaterial>::default())
            .init_resource::<TerrainMaterialCache>()
            .init_resource::<NativeTerrainReadiness>()
            .add_systems(
                Update,
                (restore_native_terrain_materials, update_terrain_readiness)
                    .chain()
                    .after(BattleLoadingSet::PrepareWorld)
                    .in_set(NativeTerrainReadinessSet),
            );
    }
}

type TerrainMaterial = ExtendedMaterial<StandardMaterial, TerrainLayers>;

#[derive(Clone, Copy, Debug, ShaderType)]
struct TerrainUniforms {
    scales: Vec4,
    smoothness: Vec4,
    layer_mask: Vec4,
}

#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
struct TerrainLayers {
    #[uniform(100)]
    uniforms: TerrainUniforms,
    #[texture(101)]
    #[sampler(102)]
    control: Handle<Image>,
    #[texture(103)]
    #[sampler(104)]
    layer0: Handle<Image>,
    #[texture(105)]
    layer1: Handle<Image>,
    #[texture(106)]
    layer2: Handle<Image>,
    #[texture(107)]
    layer3: Handle<Image>,
}

impl MaterialExtension for TerrainLayers {
    fn fragment_shader() -> ShaderRef {
        "shaders/terrain_layers.wesl".into()
    }
}

#[derive(Resource, Default)]
struct TerrainMaterialCache {
    materials: HashMap<AssetId<StandardMaterial>, Handle<TerrainMaterial>>,
    linear_controls: HashMap<AssetId<Image>, Handle<Image>>,
}

#[derive(Deserialize)]
struct TerrainExtras {
    devx_terrain: NativeTerrain,
}

#[derive(Deserialize)]
struct NativeTerrain {
    version: u32,
    shader: String,
    control_texture: usize,
    normal_enabled: bool,
    layers: Vec<NativeLayer>,
}

#[derive(Deserialize)]
struct NativeLayer {
    texture: usize,
    scale: f32,
    smoothness: f32,
}

#[derive(Component)]
struct TerrainMaterialResolved;

#[derive(Component)]
pub(crate) struct NativeTerrainMaterial;

#[derive(Resource, Default)]
pub struct NativeTerrainReadiness {
    pub resolved_count: usize,
    pub pending_count: usize,
}

fn update_terrain_readiness(
    query: Query<(), With<NativeTerrainMaterial>>,
    pending: Query<&GltfMaterialExtras, With<MeshMaterial3d<StandardMaterial>>>,
    mut readiness: ResMut<NativeTerrainReadiness>,
) {
    readiness.resolved_count = query.iter().count();
    // A chunk may contain several native materials. Counting resolved meshes alone could
    // hide another chunk/material that is still waiting for its textures.
    readiness.pending_count = pending
        .iter()
        .filter(|extras| serde_json::from_str::<TerrainExtras>(&extras.value).is_ok())
        .count();
}

fn restore_native_terrain_materials(
    mut commands: Commands,
    query: Query<
        (
            Entity,
            &MeshMaterial3d<StandardMaterial>,
            &GltfMaterialExtras,
        ),
        Without<TerrainMaterialResolved>,
    >,
    standard_materials: Res<Assets<StandardMaterial>>,
    mut terrain_materials: ResMut<Assets<TerrainMaterial>>,
    mut images: ResMut<Assets<Image>>,
    asset_server: Res<AssetServer>,
    mut cache: ResMut<TerrainMaterialCache>,
) {
    for (entity, source_handle, extras) in &query {
        let Ok(TerrainExtras {
            devx_terrain: native,
        }) = serde_json::from_str(&extras.value)
        else {
            commands.entity(entity).insert(TerrainMaterialResolved);
            continue;
        };
        let supported_profile = matches!(
            native.shader.as_str(),
            "_URP/TerrainMeshBase/4 Textures" | "_URP/TerrainMeshBase/3 Textures"
        );
        if native.version != 1
            || !supported_profile
            || !(3..=4).contains(&native.layers.len())
            || native.normal_enabled
        {
            warn!(
                "Unsupported native terrain profile: {} version={} normal_enabled={}",
                native.shader, native.version, native.normal_enabled
            );
            commands.entity(entity).insert(TerrainMaterialResolved);
            continue;
        }
        if let Some(handle) = cache.materials.get(&source_handle.id()) {
            commands
                .entity(entity)
                .remove::<MeshMaterial3d<StandardMaterial>>()
                .insert((
                    MeshMaterial3d(handle.clone()),
                    TerrainMaterialResolved,
                    NativeTerrainMaterial,
                ));
            continue;
        }
        let Some(base) = standard_materials.get(&source_handle.0) else {
            continue;
        };
        let Some(path) = source_handle.0.path() else {
            continue;
        };
        let texture =
            |index| asset_server.load::<Image>(path.clone().with_label(format!("Texture{index}")));
        let control_source = texture(native.control_texture);
        let layers: [Handle<Image>; 4] = std::array::from_fn(|index| {
            texture(
                native
                    .layers
                    .get(index)
                    .unwrap_or(&native.layers[0])
                    .texture,
            )
        });
        if layers.iter().any(|handle| images.get(handle).is_none())
            || images.get(&control_source).is_none()
        {
            continue;
        }
        // glTF has no data-texture declaration for custom extras. Its unused images load as
        // sRGB, so clone only the control texture with a linear format before sampling weights.
        let control = if let Some(handle) = cache.linear_controls.get(&control_source.id()) {
            handle.clone()
        } else {
            let Some(original) = images.get(&control_source) else {
                continue;
            };
            let mut linear = original.clone();
            linear.texture_descriptor.format =
                linear.texture_descriptor.format.remove_srgb_suffix();
            let handle = images.add(linear);
            cache
                .linear_controls
                .insert(control_source.id(), handle.clone());
            handle
        };
        let mut base = base.clone();
        base.base_color_texture = None;
        base.metallic_roughness_texture = None;
        base.occlusion_texture = None;
        base.normal_map_texture = None;
        base.base_color = Color::WHITE;
        base.metallic = 0.0;
        base.opaque_render_method = OpaqueRendererMethod::Forward;
        let handle = terrain_materials.add(ExtendedMaterial {
            base,
            extension: TerrainLayers {
                uniforms: TerrainUniforms {
                    scales: Vec4::from_array(std::array::from_fn(|i| {
                        native.layers.get(i).map_or(1.0, |layer| layer.scale)
                    })),
                    smoothness: Vec4::from_array(std::array::from_fn(|i| {
                        native.layers.get(i).map_or(0.0, |layer| layer.smoothness)
                    })),
                    layer_mask: Vec4::new(
                        1.0,
                        1.0,
                        1.0,
                        if native.layers.len() == 4 { 1.0 } else { 0.0 },
                    ),
                },
                control,
                layer0: layers[0].clone(),
                layer1: layers[1].clone(),
                layer2: layers[2].clone(),
                layer3: layers[3].clone(),
            },
        });
        cache.materials.insert(source_handle.id(), handle.clone());
        commands
            .entity(entity)
            .remove::<MeshMaterial3d<StandardMaterial>>()
            .insert((
                MeshMaterial3d(handle),
                TerrainMaterialResolved,
                NativeTerrainMaterial,
            ));
        info!(
            "Restored native terrain material {} with {} tiled layers",
            path,
            native.layers.len()
        );
    }
}
