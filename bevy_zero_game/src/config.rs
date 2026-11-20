//! APK configuration & localization loader (`version.txt`, `ResConfig.txt`, `serverList.xml`,
//! and decoded `.oc` JSON tables).
//!
//! Large JSON localization tables (such as `cfg.MultiLanguageCfg.json` at ~40k rows) load
//! asynchronously on Bevy's [`IoTaskPool`] and advance [`ConfigLoadProgress`] smoothly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task, block_on, poll_once};
use serde::{Deserialize, Serialize};

use crate::states::AppState;

/// Exported 3D glTF/GLB asset descriptor in `assets/manifest.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct GlbAssetEntry {
    pub name: String,
    pub asset_path: String,
    pub source_bundle: String,
    pub node_count: usize,
    pub mesh_count: usize,
    pub material_count: usize,
    pub texture_count: usize,
    pub skin_count: usize,
    pub animation_count: usize,
    pub triangle_count: usize,
    pub animation_names: Vec<String>,
}

/// Exported audio/font/sprite entry in `assets/manifest.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MediaEntry {
    pub name: String,
    pub asset_path: String,
    pub kind: String,
}

/// Exported Unity uGUI panel summary in `assets/manifest.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UiPanelEntry {
    pub name: String,
    pub asset_path: String,
    pub source_bundle: String,
    pub reference_width: f32,
    pub reference_height: f32,
    pub node_count: usize,
    pub graphic_count: usize,
    pub text_count: usize,
    #[serde(default)]
    pub resolved_texture_count: usize,
    #[serde(default)]
    pub missing_texture_count: usize,
}

/// Root `assets/manifest.json` resource shared across `InGame` and `DevSandbox`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default, Resource)]
pub struct AssetManifest {
    pub project_name: String,
    pub unity_version: String,
    pub scenes: Vec<GlbAssetEntry>,
    pub characters: Vec<GlbAssetEntry>,
    pub props: Vec<GlbAssetEntry>,
    #[serde(default)]
    pub build_parts: Vec<GlbAssetEntry>,
    #[serde(default)]
    pub world_props: Vec<GlbAssetEntry>,
    pub audio: Vec<MediaEntry>,
    pub fonts: Vec<MediaEntry>,
    pub ui_sprites: Vec<MediaEntry>,
    #[serde(default)]
    pub ui_panels: Vec<UiPanelEntry>,
}

/// Generic wrapper matching `devx_cli decode-oc` JSON output schema.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DecodedOcFile<T> {
    #[serde(rename = "type", default)]
    pub type_name: String,
    #[serde(default)]
    pub singleton: bool,
    #[serde(default)]
    pub count: usize,
    #[serde(default)]
    pub records: Vec<T>,
}

/// Parsed `assets/config/version.txt`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    #[serde(default)]
    pub str_v: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub code_version: String,
}

/// Parsed key-value configuration from `assets/config/ResConfig.txt`.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct ResConfigData {
    pub res_root_path: String,
    pub is_open_res_divide: bool,
    #[allow(dead_code)]
    pub entries: HashMap<String, String>,
}

/// Gameplay mode bit parsed from `serverList.xml` (`type: 0-Building, 1-100-Player`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerModeType {
    #[default]
    Building,
    BattleRoyale100,
    Unknown(u32),
}

impl ServerModeType {
    pub fn from_code(code: u32) -> Self {
        match code {
            0 => Self::Building,
            1 => Self::BattleRoyale100,
            other => Self::Unknown(other),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Building => "Building [0]",
            Self::BattleRoyale100 => "100-Player [1]",
            Self::Unknown(_) => "Custom",
        }
    }
}

/// Server health tag parsed from `serverList.xml` (`status: 0-Rec, 1-Good, 2-Full, 3-Maint`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerStatus {
    #[default]
    Recommended,
    Smooth,
    Full,
    Maintenance,
}

impl ServerStatus {
    pub fn from_code(code: u32) -> Self {
        match code {
            0 => Self::Recommended,
            1 => Self::Smooth,
            2 => Self::Full,
            _ => Self::Maintenance,
        }
    }

    pub fn cn_key(self) -> &'static str {
        match self {
            Self::Recommended => "推荐",
            Self::Smooth => "流畅",
            Self::Full => "爆满",
            Self::Maintenance => "维护",
        }
    }
}

/// One server endpoint listed in `serverList.xml` (plus built-in local UDP loopback option).
#[derive(Debug, Clone)]
pub struct ServerEntry {
    pub id: u32,
    pub mode: ServerModeType,
    pub name: String,
    pub gate_url: String,
    pub port: u16,
    pub status: ServerStatus,
    pub is_local_loopback: bool,
}

/// Server region tab listed in `serverList.xml`.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RegionEntry {
    pub id: u32,
    pub name: String,
    pub server_list_raw: String,
}

/// Parsed server list from `assets/config/serverList.xml`.
#[derive(Debug, Clone, Default)]
pub struct ServerListData {
    pub servers: Vec<ServerEntry>,
    pub regions: Vec<RegionEntry>,
}

/// Native WarChoice mode metadata; the client selects visibility and order from this table.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ZhanJuCfgEntry {
    pub id: u8,
    pub name: String,
    pub desc: String,
    pub need_show: bool,
    pub show_in_competition_selection_panel: bool,
    #[serde(rename = "sortID")]
    pub sort_id: i32,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ZhanjuRegionCfgEntry {
    pub id: i32,
    #[serde(rename = "nameCN")]
    pub name_cn: String,
    #[serde(rename = "nameEN")]
    pub name_en: String,
    pub oversea: bool,
    #[serde(rename = "isHeroServer")]
    pub is_hero_server: bool,
    #[serde(rename = "type")]
    pub region_type: i32,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BattleMapCfgEntry {
    pub id: i32,
    pub name: String,
    pub battle_ab_name: String,
    pub mditu_scene_name: String,
}

/// Row from `cfg.MultiLanguageIndexCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MultiLanguageIndexEntry {
    pub id: i32,
    pub index: i32,
    pub language: String,
    #[serde(default)]
    pub gme_language_type: String,
    #[serde(default)]
    pub notice_url_suffix: String,
}

/// Row from `cfg.MultiLanguageCfg.json`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MultiLanguageRow {
    pub id: String,
    #[serde(default)]
    pub languages: Vec<String>,
}

/// Row from `cfg.MainBgCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MainBgEntry {
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub scene_path: String,
    #[serde(default)]
    pub audio_path: String,
    #[serde(default)]
    pub is_default: bool,
}

/// Row from `cfg.LoadDescCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LoadDescEntry {
    pub mode: i32,
    pub texture_id: i32,
    pub bg_path: String,
    pub is_left: bool,
}

/// Row from `cfg.LoadPanelCfg.json` (`LoadingPanel` / `CarDiyLoadingPanel` backdrop rotation).
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct LoadPanelCfgEntry {
    pub id: i32,
    #[serde(default)]
    pub texture: String,
}

/// Row from `cfg.Strings.json` backing APK `Utils.GetString(id, args)`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct StringCfgEntry {
    pub id: i32,
    #[serde(default)]
    pub content: String,
}

/// Row from `cfg.BattleLoadingTipsCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BattleLoadingTipEntry {
    pub id: i32,
    #[serde(default)]
    pub map_id: i32,
    #[serde(default)]
    pub mode_id: i32,
    #[serde(default)]
    pub show_time: f32,
    #[serde(default)]
    pub weight: f32,
    #[serde(default)]
    pub content: String,
}

/// Subset of fields from `cfg.CloseWeaponCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CloseWeaponEntry {
    pub id: i32,
    #[serde(default)]
    pub base_damage: i32,
    #[serde(default)]
    pub btn_icon: String,
}

/// Per-hair-style color swatch ID list in `cfg.EstablishRole.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct HairColorGroup {
    #[serde(default)]
    pub colorlist: Vec<i32>,
}

/// Starter character preset row from `cfg.EstablishRole.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EstablishRoleEntry {
    pub id: i32,
    #[serde(default)]
    pub sex: bool,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub head: Vec<i32>,
    #[serde(default)]
    pub face: Vec<i32>,
    #[serde(default)]
    pub skin_color: Vec<i32>,
    #[serde(default)]
    pub hair_color: Vec<HairColorGroup>,
    #[serde(rename = "Clothes", default)]
    pub clothes: Vec<i32>,
    #[serde(rename = "Pants", default)]
    pub pants: Vec<i32>,
    #[serde(rename = "Shoes", default)]
    pub shoes: Vec<i32>,
    #[serde(rename = "Glove", default)]
    pub glove: Vec<i32>,
}

/// Firearm combat stat row from `cfg.GunCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GunCfgEntry {
    pub id: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub gun_type_name: String,
    #[serde(default)]
    pub factors: Vec<f32>,
    #[serde(default)]
    pub sound_setting_path: String,
    #[serde(default)]
    pub show_in_store: bool,
    #[serde(default)]
    pub main_panel_aim: String,
}

/// Cosmetic fashion entry from `cfg.RoleFashionCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RoleFashionEntry {
    pub fashion_id: i32,
    #[serde(default)]
    pub need_sex: i32,
    #[serde(default)]
    pub model_path: String,
}

/// Portrait head descriptor from `cfg.RoleHeadCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RoleHeadEntry {
    pub id: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub icon: String,
}

/// Skin tone swatch descriptor from `cfg.SkinColorCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SkinColorEntry {
    pub id: i32,
    #[serde(default)]
    pub color1: String,
    #[serde(default)]
    pub color2: String,
    #[serde(default)]
    pub color3: String,
    #[serde(default)]
    pub show_color: String,
}

/// Starter & armor equipment model descriptor from `cfg.EquipCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EquipCfgEntry {
    pub id: i32,
    #[serde(default)]
    pub equip_type: i32,
    #[serde(default)]
    pub model_path: String,
    #[serde(default)]
    pub woman_model_path: String,
}

/// Single name word entry from `cfg.Name1Cfg.json` .. `cfg.Name5Cfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SimpleNameEntry {
    pub id: i32,
    #[serde(default)]
    pub name: String,
}

/// Localized name row from `cfg.MultiLanguageNameCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MultiLanguageNameEntry {
    pub id: i32,
    #[serde(default)]
    pub language_names: Vec<String>,
}

/// Sparse projection of `cfg.ItemCfg.json` used to enrich weapons and inventory items.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ItemCfgEntry {
    pub id: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub model_path: String,
    #[serde(default)]
    pub quality: i32,
    #[serde(rename = "type", default)]
    pub item_type: i32,
    #[serde(default)]
    pub child_type: i32,
    #[serde(default)]
    pub is_pile_able: bool,
    #[serde(default)]
    pub max_pile_num: i32,
    #[serde(default)]
    pub can_use: bool,
    #[serde(default)]
    pub use_time: f32,
    #[serde(default)]
    pub extras: Vec<f32>,
    #[serde(default)]
    pub extrasstring: Vec<String>,
    #[serde(default)]
    pub durability: i32,
}

/// Native ingredient IDs and per-batch quantities in `cfg.DrawingCfg`.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DrawingMaterialEntry {
    pub item_id: i32,
    pub num: i32,
}

/// Complete decoded general-production row; the APK uses signed 32-bit IDs/counts.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DrawingCfgEntry {
    pub id: i32,
    pub name: String,
    pub big_type: i32,
    pub small_type: i32,
    pub is_normal: bool,
    pub is_show_locked: bool,
    pub is_default: bool,
    pub is_civi_normal: bool,
    pub is_civi_show_locked: bool,
    pub is_civi_default: bool,
    pub is_nostalgia: bool,
    pub is_nostalgia_show_locked: bool,
    pub is_nostalgia_default: bool,
    pub is_rare: bool,
    pub level_limit: i32,
    pub need_workbench: i32,
    pub need_workbench_level: i32,
    pub need_civilized_workbench: i32,
    pub need_civilized_workbench_level: i32,
    pub need_time: i32,
    pub material: Vec<DrawingMaterialEntry>,
    pub target_item_id: i32,
    pub once_max_create: i32,
    pub order: i32,
    pub base_number: i32,
    pub product_num: i32,
    pub is_talent: bool,
    pub infor_panel_type: i32,
    pub battle_type_limit: i32,
    pub need_tool_box_level: i32,
    pub costom_need_show: bool,
    pub is_fire_power_normal: bool,
    pub is_fire_power_show_locked: bool,
    pub is_fire_power_default: bool,
}

/// Base vehicle row from `cfg.VehicleCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VehicleCfgEntry {
    pub id: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub ab_path: String,
    #[serde(default)]
    pub max_fuel: f32,
    #[serde(default)]
    pub fuel_consume: f32,
    #[serde(default)]
    pub max_hp: f32,
    #[serde(default)]
    pub max_speed: f32,
}

/// Car handling parameters from `cfg.CarCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CarCfgEntry {
    pub id: i32,
    #[serde(default)]
    pub max_speed: f32,
    #[serde(default)]
    pub max_speed_with_nitrogen: f32,
    #[serde(default)]
    pub steer_angle: f32,
    #[serde(default)]
    pub engine_torque: f32,
}

/// Electric/Tesla turret tuning from `cfg.ElectricTurretCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ElectricTurretCfgEntry {
    pub id: i32,
    #[serde(default)]
    pub check_range: f32,
    #[serde(default)]
    pub fire_range: f32,
    #[serde(default)]
    pub attack_interval_time: i32,
}

/// Battery capacity parameter from `cfg.BatteryCapacityCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BatteryCapacityCfgEntry {
    pub id: i32,
    #[serde(default)]
    pub bat_capacity: f32,
}

/// Quick chat / comms shortcut row from `cfg.ShortCutKeysCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShortCutKeysEntry {
    pub id: i32,
    #[serde(default)]
    pub desc: String,
}

/// PC input key mapping row from `cfg.PCKeyCodeCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PcKeyCodeEntry {
    pub id: i32,
    #[serde(default)]
    pub func_name: String,
    #[serde(default)]
    pub function: String,
    #[serde(default)]
    pub keycode: Vec<i32>,
    #[serde(rename = "type", default)]
    pub key_type: i32,
}

fn default_true() -> bool {
    true
}

/// Lookup table mapping persisted key names to Bevy [`KeyCode`] values and short UI badges.
const KEY_LUT: &[(&str, bevy::prelude::KeyCode, &str)] = &[
    ("KeyF", bevy::prelude::KeyCode::KeyF, "F"),
    ("KeyE", bevy::prelude::KeyCode::KeyE, "E"),
    ("KeyR", bevy::prelude::KeyCode::KeyR, "R"),
    ("KeyQ", bevy::prelude::KeyCode::KeyQ, "Q"),
    ("KeyU", bevy::prelude::KeyCode::KeyU, "U"),
    ("KeyV", bevy::prelude::KeyCode::KeyV, "V"),
    ("KeyC", bevy::prelude::KeyCode::KeyC, "C"),
    ("KeyX", bevy::prelude::KeyCode::KeyX, "X"),
    ("KeyH", bevy::prelude::KeyCode::KeyH, "H"),
    ("KeyT", bevy::prelude::KeyCode::KeyT, "T"),
    ("KeyG", bevy::prelude::KeyCode::KeyG, "G"),
    ("KeyO", bevy::prelude::KeyCode::KeyO, "O"),
    ("KeyP", bevy::prelude::KeyCode::KeyP, "P"),
    ("KeyK", bevy::prelude::KeyCode::KeyK, "K"),
    ("KeyI", bevy::prelude::KeyCode::KeyI, "I"),
    ("KeyJ", bevy::prelude::KeyCode::KeyJ, "J"),
    ("KeyY", bevy::prelude::KeyCode::KeyY, "Y"),
    ("Tab", bevy::prelude::KeyCode::Tab, "Tab"),
    ("F5", bevy::prelude::KeyCode::F5, "F5"),
    ("F6", bevy::prelude::KeyCode::F6, "F6"),
];

/// Remappable action keybindings persisted in [`PlayerProfileSave`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct KeybindProfile {
    pub preset_idx: u8,
    pub interact_key: String,
    pub rotate_key: String,
    pub upgrade_key: String,
    pub craft_key: String,
    pub heal_key: String,
    pub turret_key: String,
    pub generator_key: String,
    pub save_base_key: String,
}

impl Default for KeybindProfile {
    fn default() -> Self {
        Self::preset(0)
    }
}

impl KeybindProfile {
    /// Returns a named keybind layout (`0 = Standard WASD`, `1 = Compact Action`, `2 = Left-Handed`).
    pub fn preset(preset_idx: u8) -> Self {
        match preset_idx % 3 {
            1 => Self {
                preset_idx: 1,
                interact_key: "KeyE".to_owned(),
                rotate_key: "KeyQ".to_owned(),
                upgrade_key: "KeyV".to_owned(),
                craft_key: "KeyC".to_owned(),
                heal_key: "KeyX".to_owned(),
                turret_key: "KeyT".to_owned(),
                generator_key: "KeyG".to_owned(),
                save_base_key: "F5".to_owned(),
            },
            2 => Self {
                preset_idx: 2,
                interact_key: "KeyO".to_owned(),
                rotate_key: "KeyP".to_owned(),
                upgrade_key: "KeyK".to_owned(),
                craft_key: "KeyI".to_owned(),
                heal_key: "KeyJ".to_owned(),
                turret_key: "KeyY".to_owned(),
                generator_key: "KeyU".to_owned(),
                save_base_key: "F6".to_owned(),
            },
            _ => Self {
                preset_idx: 0,
                interact_key: "KeyF".to_owned(),
                rotate_key: "KeyR".to_owned(),
                upgrade_key: "KeyU".to_owned(),
                craft_key: "Tab".to_owned(),
                heal_key: "KeyH".to_owned(),
                turret_key: "KeyT".to_owned(),
                generator_key: "KeyG".to_owned(),
                save_base_key: "F5".to_owned(),
            },
        }
    }

    pub fn keycode(name: &str) -> bevy::prelude::KeyCode {
        KEY_LUT
            .iter()
            .find(|(k, _, _)| *k == name)
            .map(|(_, code, _)| *code)
            .unwrap_or(bevy::prelude::KeyCode::KeyF)
    }

    pub fn short_label(name: &str) -> &str {
        KEY_LUT
            .iter()
            .find(|(k, _, _)| *k == name)
            .map(|(_, _, short)| *short)
            .unwrap_or("?")
    }

    /// Cycles the primary interact/vehicle/refuel key between F -> E -> O.
    pub fn cycle_interact_key(&mut self) {
        const CHOICES: &[&str] = &["KeyF", "KeyE", "KeyO"];
        let cur = CHOICES
            .iter()
            .position(|k| *k == self.interact_key)
            .unwrap_or(0);
        self.interact_key = CHOICES[(cur + 1) % CHOICES.len()].to_owned();
    }
}

/// Serializable placed structure entry for offline/local-host base persistence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedStructureEntry {
    pub kind: u8,
    pub tier: u8,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub yaw_steps: u8,
    pub hp: u16,
    pub max_hp: u16,
    #[serde(default)]
    pub is_open: bool,
    #[serde(default)]
    pub fuel: u16,
}

/// Persistent player base snapshot (`base_save.json`) inspired by APK `DataSaveMgr` / `HomeData`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BaseSaveData {
    pub version: u32,
    pub wood: u32,
    pub stone: u32,
    pub iron: u32,
    pub fiber: u32,
    pub ammo: u32,
    pub bandages: u8,
    pub generator_fuel: u16,
    pub structures: Vec<SavedStructureEntry>,
    pub owner_key: Option<LocalRoleKey>,
    pub inventory: Option<crate::crafting::ItemInventory>,
    pub craft_queue: crate::crafting::CraftQueue,
}

impl Default for BaseSaveData {
    fn default() -> Self {
        Self {
            version: 1,
            wood: 180,
            stone: 90,
            iron: 45,
            fiber: 40,
            ammo: 60,
            bandages: 2,
            generator_fuel: 400,
            structures: Vec::new(),
            owner_key: None,
            inventory: None,
            craft_queue: crate::crafting::CraftQueue::default(),
        }
    }
}

impl BaseSaveData {
    /// Local replacement storage uses the same account/server identity as role snapshots.
    pub fn resolve_role_save_path(account: &str, server_id: u32) -> PathBuf {
        // Fixed FNV-1a filename; the embedded full owner key is also checked before overwriting.
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in account.as_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        Self::resolve_save_path()
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("bases")
            .join(server_id.to_string())
            .join(format!("{hash:016x}.json"))
    }
    /// Returns `~/.local/share/bevy_zero_game/base_save.json` (or `XDG_DATA_HOME` equivalent).
    pub fn resolve_save_path() -> PathBuf {
        PlayerProfileSave::resolve_save_path()
            .parent()
            .map(|dir| dir.join("base_save.json"))
            .unwrap_or_else(|| std::env::temp_dir().join("bevy_zero_game_base_save.json"))
    }

    /// Loads saved base snapshot from disk if present and valid.
    pub fn load_from_disk(path: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(path).ok()?;
        let data = serde_json::from_str::<Self>(&raw).ok()?;
        (data.version == 1 || data.version == 2).then_some(data)
    }

    /// Atomically writes `base_save.json` via temporary sibling + rename.
    pub fn save_atomic(&self, path: &Path) -> Result<(), String> {
        if path.exists() {
            let raw = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
            let previous: Self = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
            if !(1..=2).contains(&previous.version) || previous.owner_key != self.owner_key {
                return Err(
                    "Existing base belongs to another owner or unsupported save version".to_owned(),
                );
            }
        }
        atomic_write_json(path, self)
    }
}

fn atomic_write_json<T: Serialize>(path: &Path, val: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create `{}`: {e}", parent.display()))?;
    }
    let body =
        serde_json::to_string_pretty(val).map_err(|e| format!("failed to serialize JSON: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("failed to write `{}`: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("failed to commit `{}`: {e}", path.display()))
}

/// Resolved armor/weapon catalog entry joining `manifest.props` with APK `.oc` metadata.
#[derive(Debug, Clone, Default)]
pub struct WeaponCatalogEntry {
    pub item_id: i32,
    pub name_cn: String,
    pub category_cn: String,
    pub base_damage: i32,
    pub quality: i32,
    pub sound_hint: String,
}

/// Phase 3 building structure metadata from `cfg.BuildPart.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BuildPartEntry {
    pub id: i32,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", default)]
    pub part_type: i32,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub lv: i32,
    #[serde(default)]
    pub hp: i32,
}

/// Phase 3 tree/harvest node metadata from `cfg.TreeCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TreeCfgEntry {
    pub id: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub hp: i32,
    #[serde(default)]
    pub item_id: i32,
}

/// Phase 3 100m cell index row decoded from `assets/config/scenecell.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SceneCellEntry {
    pub x_pos: f32,
    pub z_pos: f32,
    #[serde(default)]
    pub small_scene_list: Vec<String>,
}

/// Phase 4 automated defense turret metadata decoded from `cfg.TurretCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TurretCfgEntry {
    pub id: i32,
    #[serde(default)]
    pub gun_id: i32,
    #[serde(default)]
    pub check_range: f32,
    #[serde(default)]
    pub fire_range: f32,
    #[serde(default)]
    pub drop_model: String,
}

/// Phase 4 AI monster metadata decoded from `cfg.MonsterCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MonsterCfgEntry {
    pub id: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub hp: i32,
    #[serde(default)]
    pub model_path: String,
    #[serde(default)]
    pub run_speed: f32,
    #[serde(default)]
    pub walk_speed: f32,
    #[serde(default)]
    pub free_move_range: f32,
}

/// Phase 4 Port Boss metadata decoded from `cfg.PortBossMonsterCfg.json`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PortBossMonsterEntry {
    pub id: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub hp: i32,
    #[serde(default)]
    pub shield: i32,
    #[serde(default)]
    pub attack_range: f32,
    #[serde(default)]
    pub model_path: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct SceneCellFile {
    #[serde(default)]
    cells: Vec<SceneCellEntry>,
}

/// Stable local replacement keys. Original server-side role ownership remains external.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalRoleKey {
    pub account_name: String,
    pub server_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocalAccountRoleSave {
    pub account_name: String,
    pub server_id: Option<u32>,
    pub role: LocalRoleProfile,
}

/// Local replacement for a StoreMgr GsBagItem; instance identity is separate from item ID.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LobbyStoreItemSave {
    pub instance_id: i32,
    pub item_id: i32,
    pub count: i32,
}

/// Explicit role snapshot excludes login credentials and global client preferences.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocalRoleProfile {
    pub role_name: String,
    pub level: u32,
    pub exp: u32,
    pub gold: u64,
    pub diamond: u64,
    pub coupons: u64,
    pub role_preset_id: i32,
    pub sex_male: bool,
    pub body_variant_idx: usize,
    pub head_variant_idx: usize,
    pub hair_variant_idx: usize,
    pub skin_color_idx: usize,
    pub skin_color_id: i32,
    pub hair_color_id: i32,
    pub head_item_id: i32,
    pub face_item_id: i32,
    pub equipped_prop_idx: usize,
    pub bag_prop_indices: Vec<usize>,
    #[serde(default)]
    pub lobby_store_items: Vec<LobbyStoreItemSave>,
    #[serde(default)]
    pub lobby_equipped_instance_id: Option<i32>,
    pub selected_bg_idx: usize,
}

impl LocalRoleProfile {
    fn capture(profile: &PlayerProfileSave) -> Self {
        Self {
            role_name: profile.role_name.clone(),
            level: profile.level,
            exp: profile.exp,
            gold: profile.gold,
            diamond: profile.diamond,
            coupons: profile.coupons,
            role_preset_id: profile.role_preset_id,
            sex_male: profile.sex_male,
            body_variant_idx: profile.body_variant_idx,
            head_variant_idx: profile.head_variant_idx,
            hair_variant_idx: profile.hair_variant_idx,
            skin_color_idx: profile.skin_color_idx,
            skin_color_id: profile.skin_color_id,
            hair_color_id: profile.hair_color_id,
            head_item_id: profile.head_item_id,
            face_item_id: profile.face_item_id,
            equipped_prop_idx: profile.equipped_prop_idx,
            bag_prop_indices: profile.bag_prop_indices.clone(),
            lobby_store_items: profile.lobby_store_items.clone(),
            lobby_equipped_instance_id: profile.lobby_equipped_instance_id,
            selected_bg_idx: profile.selected_bg_idx,
        }
    }

    fn apply(self, profile: &mut PlayerProfileSave) {
        profile.role_name = self.role_name;
        profile.level = self.level;
        profile.exp = self.exp;
        profile.gold = self.gold;
        profile.diamond = self.diamond;
        profile.coupons = self.coupons;
        profile.role_preset_id = self.role_preset_id;
        profile.sex_male = self.sex_male;
        profile.body_variant_idx = self.body_variant_idx;
        profile.head_variant_idx = self.head_variant_idx;
        profile.hair_variant_idx = self.hair_variant_idx;
        profile.skin_color_idx = self.skin_color_idx;
        profile.skin_color_id = self.skin_color_id;
        profile.hair_color_id = self.hair_color_id;
        profile.head_item_id = self.head_item_id;
        profile.face_item_id = self.face_item_id;
        profile.equipped_prop_idx = self.equipped_prop_idx;
        profile.bag_prop_indices = self.bag_prop_indices;
        profile.lobby_store_items = self.lobby_store_items;
        profile.lobby_equipped_instance_id = self.lobby_equipped_instance_id;
        profile.selected_bg_idx = self.selected_bg_idx;
    }
}

/// Local offline profile, loadout, customization, and `SettingMgr`-aligned preferences
/// persisted cleanly as JSON on disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Resource)]
#[serde(default)]
pub struct PlayerProfileSave {
    pub native_settings: std::collections::BTreeMap<String, f32>,
    /// Native PCKeyCodeCfg row ID to primary/secondary key chords; global client preference.
    pub native_keybinds: std::collections::BTreeMap<i32, Vec<Vec<i32>>>,
    pub schema_version: u32,
    /// Persisted login identity (`PlayerPrefsData.Account`), independent of the role name.
    pub account_name: String,
    /// Editable character nickname (`RoleMgr.CreateRoleName`).
    pub role_name: String,
    pub level: u32,
    pub exp: u32,
    pub gold: u64,
    pub diamond: u64,
    pub coupons: u64,
    pub role_preset_id: i32,
    pub sex_male: bool,
    pub body_variant_idx: usize,
    pub head_variant_idx: usize,
    pub hair_variant_idx: usize,
    pub skin_color_idx: usize,
    pub skin_color_id: i32,
    pub hair_color_id: i32,
    pub head_item_id: i32,
    pub face_item_id: i32,
    pub equipped_prop_idx: usize,
    pub bag_prop_indices: Vec<usize>,
    /// Role-owned lobby storage. StoreMgr stock is replaced by SGsItems; Init grants nothing.
    pub lobby_store_items: Vec<LobbyStoreItemSave>,
    pub lobby_equipped_instance_id: Option<i32>,
    pub selected_bg_idx: usize,
    pub music_volume: f32,
    pub sfx_volume: f32,
    pub fps_tier: u8,
    pub quality_tier: u8,
    pub lang_index: i32,
    pub keybinds: KeybindProfile,
    pub high_contrast_hud: bool,
    #[serde(default = "default_true")]
    pub show_control_hints: bool,
    /// Legacy account-only ownership list, retained for migration. `local_roles` owns roles.
    pub created_roles: Vec<String>,
    pub local_roles: Vec<LocalAccountRoleSave>,
    pub active_role_key: Option<LocalRoleKey>,
    /// Old saves with ambiguous ownership retain their role data without assigning it.
    pub unassigned_legacy_role: Option<LocalRoleProfile>,
    #[serde(skip, default = "default_true")]
    pub save_allowed: bool,
    /// Last `AuthMgr.sessionKey` entered in `LoginAccoutPanel`.
    pub session_key: String,
    /// Cached account list mirroring `PlayerPrefs.GetString("dev_Accounts")`.
    pub dev_accounts: Vec<String>,
}

impl Default for PlayerProfileSave {
    fn default() -> Self {
        Self {
            native_settings: Default::default(),
            native_keybinds: Default::default(),
            schema_version: 2,
            account_name: "Survivor_01".to_owned(),
            role_name: "Survivor_01".to_owned(),
            level: 1,
            exp: 120,
            gold: 2_500,
            diamond: 180,
            coupons: 50,
            role_preset_id: 1,
            sex_male: true,
            body_variant_idx: 0,
            head_variant_idx: 0,
            hair_variant_idx: 0,
            skin_color_idx: 0,
            skin_color_id: 1,
            hair_color_id: 150,
            head_item_id: 20500,
            face_item_id: 20550,
            equipped_prop_idx: 4, // wq_m4_1 in manifest.props
            bag_prop_indices: (0..10).collect(),
            lobby_store_items: Vec::new(),
            lobby_equipped_instance_id: None,
            selected_bg_idx: 0,
            music_volume: 0.45,
            sfx_volume: 0.75,
            fps_tier: 2,
            quality_tier: 2,
            lang_index: 0,
            keybinds: KeybindProfile::default(),
            high_contrast_hud: false,
            show_control_hints: true,
            created_roles: Vec::new(),
            local_roles: Vec::new(),
            active_role_key: None,
            unassigned_legacy_role: None,
            save_allowed: true,
            session_key: String::new(),
            dev_accounts: vec![
                "Survivor_01".to_owned(),
                "Andlua".to_owned(),
                "AlphaScout".to_owned(),
                "Builder_UA".to_owned(),
            ],
        }
    }
}

impl PlayerProfileSave {
    /// Returns the OS-appropriate persistent profile save path (`~/.local/share/bevy_zero_game/profile.json`).
    pub fn resolve_save_path() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
            && !xdg.trim().is_empty()
        {
            return PathBuf::from(xdg).join("bevy_zero_game/profile.json");
        }
        if let Ok(home) = std::env::var("HOME")
            && !home.trim().is_empty()
        {
            return PathBuf::from(home).join(".local/share/bevy_zero_game/profile.json");
        }
        std::env::temp_dir().join("bevy_zero_game_profile.json")
    }

    /// Reads `profile.json` from `path`, gracefully falling back to [`Default::default()`].
    pub fn load_from_disk_or_default(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        Self::decode_json(&raw).unwrap_or_else(|err| {
            warn!("Profile load rejected: {err}. Existing save will be preserved.");
            Self {
                save_allowed: false,
                ..Self::default()
            }
        })
    }

    /// Writes `self` atomically to `path`, creating parent directories as needed.
    pub fn save_to_disk(&self, path: &Path) -> Result<(), String> {
        if !self.save_allowed {
            return Err("Existing profile could not be loaded; preserving that save.".to_owned());
        }
        let mut saved = self.clone();
        saved.stash_active_role();
        atomic_write_json(path, &saved)
    }

    /// Explicitly migrate the former single-profile format, where the role editor also
    /// overwrote `account_name`. Only a sole recorded owner can recover that snapshot.
    pub fn decode_json(raw: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        let version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1);
        if version > 2 {
            return Err(format!("unsupported profile schema {version}"));
        }
        let mut profile: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if version == 1 {
            profile.role_name = profile.account_name.clone();
            profile.created_roles.sort();
            profile.created_roles.dedup();
            if let [owner] = profile.created_roles.as_slice() {
                let owner = owner.clone();
                profile.local_roles.push(LocalAccountRoleSave {
                    account_name: owner.clone(),
                    // The old save did not record a server. Bind it once at next login.
                    server_id: None,
                    role: LocalRoleProfile::capture(&profile),
                });
                profile.account_name = owner;
            } else {
                profile.unassigned_legacy_role = Some(LocalRoleProfile::capture(&profile));
            }
            profile.schema_version = 2;
        }
        Ok(profile)
    }

    pub fn has_saved_role(&self, account: &str, server_id: u32) -> bool {
        self.local_roles.iter().any(|entry| {
            entry.account_name == account
                && (entry.server_id == Some(server_id) || entry.server_id.is_none())
        })
    }

    /// Activate only this account's role on the selected endpoint. A new account gets a
    /// fresh character draft; account switching cannot inherit another player's role.
    pub fn activate_role(&mut self, account: &str, server_id: u32) -> bool {
        self.stash_active_role();
        self.account_name = account.to_owned();
        let key = LocalRoleKey {
            account_name: account.to_owned(),
            server_id,
        };
        let saved = self
            .local_roles
            .iter_mut()
            .find(|entry| {
                entry.account_name == account
                    && (entry.server_id == Some(server_id) || entry.server_id.is_none())
            })
            .map(|entry| {
                entry.server_id = Some(server_id);
                entry.role.clone()
            });
        if let Some(role) = saved {
            role.apply(self);
            self.active_role_key = Some(key);
            true
        } else {
            LocalRoleProfile::capture(&Self::default()).apply(self);
            self.role_name.clear();
            self.active_role_key = None;
            false
        }
    }

    pub fn complete_role_creation(&mut self, account: &str, server_id: u32) {
        self.account_name = account.to_owned();
        self.active_role_key = Some(LocalRoleKey {
            account_name: account.to_owned(),
            server_id,
        });
        self.stash_active_role();
        if !self.created_roles.iter().any(|owner| owner == account) {
            self.created_roles.push(account.to_owned());
        }
    }

    fn stash_active_role(&mut self) {
        let Some(key) = self.active_role_key.clone() else {
            return;
        };
        let role = LocalRoleProfile::capture(self);
        if let Some(entry) = self.local_roles.iter_mut().find(|entry| {
            entry.account_name == key.account_name && entry.server_id == Some(key.server_id)
        }) {
            entry.role = role;
        } else {
            self.local_roles.push(LocalAccountRoleSave {
                account_name: key.account_name,
                server_id: Some(key.server_id),
                role,
            });
        }
    }

}

/// Injectable profile destination; native runtime uses the normal per-user save location.
#[derive(Debug, Clone, Resource)]
pub struct PlayerProfileStorage(pub PathBuf);

impl Default for PlayerProfileStorage {
    fn default() -> Self {
        Self(PlayerProfileSave::resolve_save_path())
    }
}

/// Runtime localization table loaded from `cfg.MultiLanguageIndexCfg.json` + `cfg.MultiLanguageCfg.json`.
#[derive(Debug, Clone, Default, Resource)]
pub struct LocalizationDb {
    pub index_entries: Vec<MultiLanguageIndexEntry>,
    pub by_key: HashMap<String, Vec<String>>,
    /// Matches `MultiLanguageIndexEntry::index` (`-1` = key itself, `0` = English, `8` = Ukrainian).
    pub selected_lang_index: i32,
}

impl LocalizationDb {
    /// Looks up the localized string for `cn_key`, falling back to `en-US` (`0`) or `cn_key`.
    pub fn tr<'a>(&'a self, cn_key: &'a str) -> &'a str {
        if self.selected_lang_index < 0 {
            return cn_key;
        }
        let Some(langs) = self.by_key.get(cn_key) else {
            return cn_key;
        };
        let idx = self.selected_lang_index as usize;
        if let Some(s) = langs.get(idx)
            && !s.is_empty()
        {
            return s.as_str();
        }
        if let Some(en) = langs.first()
            && !en.is_empty()
        {
            return en.as_str();
        }
        cn_key
    }

    /// Returns the currently active language entry from `MultiLanguageIndexCfg`.
    pub fn active_language(&self) -> Option<&MultiLanguageIndexEntry> {
        self.index_entries
            .iter()
            .find(|e| e.index == self.selected_lang_index)
            .or_else(|| self.index_entries.first())
    }

    /// Formats a crisp ASCII-safe display label for language picker buttons.
    pub fn display_label_for(entry: &MultiLanguageIndexEntry) -> String {
        if entry.gme_language_type.is_empty() {
            format!("Lang #{}", entry.id)
        } else {
            format!(
                "{} ({})",
                entry.gme_language_type,
                entry
                    .notice_url_suffix
                    .trim_start_matches('_')
                    .to_uppercase()
            )
        }
    }
}

/// Player login & lobby selection state persisted locally across Phase 1 sessions.
#[derive(Debug, Clone, Resource)]
pub struct PlayerSessionPrefs {
    pub account_name: String,
    pub session_key: String,
    pub selected_server_idx: usize,
    pub selected_region_idx: usize,
    pub notice_acknowledged: bool,
    pub language_modal_open: bool,
    pub server_modal_open: bool,
    pub account_modal_open: bool,
}

impl Default for PlayerSessionPrefs {
    fn default() -> Self {
        Self {
            account_name: "Survivor_01".to_owned(),
            session_key: String::new(),
            selected_server_idx: 0,
            selected_region_idx: 0,
            notice_acknowledged: true,
            language_modal_open: false,
            server_modal_open: false,
            account_modal_open: false,
        }
    }
}

/// Consolidated runtime game configuration loaded from `assets/config/`.
#[derive(Debug, Clone, Default, Resource)]
pub struct GameConfigs {
    pub native_consts: HashMap<String, serde_json::Value>,
    pub version: VersionInfo,
    pub res_config: ResConfigData,
    pub server_list: ServerListData,
    pub zhanju_cfgs: Vec<ZhanJuCfgEntry>,
    pub zhanju_regions: Vec<ZhanjuRegionCfgEntry>,
    pub battle_maps: Vec<BattleMapCfgEntry>,
    pub main_bgs: Vec<MainBgEntry>,
    pub load_descs: Vec<LoadDescEntry>,
    pub load_panels: Vec<LoadPanelCfgEntry>,
    pub strings: HashMap<i32, String>,
    pub battle_loading_tips: Vec<BattleLoadingTipEntry>,
    pub close_weapons: Vec<CloseWeaponEntry>,
    pub establish_roles: Vec<EstablishRoleEntry>,
    pub guns: Vec<GunCfgEntry>,
    pub role_fashions: Vec<RoleFashionEntry>,
    pub role_heads: Vec<RoleHeadEntry>,
    pub skin_colors: Vec<SkinColorEntry>,
    pub equip_cfgs: Vec<EquipCfgEntry>,
    pub cn_name_pools: [Vec<String>; 5],
    pub multilang_names: Vec<Vec<String>>,
    pub starter_item_icons: HashMap<i32, String>,
    pub build_parts: Vec<BuildPartEntry>,
    pub tree_cfgs: Vec<TreeCfgEntry>,
    pub scene_cells: Vec<SceneCellEntry>,
    pub turret_cfgs: Vec<TurretCfgEntry>,
    pub monster_cfgs: Vec<MonsterCfgEntry>,
    pub port_boss_cfgs: Vec<PortBossMonsterEntry>,
    pub vehicle_cfgs: Vec<VehicleCfgEntry>,
    pub car_cfgs: Vec<CarCfgEntry>,
    pub electric_turret_cfgs: Vec<ElectricTurretCfgEntry>,
    pub battery_cfgs: Vec<BatteryCapacityCfgEntry>,
    pub shortcut_keys: Vec<ShortCutKeysEntry>,
    pub pc_key_codes: Vec<PcKeyCodeEntry>,
    pub item_catalog_count: usize,
    pub item_cfgs: Vec<ItemCfgEntry>,
    pub drawing_cfgs: Vec<DrawingCfgEntry>,
    pub weapon_catalog: Vec<WeaponCatalogEntry>,
    pub system_item_count: usize,
    pub loaded_small: bool,
    pub loaded_multilang: bool,
    pub warnings: Vec<String>,
}

impl GameConfigs {
    pub fn native_const_u64(&self, key: &str) -> Option<u64> {
        self.native_consts
            .get(key)
            .and_then(serde_json::Value::as_u64)
    }

    pub fn recreate_role_money(&self) -> u64 {
        self.native_const_u64("RECREATE_ROLE_MONEY")
            .unwrap_or(3_000)
    }

    pub fn role_name_limit(&self) -> usize {
        self.native_const_u64("MAX_ROLE_NAME_LENGTH")
            .and_then(|limit| usize::try_from(limit).ok())
            .unwrap_or(7)
    }

    pub fn active_server(&self, idx: usize) -> Option<&ServerEntry> {
        self.server_list
            .servers
            .get(idx)
            .or_else(|| self.server_list.servers.first())
    }

    /// Faithfully mirrors APK `Utils.GetString(int id, params object[] args)` using
    /// `cfg.Strings.json` + `LocalizationDb` + `{0}`, `{1}`, ... placeholder formatting.
    pub fn get_string(&self, l10n: &LocalizationDb, id: i32, args: &[&str]) -> String {
        let Some(raw) = self.strings.get(&id) else {
            return format!("String#{id}");
        };
        let mut text = l10n.tr(raw).to_owned();
        for (idx, arg) in args.iter().enumerate() {
            let pat = format!("{{{idx}}}");
            text = text.replace(&pat, arg);
        }
        text
    }

    /// Resolves an APK `EstablishRole.icon` or `ItemCfg.icon` path (e.g. `"head1/man_tou_1_1"`)
    /// into its exported `assets/ui/<stem>.png` path.
    pub fn starter_icon_asset_path(&self, raw_icon: &str) -> String {
        let stem = raw_icon
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or("man_tou_1_1")
            .trim_end_matches(".png");
        format!("ui/{stem}.png")
    }

    /// Resolves an `ItemCfg` starter hair/face item ID (`20500..=23557`) into its exported UI icon path.
    pub fn item_icon_asset_path(&self, item_id: i32) -> Option<String> {
        if let Some(raw) = self.starter_item_icons.get(&item_id) {
            return Some(self.starter_icon_asset_path(raw));
        }
        if (20500..=20504).contains(&item_id) {
            return Some(format!("ui/man_fa_{}_1.png", 2 + (item_id - 20500)));
        }
        if (23500..=23506).contains(&item_id) {
            return Some(format!("ui/woman_fa_{}_1.png", 1 + (item_id - 23500)));
        }
        if (20550..=20557).contains(&item_id) {
            return Some(format!("ui/man_tou_{}_1.png", 1 + (item_id - 20550)));
        }
        if (23550..=23557).contains(&item_id) {
            return Some(format!("ui/woman_tou_{}_1.png", 1 + (item_id - 23550)));
        }
        None
    }

    /// Finds a `SkinColorCfg` entry by `id`.
    pub fn skin_color_by_id(&self, id: i32) -> Option<&SkinColorEntry> {
        self.skin_colors.iter().find(|s| s.id == id)
    }

    /// Deterministic local recommendation using native name tables. The original client
    /// requests recommendations from the server; server selection parity remains unknown.
    pub fn generate_random_role_name(&self, lang_index: i32, seed: usize) -> String {
        let sanitize = |s: &str| -> String {
            s.chars()
                .filter(|c| !crate::auth::SPECIAL_NAME_CHARACTERS.contains(*c))
                .collect()
        };
        let limit_name = |s: &str| -> String {
            let mut units = 0;
            s.chars()
                .take_while(|c| {
                    units += c.len_utf16();
                    units <= self.role_name_limit()
                })
                .collect()
        };
        if lang_index > 0 && !self.multilang_names.is_empty() {
            let n = self.multilang_names.len();
            let row_a = &self.multilang_names[(seed.wrapping_mul(31).wrapping_add(7)) % n];
            let row_b = &self.multilang_names[(seed.wrapping_mul(97).wrapping_add(23)) % n];
            let col_a = (lang_index as usize).min(row_a.len().saturating_sub(1));
            let col_b = (lang_index as usize).min(row_b.len().saturating_sub(1));
            let a = row_a.get(col_a).map(|s| sanitize(s)).unwrap_or_default();
            let b = row_b.get(col_b).map(|s| sanitize(s)).unwrap_or_default();
            let combined = format!("{a}{b}");
            let limited = limit_name(&combined);
            if crate::auth::check_name(&limited).is_none() {
                return limited;
            }
        }
        let p1 = &self.cn_name_pools[1];
        let p2 = &self.cn_name_pools[2];
        let p3 = &self.cn_name_pools[3];
        let p4 = &self.cn_name_pools[4];
        if !p1.is_empty() && !p2.is_empty() {
            let a = sanitize(&p1[(seed.wrapping_mul(17).wrapping_add(3)) % p1.len()]);
            let b_pool = if seed.is_multiple_of(2) && !p3.is_empty() {
                p3
            } else if !p4.is_empty() {
                p4
            } else {
                p2
            };
            let b = sanitize(&b_pool[(seed.wrapping_mul(53).wrapping_add(11)) % b_pool.len()]);
            let combined = format!("{a}{b}");
            let limited = limit_name(&combined);
            if crate::auth::check_name(&limited).is_none() {
                return limited;
            }
        }
        String::new()
    }

    /// Resolves an APK `LoadPanelCfg.texture` path (e.g. `"texture/loading_6"`) into an exported
    /// Pure-Bevy PNG asset path (`"ui/Loading_6.png"`).
    pub fn resolve_load_panel_texture_asset(tex_field: &str) -> Option<String> {
        let stem = tex_field
            .trim()
            .rsplit('/')
            .next()?
            .trim_end_matches(".png");
        let low = stem.to_ascii_lowercase();
        let file = match low.as_str() {
            "loading_5" => "ui/Loading_5.png",
            "loading_6" => "ui/Loading_6.png",
            "loading_7" => "ui/Loading_7.png",
            "loading_8" => "ui/Loading_8.png",
            "loading_bg_2" => "ui/Loading_bg_2.png",
            _ => return None,
        };
        Some(file.to_owned())
    }
}

/// Asynchronous loading progress displayed in `LauncherSplash`.
#[derive(Debug, Clone, Default, Resource)]
pub struct ConfigLoadProgress {
    pub fraction: f32,
    pub stage_label: String,
    pub finished: bool,
    pub splash_elapsed_secs: f32,
}

type MultilangMapResult = Result<
    (
        HashMap<String, Vec<String>>,
        Vec<ItemCfgEntry>,
        Vec<WeaponCatalogEntry>,
        HashMap<i32, String>,
        Vec<Vec<String>>,
    ),
    String,
>;

#[derive(Resource)]
struct PendingMultilangTask(Option<Task<MultilangMapResult>>);

#[derive(SystemParam)]
struct PreloadStateCtx<'w> {
    state: Res<'w, State<AppState>>,
    next_state: ResMut<'w, NextState<AppState>>,
    configs: ResMut<'w, GameConfigs>,
    l10n: ResMut<'w, LocalizationDb>,
    progress: ResMut<'w, ConfigLoadProgress>,
}

struct SmallConfigBundle {
    native_consts: HashMap<String, serde_json::Value>,
    version: VersionInfo,
    res_config: ResConfigData,
    server_list: ServerListData,
    zhanju_cfgs: Vec<ZhanJuCfgEntry>,
    zhanju_regions: Vec<ZhanjuRegionCfgEntry>,
    battle_maps: Vec<BattleMapCfgEntry>,
    lang_indices: Vec<MultiLanguageIndexEntry>,
    main_bgs: Vec<MainBgEntry>,
    load_descs: Vec<LoadDescEntry>,
    load_panels: Vec<LoadPanelCfgEntry>,
    strings: HashMap<i32, String>,
    battle_loading_tips: Vec<BattleLoadingTipEntry>,
    close_weapons: Vec<CloseWeaponEntry>,
    establish_roles: Vec<EstablishRoleEntry>,
    guns: Vec<GunCfgEntry>,
    role_fashions: Vec<RoleFashionEntry>,
    role_heads: Vec<RoleHeadEntry>,
    skin_colors: Vec<SkinColorEntry>,
    equip_cfgs: Vec<EquipCfgEntry>,
    cn_name_pools: [Vec<String>; 5],
    build_parts: Vec<BuildPartEntry>,
    tree_cfgs: Vec<TreeCfgEntry>,
    scene_cells: Vec<SceneCellEntry>,
    turret_cfgs: Vec<TurretCfgEntry>,
    monster_cfgs: Vec<MonsterCfgEntry>,
    port_boss_cfgs: Vec<PortBossMonsterEntry>,
    vehicle_cfgs: Vec<VehicleCfgEntry>,
    car_cfgs: Vec<CarCfgEntry>,
    electric_turret_cfgs: Vec<ElectricTurretCfgEntry>,
    battery_cfgs: Vec<BatteryCapacityCfgEntry>,
    shortcut_keys: Vec<ShortCutKeysEntry>,
    pc_key_codes: Vec<PcKeyCodeEntry>,
    system_item_count: usize,
    drawing_cfgs: Vec<DrawingCfgEntry>,
    warnings: Vec<String>,
}

/// Resolves the canonical `assets/` root cleanly regardless of whether the process launches from
/// the workspace parent directory (`DevX/`), crate root (`bevy_zero_game/`), or `target/*`.
pub fn resolve_assets_root() -> PathBuf {
    let candidates = [
        PathBuf::from("assets"),
        PathBuf::from("bevy_zero_game/assets"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"),
    ];
    for cand in candidates {
        if cand.join("manifest.json").is_file() {
            return std::fs::canonicalize(&cand).unwrap_or(cand);
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")
}

/// Loads `assets/manifest.json` cleanly with graceful fallback.
pub fn load_manifest_from_disk(base_dir: &Path) -> Result<AssetManifest, String> {
    let path = base_dir.join("manifest.json");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read `{}`: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("invalid manifest JSON: {e}"))
}

/// Parses `assets/config/ResConfig.txt` (`key = value` lines).
pub fn parse_res_config(raw: &str) -> ResConfigData {
    let mut entries = HashMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            entries.insert(k.trim().to_owned(), v.trim().to_owned());
        }
    }
    let res_root_path = entries
        .get("ResRootPath")
        .cloned()
        .unwrap_or_else(|| "res/".to_owned());
    let is_open_res_divide = entries
        .get("IsOpenResDivide")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"));
    ResConfigData {
        res_root_path,
        is_open_res_divide,
        entries,
    }
}

/// Parses `assets/config/serverList.xml` self-closing `<server .../>` and `<region .../>` tags.
pub fn parse_server_list_xml(raw: &str) -> ServerListData {
    let mut out = ServerListData::default();
    for chunk in raw.split('<') {
        let tag = chunk.trim();
        if let Some(rest) = tag.strip_prefix("server")
            && rest.starts_with(char::is_whitespace)
        {
            let attrs = parse_xml_attributes(rest);
            let id = attrs.get("id").and_then(|s| s.parse().ok()).unwrap_or(0);
            let type_code = attrs.get("type").and_then(|s| s.parse().ok()).unwrap_or(0);
            let name = attrs
                .get("name")
                .cloned()
                .unwrap_or_else(|| format!("Server-{id}"));
            let gate_url = attrs.get("gateurl").cloned().unwrap_or_default();
            let port = attrs
                .get("port")
                .and_then(|s| s.parse().ok())
                .unwrap_or(9211);
            let status_code = attrs
                .get("status")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            out.servers.push(ServerEntry {
                id,
                mode: ServerModeType::from_code(type_code),
                name,
                gate_url,
                port,
                status: ServerStatus::from_code(status_code),
                is_local_loopback: false,
            });
        } else if let Some(rest) = tag.strip_prefix("region")
            && rest.starts_with(char::is_whitespace)
        {
            let attrs = parse_xml_attributes(rest);
            let id = attrs.get("id").and_then(|s| s.parse().ok()).unwrap_or(0);
            let name = attrs
                .get("name")
                .cloned()
                .unwrap_or_else(|| format!("Region-{id}"));
            let server_list_raw = attrs.get("serverList").cloned().unwrap_or_default();
            out.regions.push(RegionEntry {
                id,
                name,
                server_list_raw,
            });
        }
    }
    out
}

fn parse_xml_attributes(tag_body: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let end = tag_body.find('>').unwrap_or(tag_body.len());
    let mut cursor = &tag_body[..end];
    while let Some(eq_idx) = cursor.find('=') {
        let key = cursor[..eq_idx].trim().trim_end_matches('/');
        let key_clean = key.split_whitespace().last().unwrap_or("");
        let after_eq = cursor[eq_idx + 1..].trim_start();
        let Some(quote) = after_eq.chars().next().filter(|&c| c == '"' || c == '\'') else {
            break;
        };
        let rest = &after_eq[quote.len_utf8()..];
        let Some(close_idx) = rest.find(quote) else {
            break;
        };
        if !key_clean.is_empty() {
            map.insert(key_clean.to_owned(), rest[..close_idx].to_owned());
        }
        cursor = &rest[close_idx + quote.len_utf8()..];
    }
    map
}

fn load_small_configs_from_dir(cfg_dir: &Path) -> SmallConfigBundle {
    let mut warnings = Vec::new();

    let zhanju_cfgs: DecodedOcFile<ZhanJuCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.ZhanJuCfg.json", &mut warnings);
    let zhanju_regions: DecodedOcFile<ZhanjuRegionCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.ZhanjuRegionCfg.json", &mut warnings);
    let battle_maps: DecodedOcFile<BattleMapCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.MapCfg.json", &mut warnings);

    let native_consts: DecodedOcFile<HashMap<String, serde_json::Value>> =
        read_json_or_warn(cfg_dir, "cfg.Consts.json", &mut warnings);

    let version: VersionInfo = read_json_or_warn(cfg_dir, "version.txt", &mut warnings);
    let res_config = match std::fs::read_to_string(cfg_dir.join("ResConfig.txt")) {
        Ok(raw) => parse_res_config(&raw),
        Err(e) => {
            warnings.push(format!("ResConfig.txt missing: {e}"));
            ResConfigData::default()
        }
    };

    let mut server_list = match std::fs::read_to_string(cfg_dir.join("serverList.xml")) {
        Ok(raw) => parse_server_list_xml(&raw),
        Err(e) => {
            warnings.push(format!("serverList.xml missing: {e}"));
            ServerListData::default()
        }
    };
    // Ensure a local 100-player loopback server option is always first for offline/LAN test play.
    server_list.servers.insert(
        0,
        ServerEntry {
            id: 1,
            mode: ServerModeType::BattleRoyale100,
            name: "Localhost 100P UDP Host".to_owned(),
            gate_url: "127.0.0.1".to_owned(),
            port: 0,
            status: ServerStatus::Smooth,
            is_local_loopback: true,
        },
    );

    let lang_indices: DecodedOcFile<MultiLanguageIndexEntry> =
        read_json_or_warn(cfg_dir, "cfg.MultiLanguageIndexCfg.json", &mut warnings);
    let main_bgs: DecodedOcFile<MainBgEntry> =
        read_json_or_warn(cfg_dir, "cfg.MainBgCfg.json", &mut warnings);
    let load_descs: DecodedOcFile<LoadDescEntry> =
        read_json_or_warn(cfg_dir, "cfg.LoadDescCfg.json", &mut warnings);
    let load_panels: DecodedOcFile<LoadPanelCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.LoadPanelCfg.json", &mut warnings);
    let strings_file: DecodedOcFile<StringCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.Strings.json", &mut warnings);
    let strings: HashMap<i32, String> = strings_file
        .records
        .into_iter()
        .map(|s| (s.id, s.content))
        .collect();
    let battle_loading_tips: DecodedOcFile<BattleLoadingTipEntry> =
        read_json_or_warn(cfg_dir, "cfg.BattleLoadingTipsCfg.json", &mut warnings);
    let close_weapons: DecodedOcFile<CloseWeaponEntry> =
        read_json_or_warn(cfg_dir, "cfg.CloseWeaponCfg.json", &mut warnings);
    let establish_roles: DecodedOcFile<EstablishRoleEntry> =
        read_json_or_warn(cfg_dir, "cfg.EstablishRole.json", &mut warnings);
    let guns: DecodedOcFile<GunCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.GunCfg.json", &mut warnings);
    let role_fashions: DecodedOcFile<RoleFashionEntry> =
        read_json_or_warn(cfg_dir, "cfg.RoleFashionCfg.json", &mut warnings);
    let role_heads: DecodedOcFile<RoleHeadEntry> =
        read_json_or_warn(cfg_dir, "cfg.RoleHeadCfg.json", &mut warnings);
    let skin_colors: DecodedOcFile<SkinColorEntry> =
        read_json_or_warn(cfg_dir, "cfg.SkinColorCfg.json", &mut warnings);
    let equip_cfgs: DecodedOcFile<EquipCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.EquipCfg.json", &mut warnings);
    let cn_name_pools = [
        read_json_or_warn::<DecodedOcFile<SimpleNameEntry>>(
            cfg_dir,
            "cfg.Name1Cfg.json",
            &mut warnings,
        )
        .records
        .into_iter()
        .map(|r| r.name)
        .filter(|s| !s.is_empty())
        .collect(),
        read_json_or_warn::<DecodedOcFile<SimpleNameEntry>>(
            cfg_dir,
            "cfg.Name2Cfg.json",
            &mut warnings,
        )
        .records
        .into_iter()
        .map(|r| r.name)
        .filter(|s| !s.is_empty())
        .collect(),
        read_json_or_warn::<DecodedOcFile<SimpleNameEntry>>(
            cfg_dir,
            "cfg.Name3Cfg.json",
            &mut warnings,
        )
        .records
        .into_iter()
        .map(|r| r.name)
        .filter(|s| !s.is_empty())
        .collect(),
        read_json_or_warn::<DecodedOcFile<SimpleNameEntry>>(
            cfg_dir,
            "cfg.Name4Cfg.json",
            &mut warnings,
        )
        .records
        .into_iter()
        .map(|r| r.name)
        .filter(|s| !s.is_empty())
        .collect(),
        read_json_or_warn::<DecodedOcFile<SimpleNameEntry>>(
            cfg_dir,
            "cfg.Name5Cfg.json",
            &mut warnings,
        )
        .records
        .into_iter()
        .map(|r| r.name)
        .filter(|s| !s.is_empty())
        .collect(),
    ];
    let build_parts: DecodedOcFile<BuildPartEntry> =
        read_json_or_warn(cfg_dir, "cfg.BuildPart.json", &mut warnings);
    let tree_cfgs: DecodedOcFile<TreeCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.TreeCfg.json", &mut warnings);
    let scene_cell_file: SceneCellFile =
        read_json_or_warn(cfg_dir, "scenecell.json", &mut warnings);
    let turret_cfgs: DecodedOcFile<TurretCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.TurretCfg.json", &mut warnings);
    let monster_cfgs: DecodedOcFile<MonsterCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.MonsterCfg.json", &mut warnings);
    let port_boss_cfgs: DecodedOcFile<PortBossMonsterEntry> =
        read_json_or_warn(cfg_dir, "cfg.PortBossMonsterCfg.json", &mut warnings);
    let vehicle_cfgs: DecodedOcFile<VehicleCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.VehicleCfg.json", &mut warnings);
    let car_cfgs: DecodedOcFile<CarCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.CarCfg.json", &mut warnings);
    let electric_turret_cfgs: DecodedOcFile<ElectricTurretCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.ElectricTurretCfg.json", &mut warnings);
    let battery_cfgs: DecodedOcFile<BatteryCapacityCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.BatteryCapacityCfg.json", &mut warnings);
    let shortcut_keys: DecodedOcFile<ShortCutKeysEntry> =
        read_json_or_warn(cfg_dir, "cfg.ShortCutKeysCfg.json", &mut warnings);
    let pc_key_codes: DecodedOcFile<PcKeyCodeEntry> =
        read_json_or_warn(cfg_dir, "cfg.PCKeyCodeCfg.json", &mut warnings);
    let system_items: DecodedOcFile<serde_json::Value> =
        read_json_or_warn(cfg_dir, "cfg.SystemItemCfg.json", &mut warnings);
    let drawing_cfgs: DecodedOcFile<DrawingCfgEntry> =
        read_json_or_warn(cfg_dir, "cfg.DrawingCfg.json", &mut warnings);

    SmallConfigBundle {
        zhanju_cfgs: zhanju_cfgs.records,
        zhanju_regions: zhanju_regions.records,
        battle_maps: battle_maps.records,
        native_consts: native_consts.records.into_iter().next().unwrap_or_default(),
        version,
        res_config,
        server_list,
        lang_indices: lang_indices.records,
        main_bgs: main_bgs.records,
        load_descs: load_descs.records,
        load_panels: load_panels.records,
        strings,
        battle_loading_tips: battle_loading_tips.records,
        close_weapons: close_weapons.records,
        establish_roles: establish_roles.records,
        guns: guns.records,
        role_fashions: role_fashions.records,
        role_heads: role_heads.records,
        skin_colors: skin_colors.records,
        equip_cfgs: equip_cfgs.records,
        cn_name_pools,
        build_parts: build_parts.records,
        tree_cfgs: tree_cfgs.records,
        scene_cells: scene_cell_file.cells,
        turret_cfgs: turret_cfgs.records,
        monster_cfgs: monster_cfgs.records,
        port_boss_cfgs: port_boss_cfgs.records,
        vehicle_cfgs: vehicle_cfgs.records,
        car_cfgs: car_cfgs.records,
        electric_turret_cfgs: electric_turret_cfgs.records,
        battery_cfgs: battery_cfgs.records,
        shortcut_keys: shortcut_keys.records,
        pc_key_codes: pc_key_codes.records,
        system_item_count: system_items.records.len(),
        drawing_cfgs: drawing_cfgs.records,
        warnings,
    }
}

fn load_multilang_map_from_dir(cfg_dir: &Path) -> Result<HashMap<String, Vec<String>>, String> {
    let path = cfg_dir.join("cfg.MultiLanguageCfg.json");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read `{}`: {e}", path.display()))?;
    let parsed: DecodedOcFile<MultiLanguageRow> =
        serde_json::from_str(&raw).map_err(|e| format!("invalid MultiLanguageCfg JSON: {e}"))?;
    let mut map = HashMap::with_capacity(parsed.records.len());
    for row in parsed.records {
        map.insert(row.id, row.languages);
    }
    Ok(map)
}

fn load_multilang_names_from_dir(cfg_dir: &Path) -> Vec<Vec<String>> {
    let path = cfg_dir.join("cfg.MultiLanguageNameCfg.json");
    let parsed: DecodedOcFile<MultiLanguageNameEntry> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    parsed
        .records
        .into_iter()
        .map(|r| r.language_names)
        .filter(|v| !v.is_empty())
        .collect()
}

/// Joins the 10 exported prop GLB names with `cfg.ItemCfg.json`, `cfg.GunCfg.json`, and
/// `cfg.CloseWeaponCfg.json` in a single pass on the background worker.
fn build_weapon_catalog_from_dir(
    cfg_dir: &Path,
    guns: &[GunCfgEntry],
    close_weapons: &[CloseWeaponEntry],
    prop_names: &[String],
) -> (
    Vec<ItemCfgEntry>,
    Vec<WeaponCatalogEntry>,
    HashMap<i32, String>,
) {
    let item_path = cfg_dir.join("cfg.ItemCfg.json");
    let parsed: DecodedOcFile<ItemCfgEntry> = std::fs::read_to_string(&item_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();

    let mut starter_icons = HashMap::new();
    let mut by_model_stem: HashMap<String, &ItemCfgEntry> = HashMap::new();
    for item in &parsed.records {
        if (20500..=24000).contains(&item.id) && !item.icon.is_empty() {
            starter_icons.insert(item.id, item.icon.clone());
        }
        let low = item.model_path.to_ascii_lowercase();
        if low.is_empty() {
            continue;
        }
        for name in prop_names {
            if low.contains(&name.to_ascii_lowercase()) && !by_model_stem.contains_key(name) {
                by_model_stem.insert(name.clone(), item);
            }
        }
    }

    let mut catalog = Vec::with_capacity(prop_names.len());
    for (idx, name) in prop_names.iter().enumerate() {
        let matched = by_model_stem.get(name).copied();
        let item_id = matched.map(|m| m.id).unwrap_or(50_000 + idx as i32);
        let gun_match = guns.iter().find(|g| g.id == item_id);
        let melee_match = close_weapons.iter().find(|c| c.id == item_id);
        let base_damage = gun_match
            .and_then(|g| g.factors.first().copied())
            .map(|f| f.round() as i32)
            .or_else(|| melee_match.map(|m| m.base_damage))
            .unwrap_or(25);
        let category_cn = gun_match
            .map(|g| g.gun_type_name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if melee_match.is_some() {
                    "Melee".to_owned()
                } else {
                    "Tactical".to_owned()
                }
            });
        let sound_hint = gun_match
            .map(|g| g.sound_setting_path.clone())
            .unwrap_or_default();
        catalog.push(WeaponCatalogEntry {
            item_id,
            name_cn: matched
                .map(|m| m.name.clone())
                .unwrap_or_else(|| name.clone()),
            category_cn,
            base_damage,
            quality: matched.map(|m| m.quality).unwrap_or(1),
            sound_hint,
        });
    }
    (parsed.records, catalog, starter_icons)
}

fn read_json_or_warn<T: for<'de> Deserialize<'de> + Default>(
    dir: &Path,
    filename: &str,
    warnings: &mut Vec<String>,
) -> T {
    let path = dir.join(filename);
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            warnings.push(format!("{filename} parse error: {e}"));
            T::default()
        }),
        Err(e) => {
            warnings.push(format!("{filename} read error: {e}"));
            T::default()
        }
    }
}

/// Bevy plugin managing boot config loading and `MultiLanguageCfg` background parsing.
pub struct GameConfigPlugin;

impl Plugin for GameConfigPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GameConfigs>()
            .init_resource::<LocalizationDb>()
            .init_resource::<PlayerSessionPrefs>()
            .init_resource::<PlayerProfileSave>()
            .init_resource::<PlayerProfileStorage>()
            .init_resource::<ConfigLoadProgress>()
            .add_systems(OnEnter(AppState::Boot), start_boot_preload)
            .add_systems(
                Update,
                poll_config_preload.run_if(
                    in_state(AppState::PreloadConfigs).or_else(in_state(AppState::LauncherSplash)),
                ),
            );
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Bevy injects the boot system's resources, including the testable profile destination."
)]
fn start_boot_preload(
    mut commands: Commands,
    mut configs: ResMut<GameConfigs>,
    mut l10n: ResMut<LocalizationDb>,
    mut prefs: ResMut<PlayerSessionPrefs>,
    mut profile: ResMut<PlayerProfileSave>,
    profile_storage: Res<PlayerProfileStorage>,
    mut progress: ResMut<ConfigLoadProgress>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    *profile = PlayerProfileSave::load_from_disk_or_default(&profile_storage.0);
    prefs.account_name = profile.account_name.clone();
    prefs.session_key = profile.session_key.clone();
    l10n.selected_lang_index = profile.lang_index;

    let assets_root = resolve_assets_root();
    let cfg_dir = assets_root.join("config");
    let manifest_props: Vec<String> = load_manifest_from_disk(&assets_root)
        .map(|m| m.props.into_iter().map(|p| p.name).collect())
        .unwrap_or_default();

    let bundle = load_small_configs_from_dir(&cfg_dir);
    configs.native_consts = bundle.native_consts;
    configs.version = bundle.version;
    configs.res_config = bundle.res_config;
    configs.server_list = bundle.server_list;
    configs.zhanju_cfgs = bundle.zhanju_cfgs;
    configs.zhanju_regions = bundle.zhanju_regions;
    configs.battle_maps = bundle.battle_maps;
    l10n.index_entries = bundle.lang_indices;
    configs.main_bgs = bundle.main_bgs;
    configs.load_descs = bundle.load_descs;
    configs.load_panels = bundle.load_panels;
    configs.strings = bundle.strings;
    configs.battle_loading_tips = bundle.battle_loading_tips;
    configs.close_weapons = bundle.close_weapons;
    configs.establish_roles = bundle.establish_roles;
    configs.guns = bundle.guns;
    configs.role_fashions = bundle.role_fashions;
    configs.role_heads = bundle.role_heads;
    configs.skin_colors = bundle.skin_colors;
    configs.equip_cfgs = bundle.equip_cfgs;
    configs.cn_name_pools = bundle.cn_name_pools;
    configs.build_parts = bundle.build_parts;
    configs.tree_cfgs = bundle.tree_cfgs;
    configs.scene_cells = bundle.scene_cells;
    configs.turret_cfgs = bundle.turret_cfgs;
    configs.monster_cfgs = bundle.monster_cfgs;
    configs.port_boss_cfgs = bundle.port_boss_cfgs;
    configs.vehicle_cfgs = bundle.vehicle_cfgs;
    configs.car_cfgs = bundle.car_cfgs;
    configs.electric_turret_cfgs = bundle.electric_turret_cfgs;
    configs.battery_cfgs = bundle.battery_cfgs;
    configs.shortcut_keys = bundle.shortcut_keys;
    configs.pc_key_codes = bundle.pc_key_codes;
    configs.system_item_count = bundle.system_item_count;
    configs.drawing_cfgs = bundle.drawing_cfgs;
    configs.warnings.extend(bundle.warnings);
    configs.loaded_small = true;

    let guns_bg = configs.guns.clone();
    let melee_bg = configs.close_weapons.clone();
    let pool = IoTaskPool::get();
    let lang_task = pool.spawn(async move {
        let map = load_multilang_map_from_dir(&cfg_dir)?;
        let (items, catalog, starter_icons) =
            build_weapon_catalog_from_dir(&cfg_dir, &guns_bg, &melee_bg, &manifest_props);
        let ml_names = load_multilang_names_from_dir(&cfg_dir);
        Ok((map, items, catalog, starter_icons, ml_names))
    });

    commands.insert_resource(PendingMultilangTask(Some(lang_task)));
    progress.fraction = 0.45;
    progress.stage_label = "Streaming localization & catalog...".to_owned();
    progress.finished = false;
    progress.splash_elapsed_secs = 0.0;
    next_state.set_if_different(AppState::PreloadConfigs);
}

fn poll_config_preload(
    time: Res<Time>,
    mut ctx: PreloadStateCtx,
    mut lang_task_opt: Option<ResMut<PendingMultilangTask>>,
) {
    if *ctx.state.get() == AppState::PreloadConfigs && ctx.configs.loaded_small {
        ctx.next_state.set_if_different(AppState::LauncherSplash);
    }

    if let Some(ref mut holder) = lang_task_opt
        && let Some(ref mut task) = holder.0
        && let Some(res) = block_on(poll_once(task))
    {
        match res {
            Ok((map, items, catalog, starter_icons, ml_names)) => {
                ctx.l10n.by_key = map;
                ctx.configs.item_catalog_count = items.len();
                ctx.configs.item_cfgs = items;
                ctx.configs.weapon_catalog = catalog;
                ctx.configs.starter_item_icons = starter_icons;
                ctx.configs.multilang_names = ml_names;
            }
            Err(err) => ctx.configs.warnings.push(err),
        }
        ctx.configs.loaded_multilang = true;
        holder.0 = None;
        ctx.progress.fraction = 1.0;
        ctx.progress.stage_label = ctx.l10n.tr("加载中 . . .").to_owned();
        ctx.progress.finished = true;
    }

    if *ctx.state.get() == AppState::LauncherSplash && ctx.progress.finished {
        ctx.progress.splash_elapsed_secs += time.delta_secs().min(0.05);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_res_config_and_server_list_xml() {
        let res = parse_res_config("ResRootPath = res/\nIsOpenResDivide = false\n");
        assert_eq!(res.res_root_path, "res/");
        assert!(!res.is_open_res_divide);

        let xml = r#"<root>
            <server id="167" type="0" name="Lite_review" gateurl="gate-review.wmcqlite.com" port="9211" status="0" shenhe="1"/>
            <region id ="1" name="Recommended" serverList="51"/>
        </root>"#;
        let list = parse_server_list_xml(xml);
        assert_eq!(list.servers.len(), 1);
        assert_eq!(list.servers[0].id, 167);
        assert_eq!(list.servers[0].mode, ServerModeType::Building);
        assert_eq!(list.servers[0].port, 9211);
        assert_eq!(list.servers[0].status, ServerStatus::Recommended);
        assert_eq!(list.regions.len(), 1);
    }

    #[test]
    fn loads_real_exported_config_files_from_assets() {
        let cfg_dir = Path::new("assets/config");
        let small = load_small_configs_from_dir(cfg_dir);
        assert_eq!(small.version.str_v, "3.14.0.0");
        assert!(
            small.server_list.servers.iter().any(|s| s.id == 167),
            "serverList.xml must include server 167"
        );
        assert!(
            !small.lang_indices.is_empty(),
            "MultiLanguageIndexCfg must load"
        );
        assert!(!small.close_weapons.is_empty(), "CloseWeaponCfg must load");
        assert!(small.warnings.is_empty(), "warnings: {:?}", small.warnings);

        let map = load_multilang_map_from_dir(cfg_dir).expect("MultiLanguageCfg.json must load");
        let l10n = LocalizationDb {
            index_entries: small.lang_indices,
            by_key: map,
            selected_lang_index: 8,
        };
        assert_eq!(l10n.tr("开始游戏"), "Почати гру");
        assert_eq!(small.establish_roles.len(), 16);
        assert!(!small.guns.is_empty());
        assert!(!small.skin_colors.is_empty());
        assert!(!small.role_heads.is_empty());
        assert!(!small.role_fashions.is_empty());
        assert!(!small.turret_cfgs.is_empty(), "TurretCfg must load");
        assert!(!small.monster_cfgs.is_empty(), "MonsterCfg must load");
        assert!(
            !small.port_boss_cfgs.is_empty(),
            "PortBossMonsterCfg must load"
        );
        assert!(!small.vehicle_cfgs.is_empty(), "VehicleCfg must load");
        assert!(!small.car_cfgs.is_empty(), "CarCfg must load");
        assert!(
            !small.electric_turret_cfgs.is_empty(),
            "ElectricTurretCfg must load"
        );
        assert!(
            !small.battery_cfgs.is_empty(),
            "BatteryCapacityCfg must load"
        );
        assert!(!small.shortcut_keys.is_empty(), "ShortCutKeysCfg must load");
        assert!(!small.pc_key_codes.is_empty(), "PCKeyCodeCfg must load");
        assert!(
            small.load_panels.len() >= 3,
            "LoadPanelCfg must load 3 backdrops"
        );
        assert!(
            small.strings.contains_key(&435),
            "Strings must contain ID 435"
        );
        assert!(
            small.strings.contains_key(&436),
            "Strings must contain ID 436"
        );
        assert!(
            small.strings.contains_key(&445),
            "Strings must contain ID 445"
        );
        assert!(
            small.strings.contains_key(&446),
            "Strings must contain ID 446"
        );
        assert!(
            !small.battle_loading_tips.is_empty(),
            "BattleLoadingTipsCfg must load"
        );
        assert_eq!(
            GameConfigs::resolve_load_panel_texture_asset("texture/loading_8").as_deref(),
            Some("ui/Loading_8.png")
        );

        let gc = GameConfigs {
            strings: small.strings,
            ..default()
        };
        let raw_l10n = LocalizationDb {
            selected_lang_index: -1,
            ..default()
        };
        assert_eq!(
            gc.get_string(&raw_l10n, 436, &["1.00M", "2.00M"]),
            "1.00M/2.00M"
        );

        let (items, catalog, starter_icons) = build_weapon_catalog_from_dir(
            cfg_dir,
            &small.guns,
            &small.close_weapons,
            &["wq_m4_1".to_owned(), "wq_bq_1".to_owned()],
        );
        assert!(items.len() > 10_000, "ItemCfg.json must load 16k+ items");
        assert_eq!(small.drawing_cfgs.len(), 315);
        let bandage = items.iter().find(|item| item.id == 55101).unwrap();
        assert_eq!(bandage.child_type, 32);
        assert_eq!(bandage.max_pile_num, 10);
        assert_eq!(bandage.extras[0], 20.0);
        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog[0].item_id, 52231);
        assert_eq!(
            starter_icons.get(&20500).map(String::as_str),
            Some("head1/man_fa_2_1")
        );
        assert!(!small.equip_cfgs.is_empty(), "EquipCfg.json must load");
        assert!(
            !small.cn_name_pools[1].is_empty(),
            "Name2Cfg.json must load"
        );
    }

    #[test]
    fn roundtrips_player_profile_save_json() {
        let tmp = std::env::temp_dir().join("bevy_zero_game_profile_test.json");
        let _ = std::fs::remove_file(&tmp);

        let profile = PlayerProfileSave {
            account_name: "Andlua_Test".to_owned(),
            sex_male: false,
            equipped_prop_idx: 6,
            gold: 9999,
            fps_tier: 3,
            music_volume: 0.25,
            ..Default::default()
        };
        profile
            .save_to_disk(&tmp)
            .expect("save_to_disk must succeed");
        let loaded = PlayerProfileSave::load_from_disk_or_default(&tmp);
        assert_eq!(profile, loaded);

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn role_snapshots_isolate_accounts_and_servers_and_keep_preferences() {
        let mut profile = PlayerProfileSave {
            music_volume: 0.23,
            ..Default::default()
        };
        assert!(!profile.activate_role("Account_A", 10));
        assert!(profile.role_name.is_empty());
        profile.role_name = "角色A".to_owned();
        profile.sex_male = false;
        profile.gold = 725;
        profile.complete_role_creation("Account_A", 10);
        profile.role_name = "Renamed".to_owned();
        assert!(!profile.activate_role("Account_B", 10));
        assert!(!profile.has_saved_role("Account_B", 10));
        assert!(profile.role_name.is_empty());
        assert!(profile.sex_male);
        profile.role_name = "Role_B".to_owned();
        profile.complete_role_creation("Account_B", 10);
        assert!(!profile.activate_role("Account_A", 11));
        assert!(profile.activate_role("Account_A", 10));
        assert_eq!(profile.account_name, "Account_A");
        assert_eq!(profile.role_name, "Renamed");
        assert!(!profile.sex_male);
        assert_eq!(profile.gold, 725);
        assert_eq!(profile.music_volume, 0.23);
        assert!(profile.activate_role("Account_B", 10));
        assert_eq!(profile.role_name, "Role_B");
    }

    #[test]
    fn legacy_role_migrates_only_to_its_single_recorded_owner_once() {
        let raw = r#"{"account_name":"OldNickname","created_roles":["Account_A"],"gold":4321}"#;
        let mut profile = PlayerProfileSave::decode_json(raw).expect("legacy save");
        assert_eq!(profile.account_name, "Account_A");
        assert!(!profile.has_saved_role("Account_B", 10));
        assert!(profile.activate_role("Account_A", 10));
        assert_eq!(profile.role_name, "OldNickname");
        assert_eq!(profile.gold, 4321);
        assert!(!profile.has_saved_role("Account_A", 11));
        let raw = serde_json::to_string(&profile).expect("serialized migrated save");
        let restored = PlayerProfileSave::decode_json(&raw).expect("schema 2");
        assert_eq!(restored, profile);
    }

    #[test]
    fn ambiguous_legacy_ownership_preserves_data_without_granting_a_role() {
        let raw = r#"{"account_name":"OldNickname","created_roles":["A","B"],"gold":4321}"#;
        let mut profile = PlayerProfileSave::decode_json(raw).expect("legacy save");
        assert!(!profile.has_saved_role("A", 10));
        assert!(!profile.has_saved_role("B", 10));
        assert!(!profile.activate_role("A", 10));
        let legacy = profile
            .unassigned_legacy_role
            .as_ref()
            .expect("retained role data");
        assert_eq!(legacy.role_name, "OldNickname");
        assert_eq!(legacy.gold, 4321);
    }

    #[test]
    fn saving_snapshots_active_role_and_future_schema_is_preserved() {
        let tmp = std::env::temp_dir().join("bevy_zero_game_role_snapshot_test.json");
        let mut profile = PlayerProfileSave::default();
        profile.activate_role("Account_A", 10);
        profile.role_name = "Created".to_owned();
        profile.complete_role_creation("Account_A", 10);
        profile.role_name = "Renamed".to_owned();
        profile.save_to_disk(&tmp).expect("save active role");
        let mut restored = PlayerProfileSave::load_from_disk_or_default(&tmp);
        assert!(restored.activate_role("Account_A", 10));
        assert_eq!(restored.role_name, "Renamed");
        let future = r#"{"schema_version":3,"account_name":"FutureAccount"}"#;
        std::fs::write(&tmp, future).expect("future fixture");
        let rejected = PlayerProfileSave::load_from_disk_or_default(&tmp);
        assert!(rejected.save_to_disk(&tmp).is_err());
        assert_eq!(
            std::fs::read_to_string(&tmp).expect("preserved future save"),
            future
        );
        let _ = std::fs::remove_file(tmp);
    }

    #[test]
    fn roundtrips_base_save_and_keybinds() {
        let tmp = std::env::temp_dir().join("bevy_zero_game_base_save_test.json");
        let _ = std::fs::remove_file(&tmp);

        let base = BaseSaveData {
            version: 1,
            wood: 420,
            stone: 210,
            iron: 88,
            fiber: 55,
            ammo: 120,
            bandages: 4,
            generator_fuel: 750,
            structures: vec![SavedStructureEntry {
                kind: 8,
                tier: 1,
                x: 4.2,
                y: 12.0,
                z: -8.4,
                yaw_steps: 2,
                hp: 300,
                max_hp: 300,
                is_open: true,
                fuel: 750,
            }],
            ..Default::default()
        };
        base.save_atomic(&tmp).expect("save_atomic must succeed");
        let loaded = BaseSaveData::load_from_disk(&tmp).expect("load_from_disk must decode");
        assert_eq!(base, loaded);

        let mut kb = KeybindProfile::preset(1);
        assert_eq!(kb.interact_key, "KeyE");
        assert_eq!(
            KeybindProfile::keycode(&kb.interact_key),
            bevy::prelude::KeyCode::KeyE
        );
        kb.cycle_interact_key();
        assert_eq!(kb.interact_key, "KeyO");

        let _ = std::fs::remove_file(&tmp);
    }
}
