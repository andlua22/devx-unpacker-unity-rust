//! Shared 3D character rig selection, glTF animation binding, and right-hand weapon socket
//! helpers shared across [`crate::lobby`], [`crate::ingame`], and [`crate::sandbox`].

use std::time::Duration;

use bevy::gltf::Gltf;
use bevy::prelude::*;

use crate::config::{AssetManifest, EquipCfgEntry, GameConfigs, GlbAssetEntry, PlayerProfileSave};

/// High-level locomotion/action state driving character clip selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimState {
    Idle,
    Walk,
    Run,
    Jump,
    Attack,
}

impl AnimState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Walk => "Walk",
            Self::Run => "Run",
            Self::Jump => "Jump",
            Self::Attack => "Attack",
        }
    }
}

/// Identifies whether an attached [`BoundAnimationSet`] drives the player rig or an ambient prop/NPC.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum BoundActorRole {
    Player,
    Other,
}

/// Built `AnimationGraph` clip index list attached to an entity once its glTF scene finishes spawning.
#[derive(Component)]
pub struct BoundAnimationSet {
    pub role: BoundActorRole,
    pub nodes: Vec<(String, AnimationNodeIndex)>,
}

/// Matches an animation state to the best clip in `nodes`, falling back to Walk/Idle/first clip.
pub fn pick_clip_for_state(
    anim_state: AnimState,
    nodes: &[(String, AnimationNodeIndex)],
) -> Option<(usize, AnimationNodeIndex)> {
    if nodes.is_empty() {
        return None;
    }
    let patterns: &[&str] = match anim_state {
        AnimState::Attack => &["attack", "fire", "shoot", "slash", "strike", "gj", "hit", "skill"],
        AnimState::Jump => &["jump", "tiao", "leap", "air"],
        AnimState::Run => &["run", "sprint", "dash", "pao", "jog"],
        AnimState::Walk => &["walk", "move", "zou"],
        AnimState::Idle => &[
            "kongshou_zhan_idle01",
            "womankongshou_zhan_idle01",
            "idle",
            "stand",
            "daiji",
            "wait",
            "breath",
        ],
    };
    for pat in patterns {
        if let Some((idx, (_, node))) = nodes
            .iter()
            .enumerate()
            .find(|(_, (name, _))| name.to_ascii_lowercase().contains(pat))
        {
            return Some((idx, *node));
        }
    }
    if anim_state == AnimState::Run
        && let Some(hit) = pick_clip_for_state(AnimState::Walk, nodes)
    {
        return Some(hit);
    }
    if matches!(anim_state, AnimState::Attack | AnimState::Jump | AnimState::Walk)
        && let Some(hit) = pick_clip_for_state(AnimState::Idle, nodes)
    {
        return Some(hit);
    }
    Some((0, nodes[0].1))
}

/// Constructs an [`AnimationGraphHandle`], [`AnimationTransitions`], and [`BoundAnimationSet`]
/// from a loaded [`Gltf`] asset and starts playing the selected clip.
#[allow(clippy::too_many_arguments)]
pub fn build_gltf_animation_set(
    gltf: &Gltf,
    label_prefix: &str,
    role: BoundActorRole,
    start_state: AnimState,
    fallback_clip_idx: usize,
    phase_offset_secs: f32,
    paused: bool,
    player: &mut AnimationPlayer,
    graphs: &mut Assets<AnimationGraph>,
) -> Option<(
    AnimationGraphHandle,
    AnimationTransitions,
    BoundAnimationSet,
)> {
    if gltf.animations.is_empty() {
        return None;
    }

    let mut graph = AnimationGraph::new();
    let mut nodes = Vec::with_capacity(gltf.animations.len());
    for (idx, clip) in gltf.animations.iter().enumerate() {
        let clip_name = gltf
            .named_animations
            .iter()
            .find(|(_, h)| *h == clip)
            .map(|(k, _)| k.to_string())
            .unwrap_or_else(|| format!("{label_prefix}_Clip{idx}"));
        let node_idx = graph.add_clip(clip.clone(), 1.0, graph.root);
        nodes.push((clip_name, node_idx));
    }

    let start_node = if role == BoundActorRole::Player {
        pick_clip_for_state(start_state, &nodes)
            .map(|(_, n)| n)
            .unwrap_or(nodes[0].1)
    } else if let Some((_, idle_n)) = pick_clip_for_state(AnimState::Idle, &nodes) {
        idle_n
    } else {
        nodes[fallback_clip_idx % nodes.len()].1
    };

    let mut transitions = AnimationTransitions::new();
    let active = transitions
        .play(player, start_node, Duration::ZERO)
        .repeat();
    active.seek_to(phase_offset_secs);
    if paused {
        active.pause();
    }

    Some((
        AnimationGraphHandle(graphs.add(graph)),
        transitions,
        BoundAnimationSet { role, nodes },
    ))
}

/// Walks the spawned skeletal hierarchy below `root` to find the highest-priority right-hand /
/// weapon mount bone entity (`Bip001 R Hand`, `RightHand`, `weapon`, etc.).
pub fn find_hand_socket_entity(
    root: Entity,
    children_q: &Query<&Children>,
    names_q: &Query<&Name>,
) -> Option<Entity> {
    const SOCKET_PATTERNS: &[&str] = &[
        "weapon",
        "r_hand",
        "righthand",
        "hand_r",
        "bip001 r hand",
        "bip001_r_hand",
        "mixamorig:righthand",
        "wrist_r",
        "r_wrist",
        "hand.r",
        "grip",
        "socket",
    ];
    let mut stack = vec![root];
    let mut best: Option<(usize, Entity)> = None;
    while let Some(e) = stack.pop() {
        if let Ok(name) = names_q.get(e) {
            let low = name.as_str().to_ascii_lowercase();
            // Never mount a new weapon onto an already-spawned weapon mesh subtree.
            if low.starts_with("ingameweapon_") || low.starts_with("lobbyweapon_") {
                continue;
            }
            for (rank, pat) in SOCKET_PATTERNS.iter().enumerate() {
                if low.contains(pat) {
                    if best.is_none_or(|(best_rank, _)| rank < best_rank) {
                        best = Some((rank, e));
                    }
                    break;
                }
            }
        }
        if let Ok(children) = children_q.get(e) {
            for child in children.iter() {
                stack.push(child);
            }
        }
    }
    best.map(|(_, e)| e)
}

/// Selects a playable male or female character body GLB from `manifest.characters`,
/// excluding modular head/glove/shoes/hair subparts and boss/monster rigs.
pub fn select_character_body(
    manifest: &AssetManifest,
    sex_male: bool,
    variant: usize,
) -> Option<&GlbAssetEntry> {
    let prefix = if sex_male { "man_" } else { "woman_" };
    let mut matching: Vec<&GlbAssetEntry> = manifest
        .characters
        .iter()
        .filter(|c| {
            let low = c.name.to_ascii_lowercase();
            low.starts_with(prefix)
                && !low.contains("head")
                && !low.contains("glove")
                && !low.contains("shoes")
                && !low.contains("hair")
                && !low.contains("trousers")
                && !low.contains("monster")
                && !low.contains("boss")
        })
        .collect();
    matching.sort_by_key(|c| (!c.name.contains("clothes"), c.name.as_str()));
    if !matching.is_empty() {
        return Some(matching[variant % matching.len()]);
    }
    manifest.characters.first()
}

/// Selects a modular character part (`"_head"`, `"_glove"`, `"_shoes"`, `"_hair"`) matching `sex_male`.
fn select_character_part<'a>(
    manifest: &'a AssetManifest,
    sex_male: bool,
    part_tag: &str,
    variant: usize,
) -> Option<&'a GlbAssetEntry> {
    let prefix = if sex_male { "man_" } else { "woman_" };
    let mut parts: Vec<&GlbAssetEntry> = manifest
        .characters
        .iter()
        .filter(|c| {
            let low = c.name.to_ascii_lowercase();
            low.starts_with(prefix) && low.contains(part_tag)
        })
        .collect();
    if parts.is_empty() {
        parts = manifest
            .characters
            .iter()
            .filter(|c| c.name.to_ascii_lowercase().contains(part_tag))
            .collect();
    }
    parts.sort_by(|a, b| a.name.cmp(&b.name));
    if parts.is_empty() {
        None
    } else {
        Some(parts[variant % parts.len()])
    }
}

/// Selects a modular head GLB matching `sex_male` from `manifest.characters`.
pub fn select_character_head(
    manifest: &AssetManifest,
    sex_male: bool,
    variant: usize,
) -> Option<&GlbAssetEntry> {
    select_character_part(manifest, sex_male, "_head", variant)
}

/// Selects modular gloves (`*_glove_*`) for `sex_male`.
pub fn select_character_gloves(
    manifest: &AssetManifest,
    sex_male: bool,
    variant: usize,
) -> Option<&GlbAssetEntry> {
    select_character_part(manifest, sex_male, "_glove", variant)
}

/// Selects modular footwear (`*_shoes_*`) for `sex_male`.
pub fn select_character_shoes(
    manifest: &AssetManifest,
    sex_male: bool,
    variant: usize,
) -> Option<&GlbAssetEntry> {
    select_character_part(manifest, sex_male, "_shoes", variant)
}

/// Selects modular hairstyle (`*_hair_*`) for `sex_male`.
pub fn select_character_hair(
    manifest: &AssetManifest,
    sex_male: bool,
    variant: usize,
) -> Option<&GlbAssetEntry> {
    select_character_part(manifest, sex_male, "_hair", variant)
}

/// Selects modular trousers (`*_trousers_*`) for `sex_male` when wearing a shirt-only `*_clothes_*` GLB.
pub fn select_character_trousers(
    manifest: &AssetManifest,
    sex_male: bool,
    variant: usize,
) -> Option<&GlbAssetEntry> {
    select_character_part(manifest, sex_male, "_trousers", variant)
}

/// Returns `true` when `entry` is a handheld weapon/tool (`wq_*`) rather than a crate/turret/bag.
pub fn is_equippable_weapon(entry: &GlbAssetEntry) -> bool {
    entry.name.to_ascii_lowercase().starts_with("wq_")
}

/// Returns indices in `manifest.props` belonging to handheld weapons (`wq_*`).
pub fn selectable_weapon_prop_indices(manifest: &AssetManifest) -> Vec<usize> {
    let mut indices: Vec<usize> = manifest
        .props
        .iter()
        .enumerate()
        .filter(|(_, p)| is_equippable_weapon(p))
        .map(|(i, _)| i)
        .collect();
    if indices.is_empty() && !manifest.props.is_empty() {
        indices.extend(0..manifest.props.len());
    }
    indices
}

/// Selects the equipped handheld weapon/tool GLB from `manifest.props`.
pub fn select_weapon_prop(
    manifest: &AssetManifest,
    prop_idx: usize,
) -> Option<&GlbAssetEntry> {
    if let Some(direct) = manifest.props.get(prop_idx)
        && is_equippable_weapon(direct)
    {
        return Some(direct);
    }
    let indices = selectable_weapon_prop_indices(manifest);
    if indices.is_empty() {
        None
    } else {
        manifest.props.get(indices[prop_idx % indices.len()])
    }
}

/// Maps an equipped weapon prop name to a matching exported WAV file in `assets/audio/`.
pub fn gunshot_audio_for_prop(prop_name: &str) -> &'static str {
    let low = prop_name.to_ascii_lowercase();
    if low.contains("mg4") {
        "audio/MG4_loop_near_end.wav"
    } else if low.contains("cfq") {
        "audio/wq_ump9_1_gunshot_outdoor_loop_end.wav"
    } else if low.contains("sdq") {
        "audio/wq_sdq_2_gunshot.wav"
    } else if low.contains("rpg") || low.contains("c4") {
        "audio/combat_dilei_baozha.wav"
    } else if low.contains("bishou") || low.contains("shifu") {
        "audio/combat_banzhuan_body2.wav"
    } else if low.contains("m4") || low.contains("qbu97") {
        "audio/wq_m92_1_gunshot.wav"
    } else {
        "audio/wq_bq_1_gunshot_outdoor_loop_end.wav"
    }
}

/// Identifies which `SkinColorCfg` tint channel (`HeroSkin.SetSkin` / `CharacterSetSkin.SetColor`)
/// applies to a spawned modular character part subtree.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharacterPartTintKind {
    Skin,
    Hair,
    Cloth,
}

#[derive(Component, Clone, Copy)]
pub struct OriginalMaterialBaseColor(pub Color);

#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub struct AppliedTintSignature(pub i32);

/// Parses a `#RRGGBB` / `#RRGGBBAA` (or 5-digit `#1292F`) hex color string from `SkinColorCfg.json`.
pub fn parse_skin_hex_color(hex: &str) -> Color {
    let trimmed = hex.trim().trim_start_matches('#');
    if trimmed.len() >= 6
        && let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&trimmed[0..2], 16),
            u8::from_str_radix(&trimmed[2..4], 16),
            u8::from_str_radix(&trimmed[4..6], 16),
        )
    {
        return Color::srgb_u8(r, g, b);
    }
    if trimmed.len() == 5 {
        let padded = format!("0{trimmed}");
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&padded[0..2], 16),
            u8::from_str_radix(&padded[2..4], 16),
            u8::from_str_radix(&padded[4..6], 16),
        ) {
            return Color::srgb_u8(r, g, b);
        }
    }
    Color::srgb(0.95, 0.72, 0.58)
}

/// Resolves an `EquipCfg.id` (`20500..=23750`) to its exported GLB entry in `manifest.characters`.
pub fn select_character_part_by_equip_id<'a>(
    manifest: &'a AssetManifest,
    equip_cfgs: &[EquipCfgEntry],
    sex_male: bool,
    equip_id: i32,
) -> Option<&'a GlbAssetEntry> {
    let cfg = equip_cfgs.iter().find(|e| e.id == equip_id)?;
    let raw_path = if !sex_male && !cfg.woman_model_path.is_empty() {
        cfg.woman_model_path.as_str()
    } else {
        cfg.model_path.as_str()
    };
    let stem = raw_path
        .trim()
        .rsplit('/')
        .next()?
        .trim_end_matches(".ab")
        .trim_end_matches(".glb");
    if stem.is_empty() {
        return None;
    }
    manifest
        .characters
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(stem))
}

/// Spawns all 5 modular character GLBs (`body`, `head`, `gloves`, `shoes`, `hair`) under `parent`
/// so all `Bip001` parts align on the same skeleton root cleanly.
pub fn spawn_modular_character(
    parent: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    manifest: &AssetManifest,
    sex_male: bool,
    body_variant: usize,
    head_variant: usize,
    hair_variant: usize,
) {
    let body = select_character_body(manifest, sex_male, body_variant);
    let head = select_character_head(manifest, sex_male, head_variant);
    let gloves = select_character_gloves(manifest, sex_male, body_variant);
    let shoes = select_character_shoes(manifest, sex_male, body_variant);
    let hair = select_character_hair(manifest, sex_male, hair_variant);
    let need_trousers = body.is_some_and(|b| !b.name.to_ascii_lowercase().contains("liantifu"));
    let trousers = if need_trousers {
        select_character_trousers(manifest, sex_male, body_variant)
    } else {
        None
    };

    for (label, tint_kind, opt_entry) in [
        ("CharPart_Body", CharacterPartTintKind::Cloth, body),
        ("CharPart_Trousers", CharacterPartTintKind::Cloth, trousers),
        ("CharPart_Head", CharacterPartTintKind::Skin, head),
        ("CharPart_Gloves", CharacterPartTintKind::Skin, gloves),
        ("CharPart_Shoes", CharacterPartTintKind::Cloth, shoes),
        ("CharPart_Hair", CharacterPartTintKind::Hair, hair),
    ] {
        if let Some(entry) = opt_entry {
            parent.spawn((
                WorldAssetRoot(asset_server.load(format!("{}#Scene0", entry.asset_path))),
                Name::new(label),
                tint_kind,
            ));
        }
    }
}

/// Spawns modular character parts resolving `EstablishRole` + `EquipCfg` IDs from `profile`,
/// falling back gracefully to variant index selection if an asset bundle is absent.
pub fn spawn_modular_character_from_profile(
    parent: &mut ChildSpawnerCommands,
    asset_server: &AssetServer,
    manifest: &AssetManifest,
    configs: &GameConfigs,
    profile: &PlayerProfileSave,
) {
    let sex_male = profile.sex_male;
    let body = select_character_body(manifest, sex_male, profile.body_variant_idx);
    let head = select_character_part_by_equip_id(
        manifest,
        &configs.equip_cfgs,
        sex_male,
        profile.face_item_id,
    )
    .or_else(|| select_character_head(manifest, sex_male, profile.head_variant_idx));
    let hair = select_character_part_by_equip_id(
        manifest,
        &configs.equip_cfgs,
        sex_male,
        profile.head_item_id,
    )
    .or_else(|| select_character_hair(manifest, sex_male, profile.hair_variant_idx));
    let gloves = select_character_gloves(manifest, sex_male, profile.body_variant_idx);
    let shoes = select_character_shoes(manifest, sex_male, profile.body_variant_idx);
    let need_trousers = body.is_some_and(|b| !b.name.to_ascii_lowercase().contains("liantifu"));
    let trousers = if need_trousers {
        select_character_trousers(manifest, sex_male, profile.body_variant_idx)
    } else {
        None
    };

    for (label, tint_kind, opt_entry) in [
        ("CharPart_Body", CharacterPartTintKind::Cloth, body),
        ("CharPart_Trousers", CharacterPartTintKind::Cloth, trousers),
        ("CharPart_Head", CharacterPartTintKind::Skin, head),
        ("CharPart_Gloves", CharacterPartTintKind::Skin, gloves),
        ("CharPart_Shoes", CharacterPartTintKind::Cloth, shoes),
        ("CharPart_Hair", CharacterPartTintKind::Hair, hair),
    ] {
        if let Some(entry) = opt_entry {
            parent.spawn((
                WorldAssetRoot(asset_server.load(format!("{}#Scene0", entry.asset_path))),
                Name::new(label),
                tint_kind,
            ));
        }
    }
}

/// Applies `SkinColorCfg` `_ChangeColor` tinting (`HeroSkin.SetSkin` + `CharacterSetSkin.SetColor`)
/// to spawned `CharPart_Head`, `CharPart_Gloves`, and `CharPart_Hair` mesh materials.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn apply_character_skin_and_hair_tints(
    mut commands: Commands,
    configs: Res<GameConfigs>,
    profile: Res<PlayerProfileSave>,
    part_roots: Query<(Entity, &CharacterPartTintKind)>,
    children_q: Query<&Children>,
    mut mesh_mats: Query<(
        Entity,
        &mut MeshMaterial3d<StandardMaterial>,
        Option<&OriginalMaterialBaseColor>,
        Option<&AppliedTintSignature>,
    )>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let skin_tint = configs
        .skin_color_by_id(profile.skin_color_id)
        .map(|s| {
            let hex = if s.show_color.is_empty() {
                &s.color1
            } else {
                &s.show_color
            };
            parse_skin_hex_color(hex)
        })
        .unwrap_or(Color::WHITE);
    let hair_tint = configs
        .skin_color_by_id(profile.hair_color_id)
        .map(|s| {
            let hex = if s.show_color.is_empty() {
                &s.color1
            } else {
                &s.show_color
            };
            parse_skin_hex_color(hex)
        })
        .unwrap_or(Color::WHITE);

    for (root_e, &kind) in &part_roots {
        let (target_id, tint_col) = match kind {
            CharacterPartTintKind::Skin => (profile.skin_color_id, skin_tint),
            CharacterPartTintKind::Hair => (profile.hair_color_id, hair_tint),
            CharacterPartTintKind::Cloth => continue,
        };
        let mut stack = vec![root_e];
        while let Some(e) = stack.pop() {
            if let Ok((mesh_e, mut mat_h, orig_opt, sig_opt)) = mesh_mats.get_mut(e)
                && sig_opt.is_none_or(|sig| sig.0 != target_id)
                && let Some(existing_mat) = materials.get(&mat_h.0).cloned()
            {
                let base = orig_opt.map(|o| o.0).unwrap_or(existing_mat.base_color);
                let b_srgba = base.to_srgba();
                let t_srgba = tint_col.to_srgba();
                // Blend 65% original material albedo with 35% swatch tint so facial/hair
                // texture detail stays crisp while clearly reflecting the chosen swatch.
                let mixed = Color::srgba(
                    (b_srgba.red * 0.60 + t_srgba.red * 0.40).clamp(0.0, 1.0),
                    (b_srgba.green * 0.60 + t_srgba.green * 0.40).clamp(0.0, 1.0),
                    (b_srgba.blue * 0.60 + t_srgba.blue * 0.40).clamp(0.0, 1.0),
                    b_srgba.alpha,
                );
                let mut cloned = existing_mat;
                cloned.base_color = mixed;
                mat_h.0 = materials.add(cloned);
                if orig_opt.is_none() {
                    commands
                        .entity(mesh_e)
                        .insert((OriginalMaterialBaseColor(base), AppliedTintSignature(target_id)));
                } else {
                    commands
                        .entity(mesh_e)
                        .insert(AppliedTintSignature(target_id));
                }
            }
            if let Ok(ch) = children_q.get(e) {
                stack.extend(ch.iter());
            }
        }
    }
}

/// Procedural skeletal locomotion & relaxed-stance state attached to modular `Bip001` actor roots.
#[derive(Component, Clone, Copy, Debug)]
pub struct CharacterAnimDriver {
    pub state: AnimState,
    pub phase: f32,
    pub speed_norm: f32,
    pub has_weapon: bool,
}

impl Default for CharacterAnimDriver {
    fn default() -> Self {
        Self {
            state: AnimState::Idle,
            phase: 0.0,
            speed_norm: 0.0,
            has_weapon: true,
        }
    }
}

#[derive(Clone, Copy)]
enum BipBoneRole {
    LUpperArm,
    LForearm,
    RUpperArm,
    RForearm,
    LThigh,
    LCalf,
    RThigh,
    RCalf,
    Spine1,
}

fn classify_bip001_bone(name: &str) -> Option<BipBoneRole> {
    match name {
        "Bip001 L UpperArm" => Some(BipBoneRole::LUpperArm),
        "Bip001 L Forearm" | "Bip001 L ForeTwist" => Some(BipBoneRole::LForearm),
        "Bip001 R UpperArm" => Some(BipBoneRole::RUpperArm),
        "Bip001 R Forearm" | "Bip001 R ForeTwist" => Some(BipBoneRole::RForearm),
        "Bip001 L Thigh" => Some(BipBoneRole::LThigh),
        "Bip001 L Calf" => Some(BipBoneRole::LCalf),
        "Bip001 R Thigh" => Some(BipBoneRole::RThigh),
        "Bip001 R Calf" => Some(BipBoneRole::RCalf),
        "Bip001 Spine1" => Some(BipBoneRole::Spine1),
        _ => None,
    }
}

/// Cached rest-pose orientation and geometrically derived swing/tuck axes for one `Bip001` limb bone.
#[derive(Component)]
pub struct Bip001RestPose {
    rest_local: Quat,
    swing_axis_local: Vec3,
    tuck_axis_local: Vec3,
    role: BipBoneRole,
    actor_root: Entity,
}

/// Marker inserted on actor roots driven by baked glTF clip players (e.g., Port Boss) so
/// [`animate_bip001_rigs`] leaves their authored animations untouched.
#[derive(Component)]
pub struct UsesBakedGltfAnim;

/// Procedural bone solver keeping all modular character parts (`clothes`, `gloves`, `shoes`, `hair`)
/// relaxed out of A-pose and animated in lockstep during Idle, Walk, Run, Jump, and Attack.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn animate_bip001_rigs(
    mut commands: Commands,
    time: Res<Time>,
    mut actors_q: Query<
        (Entity, &GlobalTransform, &mut CharacterAnimDriver),
        Without<UsesBakedGltfAnim>,
    >,
    children_q: Query<&Children>,
    names_q: Query<&Name>,
    globs_q: Query<&GlobalTransform>,
    unbound_bones_q: Query<
        (&Name, &Transform, &GlobalTransform),
        (Without<Bip001RestPose>, Without<CharacterAnimDriver>),
    >,
    mut driven_bones_q: Query<(&mut Transform, &Bip001RestPose)>,
) {
    let dt = time.delta_secs().min(0.05);

    for (actor_entity, root_gt, mut driver) in &mut actors_q {
        let rate = match driver.state {
            AnimState::Idle => 1.8,
            AnimState::Walk => 6.8 * driver.speed_norm.max(0.55),
            AnimState::Run => 10.2 * driver.speed_norm.max(0.75),
            AnimState::Jump => 3.5,
            AnimState::Attack => 12.0,
        };
        driver.phase = (driver.phase + dt * rate) % std::f32::consts::TAU;

        let mut stack = vec![actor_entity];
        let mut descendants = Vec::new();
        let mut has_unbound = false;
        while let Some(e) = stack.pop() {
            descendants.push(e);
            if e != actor_entity
                && let Ok((name, _, _)) = unbound_bones_q.get(e)
                && classify_bip001_bone(name.as_str()).is_some()
            {
                has_unbound = true;
            }
            if let Ok(ch) = children_q.get(e) {
                for c in ch.iter() {
                    stack.push(c);
                }
            }
        }
        if !has_unbound {
            continue;
        }

        let root_rot_inv = root_gt.rotation().inverse();
        let to_root = |pos: Vec3| root_rot_inv * (pos - root_gt.translation());

        let (mut l_thigh, mut r_thigh, mut l_foot, mut l_toe) = (None, None, None, None);
        let (mut l_upper, mut r_upper) = (None, None);

        for &e in &descendants {
            let (Ok(name), Ok(gt)) = (names_q.get(e), globs_q.get(e)) else {
                continue;
            };
            let pos = to_root(gt.translation());
            match name.as_str() {
                "Bip001 L Thigh" if l_thigh.is_none() && (0.30..=1.85).contains(&pos.y) => {
                    l_thigh = Some(pos);
                }
                "Bip001 R Thigh" if r_thigh.is_none() && (0.30..=1.85).contains(&pos.y) => {
                    r_thigh = Some(pos);
                }
                "Bip001 L Foot" if l_foot.is_none() && pos.y.abs() < 0.45 => l_foot = Some(pos),
                "Bip001 L Toe0" if l_toe.is_none() && pos.y.abs() < 0.45 => l_toe = Some(pos),
                "Bip001 L UpperArm" if l_upper.is_none() && (0.30..=1.85).contains(&pos.y) => {
                    l_upper = Some(pos);
                }
                "Bip001 R UpperArm" if r_upper.is_none() && (0.30..=1.85).contains(&pos.y) => {
                    r_upper = Some(pos);
                }
                _ => {}
            }
        }

        // Wait until at least one thigh pair has been propagated into GlobalTransform.
        let Some((lt, rt)) = l_thigh.zip(r_thigh) else {
            continue;
        };
        if (rt - lt).length_squared() < 0.001 {
            continue;
        }

        let right_root = (rt - lt).with_y(0.0).normalize_or(Vec3::X);
        let fwd_root = l_foot
            .zip(l_toe)
            .map(|(f, t)| (t - f).with_y(0.0).normalize_or(Vec3::Z))
            .unwrap_or_else(|| Vec3::Y.cross(right_root).normalize_or(Vec3::Z));
        let swing_axis_root = fwd_root.cross(Vec3::Y).normalize_or(right_root);

        // Shoulder-to-shoulder outward direction never changes when arms rotate, making late-loaded
        // modular glove/shoe GLBs compute the identical tuck axis as the body rig.
        let (l_tuck_axis_root, r_tuck_axis_root) = if let (Some(lu), Some(ru)) = (l_upper, r_upper) {
            let left_out = (lu - ru).with_y(0.0).normalize_or(-right_root);
            let lt_ax = left_out.cross(Vec3::NEG_Y).normalize_or(fwd_root);
            let rt_ax = (-left_out).cross(Vec3::NEG_Y).normalize_or(-fwd_root);
            (lt_ax, rt_ax)
        } else {
            (fwd_root, -fwd_root)
        };

        for &e in &descendants {
            if e == actor_entity {
                continue;
            }
            let Ok((name, tf, gt)) = unbound_bones_q.get(e) else {
                continue;
            };
            let Some(role) = classify_bip001_bone(name.as_str()) else {
                continue;
            };
            // Ensure this specific modular part has already propagated its first GlobalTransform!
            let pos = to_root(gt.translation());
            if !(0.25..=1.85).contains(&pos.y) {
                continue;
            }
            let b2r_inv = (root_rot_inv * gt.rotation()).inverse();
            let tuck_root = match role {
                BipBoneRole::LUpperArm => l_tuck_axis_root,
                BipBoneRole::RUpperArm => r_tuck_axis_root,
                _ => fwd_root,
            };
            commands.entity(e).insert(Bip001RestPose {
                rest_local: tf.rotation,
                swing_axis_local: (b2r_inv * swing_axis_root).normalize_or(Vec3::X),
                tuck_axis_local: (b2r_inv * tuck_root).normalize_or(Vec3::Z),
                role,
                actor_root: actor_entity,
            });
        }
    }

    for (mut tf, pose) in &mut driven_bones_q {
        let Ok((_, _, driver)) = actors_q.get(pose.actor_root) else {
            continue;
        };
        let (swing, tuck) = sample_bip_angles(*driver, pose.role);
        let swing_q = Quat::from_axis_angle(pose.swing_axis_local, swing);
        let tuck_q = Quat::from_axis_angle(pose.tuck_axis_local, tuck);
        tf.rotation = pose.rest_local * swing_q * tuck_q;
    }
}

fn sample_bip_angles(driver: CharacterAnimDriver, role: BipBoneRole) -> (f32, f32) {
    let s = driver.phase.sin();
    match driver.state {
        AnimState::Idle => match role {
            BipBoneRole::LUpperArm => (
                if driver.has_weapon { 0.28 } else { 0.05 + 0.025 * s },
                0.84,
            ),
            BipBoneRole::LForearm => (if driver.has_weapon { 0.56 } else { 0.30 }, 0.0),
            BipBoneRole::RUpperArm => (
                if driver.has_weapon { 0.46 + 0.02 * s } else { 0.05 - 0.025 * s },
                0.80,
            ),
            BipBoneRole::RForearm => (if driver.has_weapon { 0.68 } else { 0.28 }, 0.0),
            BipBoneRole::LThigh => (0.04, 0.0),
            BipBoneRole::RThigh => (-0.04, 0.0),
            BipBoneRole::LCalf | BipBoneRole::RCalf => (-0.06, 0.0),
            BipBoneRole::Spine1 => (-0.04 - 0.02 * s, 0.0),
        },
        AnimState::Walk | AnimState::Run => {
            let amp = if driver.state == AnimState::Run { 0.56 } else { 0.34 };
            let leg_l = amp * s;
            let leg_r = -amp * s;
            match role {
                BipBoneRole::LThigh => (leg_l, 0.0),
                BipBoneRole::LCalf => (-(leg_l.max(0.0) * 0.55 + (-leg_l).max(0.0) * 0.85) - 0.08, 0.0),
                BipBoneRole::RThigh => (leg_r, 0.0),
                BipBoneRole::RCalf => (-(leg_r.max(0.0) * 0.55 + (-leg_r).max(0.0) * 0.85) - 0.08, 0.0),
                BipBoneRole::LUpperArm => (
                    if driver.has_weapon { 0.30 - 0.14 * s } else { -amp * 0.72 * s + 0.06 },
                    0.66,
                ),
                BipBoneRole::LForearm => (
                    if driver.has_weapon { 0.60 } else { 0.36 + 0.15 * (-s).max(0.0) },
                    0.0,
                ),
                BipBoneRole::RUpperArm => (
                    if driver.has_weapon { 0.48 + 0.06 * s } else { amp * 0.72 * s + 0.06 },
                    0.64,
                ),
                BipBoneRole::RForearm => (
                    if driver.has_weapon { 0.72 } else { 0.36 + 0.15 * s.max(0.0) },
                    0.0,
                ),
                BipBoneRole::Spine1 => (
                    if driver.state == AnimState::Run { -0.14 } else { -0.06 },
                    0.0,
                ),
            }
        }
        AnimState::Jump => match role {
            BipBoneRole::LUpperArm | BipBoneRole::RUpperArm => (0.42, 0.48),
            BipBoneRole::LForearm | BipBoneRole::RForearm => (0.45, 0.0),
            BipBoneRole::LThigh => (0.36, 0.0),
            BipBoneRole::LCalf => (-0.68, 0.0),
            BipBoneRole::RThigh => (-0.22, 0.0),
            BipBoneRole::RCalf => (-0.36, 0.0),
            BipBoneRole::Spine1 => (-0.10, 0.0),
        },
        AnimState::Attack => match role {
            BipBoneRole::LUpperArm => (0.32 - 0.15 * s, 0.64),
            BipBoneRole::LForearm => (0.58, 0.0),
            BipBoneRole::RUpperArm => (
                if driver.has_weapon { 0.56 + 0.16 * s } else { 0.84 + 0.52 * s },
                0.56,
            ),
            BipBoneRole::RForearm => (
                if driver.has_weapon { 0.78 - 0.10 * s } else { 0.45 + 0.25 * s },
                0.0,
            ),
            BipBoneRole::LThigh => (0.14, 0.0),
            BipBoneRole::RThigh => (-0.18, 0.0),
            BipBoneRole::LCalf | BipBoneRole::RCalf => (-0.12, 0.0),
            BipBoneRole::Spine1 => (-0.12 - 0.05 * s, 0.0),
        },
    }
}
