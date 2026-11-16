//! Phase 3 in-game survival hotbar, crafting recipes, building placement/upgrade helpers,
//! and APK building-part upgrade tier visibility (`lv1..lv4`) + hinged door toggle logic.

use bevy::prelude::*;

use crate::world_physics::BuildPieceKind;

/// Action/item bound to each `[1..0]` slot in the bottom survival hotbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotbarItem {
    Rifle,
    Hatchet,
    Knife,
    Build(BuildPieceKind),
}

impl HotbarItem {
    pub const SLOTS: [Self; 10] = [
        Self::Rifle,
        Self::Hatchet,
        Self::Knife,
        Self::Build(BuildPieceKind::Foundation),
        Self::Build(BuildPieceKind::Wall),
        Self::Build(BuildPieceKind::Roof),
        Self::Build(BuildPieceKind::DoorFrame),
        Self::Build(BuildPieceKind::Door),
        Self::Build(BuildPieceKind::Ramp),
        Self::Build(BuildPieceKind::Workbench),
    ];

    pub fn key_hint(idx: usize) -> &'static str {
        match idx {
            0 => "1",
            1 => "2",
            2 => "3",
            3 => "4",
            4 => "5",
            5 => "6",
            6 => "7",
            7 => "8",
            8 => "9",
            _ => "0",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Rifle => "Rifle",
            Self::Hatchet => "Axe",
            Self::Knife => "Knife",
            Self::Build(kind) => kind.label(),
        }
    }
}

/// Interactive crafting recipe (`[Tab]` menu) wired to `OP_CRAFT_RECIPE`.
#[derive(Debug, Clone, Copy)]
pub struct CraftRecipeDesc {
    pub id: u8,
    pub title: &'static str,
    pub cost_text: &'static str,
}

pub const CRAFT_RECIPES: &[CraftRecipeDesc] = &[
    CraftRecipeDesc {
        id: 0,
        title: "Craft 15x Rifle Ammo",
        cost_text: "10 Wood + 5 Stone",
    },
    CraftRecipeDesc {
        id: 1,
        title: "Craft 1x Bandage (+35 HP)",
        cost_text: "10 Fiber",
    },
    CraftRecipeDesc {
        id: 2,
        title: "Refine 20x Stone Blocks",
        cost_text: "15 Wood",
    },
    CraftRecipeDesc {
        id: 3,
        title: "Smelt 12x Iron Ingots",
        cost_text: "20 Stone",
    },
    CraftRecipeDesc {
        id: 4,
        title: "Craft Heavy Ammo Pack (+30 Ammo)",
        cost_text: "15 Iron + 10 Wood",
    },
    CraftRecipeDesc {
        id: 5,
        title: "Synthesize Generator Fuel (+350 Fuel)",
        cost_text: "15 Wood + 10 Iron + 10 Fiber",
    },
    CraftRecipeDesc {
        id: 6,
        title: "Repair Nearest Vehicle (+220 HP)",
        cost_text: "20 Wood + 15 Iron",
    },
];

/// Client-side survival HUD, crosshair ray target, build ghost, and cooldown state.
#[derive(Resource)]
pub struct SurvivalSessionState {
    pub active_slot: usize,
    pub custom_build_override: Option<BuildPieceKind>,
    pub build_rot_steps: u8,
    pub craft_menu_open: bool,
    pub hovered_prompt: String,
    pub active_chunk: String,
    pub attack_cooldown: f32,
    pub ghost_valid: bool,
    pub ghost_pos: Vec3,
    pub ghost_rot_steps: u8,
    pub ghost_kind: BuildPieceKind,
    pub aimed_structure_id: Option<u32>,
    pub aimed_loot_drop_id: Option<u16>,
    pub aimed_vehicle_id: Option<u16>,
    pub equipped_prop_override: Option<usize>,
    pub driving_vehicle_id: u16,
    pub active_vehicle_kind: Option<crate::world_physics::VehicleKind>,
    pub active_vehicle_speed: f32,
    pub active_vehicle_fuel_pct: u8,
    pub active_vehicle_hp: u16,
    pub active_vehicle_max_hp: u16,
    pub base_power_pct: u8,
    pub status_banner: String,
    pub status_banner_secs: f32,
}

impl Default for SurvivalSessionState {
    fn default() -> Self {
        Self {
            active_slot: 0,
            custom_build_override: None,
            build_rot_steps: 0,
            craft_menu_open: false,
            hovered_prompt:
                "LMB: Fire/Gather | [4-0/T/G] Build | [F] Board/Use | [Tab] Craft | [F5] Save Base"
                    .to_owned(),
            active_chunk: "A-57".to_owned(),
            attack_cooldown: 0.0,
            ghost_valid: false,
            ghost_pos: Vec3::ZERO,
            ghost_rot_steps: 0,
            ghost_kind: BuildPieceKind::Foundation,
            aimed_structure_id: None,
            aimed_loot_drop_id: None,
            aimed_vehicle_id: None,
            equipped_prop_override: None,
            driving_vehicle_id: 0,
            active_vehicle_kind: None,
            active_vehicle_speed: 0.0,
            active_vehicle_fuel_pct: 0,
            active_vehicle_hp: 0,
            active_vehicle_max_hp: 0,
            base_power_pct: 0,
            status_banner: String::new(),
            status_banner_secs: 0.0,
        }
    }
}

impl SurvivalSessionState {
    pub fn active_item(&self) -> HotbarItem {
        if let Some(kind) = self.custom_build_override {
            HotbarItem::Build(kind)
        } else {
            HotbarItem::SLOTS[self.active_slot % HotbarItem::SLOTS.len()]
        }
    }
}

const PREFAB_ROOT_NAMES: &[&str] = &[
    "diji_lv1",
    "qiangbi_lv1",
    "menkuang_lv1",
    "danshanmen_lv1",
    "tianhuaban_lv1",
    "dijitaijie_lv1",
    "gongzuotai_lv1",
    "fadianji",
    "xudianchizhu_01",
    "diancipao_01",
];

/// Applies the authoritative upgrade tier (`1..=4`) and open-door hinge state directly across
/// child meshes inside exported APK `assets/models/*_lv1.glb` building prefabs, keeping prefab
/// root nodes visible while hiding collider debug meshes (`Cube`, `collider_*`, `*_LOD2`).
pub fn apply_structure_tier_and_door_pose(
    root: Entity,
    tier: u8,
    is_open: bool,
    children_q: &Query<&Children>,
    names_q: &Query<&Name>,
    vis_q: &mut Query<&mut Visibility>,
    tf_q: &mut Query<&mut Transform>,
) {
    let want_t = tier.clamp(1, 4);
    let want_model = format!("model_{want_t}");
    let want_lv = format!("_lv{want_t}");
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if e != root
            && let Ok(name) = names_q.get(e)
        {
            let low = name.as_str().to_ascii_lowercase();
            let is_prefab_root = PREFAB_ROOT_NAMES.contains(&low.as_str()) || low == "lvp";
            if !is_prefab_root {
                let is_helper = low.starts_with("cube")
                    || low.contains("collider")
                    || low == "boxinfo"
                    || low == "shield"
                    || low.contains("_lod");
                if is_helper {
                    if let Ok(mut vis) = vis_q.get_mut(e) {
                        *vis = Visibility::Hidden;
                    }
                } else if let Some(rest) = low.strip_prefix("model_")
                    && rest.chars().all(|c| c.is_ascii_digit())
                {
                    if let Ok(mut vis) = vis_q.get_mut(e) {
                        *vis = if low == want_model {
                            Visibility::Inherited
                        } else {
                            Visibility::Hidden
                        };
                    }
                } else {
                    let has_tier_suffix = low.contains("_lv1")
                        || low.contains("_lv2")
                        || low.contains("_lv3")
                        || low.contains("_lv4")
                        || low.contains("_lv5");
                    if has_tier_suffix {
                        let keep = low.contains(&want_lv);
                        if let Ok(mut vis) = vis_q.get_mut(e) {
                            *vis = if keep {
                                Visibility::Inherited
                            } else {
                                Visibility::Hidden
                            };
                        }
                        if low.contains("_men_lv")
                            && let Ok(mut tf) = tf_q.get_mut(e)
                        {
                            tf.rotation = Quat::from_rotation_y(if is_open { 1.38 } else { 0.0 });
                        }
                    }
                }
            }
        }
        if let Ok(children) = children_q.get(e) {
            for child in children.iter() {
                stack.push(child);
            }
        }
    }
}
