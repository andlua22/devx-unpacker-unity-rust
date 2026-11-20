//! Native DrawingCfg quantities and the packaged mock server's FIFO production timing.
//!
//! Source: `memory/evidence/crafting-source-2026-11-17/`. This aggregate inventory
//! preserves item IDs/counts, while original bag slots, durability and server storage
//! remain separate work. Charging all requested materials on enqueue is a local
//! transaction policy; the recovered mock cost-mutation path is incomplete.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::config::{
    DecodedOcFile, DrawingCfgEntry, GameConfigs, ItemCfgEntry, resolve_assets_root,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemAmount {
    pub item_id: i32,
    pub num: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemInventory {
    counts: BTreeMap<i32, i32>,
}

impl ItemInventory {
    pub fn amount(&self, item_id: i32) -> i32 {
        self.counts.get(&item_id).copied().unwrap_or(0)
    }

    pub fn counts(&self) -> &BTreeMap<i32, i32> {
        &self.counts
    }

    /// Validates a snapshot without accepting duplicate or nonpositive rows.
    pub fn from_counts(
        catalog: &CraftCatalog,
        counts: impl IntoIterator<Item = (i32, i32)>,
    ) -> Result<Self, CraftError> {
        let mut inventory = Self::default();
        for (item_id, num) in counts {
            if inventory.counts.contains_key(&item_id) {
                return Err(CraftError::DuplicateItem(item_id));
            }
            inventory.add(catalog, item_id, num)?;
        }
        Ok(inventory)
    }

    pub fn add(
        &mut self,
        catalog: &CraftCatalog,
        item_id: i32,
        num: i32,
    ) -> Result<(), CraftError> {
        self.add_atomic(catalog, &[ItemAmount { item_id, num }])
    }

    pub fn add_atomic(
        &mut self,
        catalog: &CraftCatalog,
        amounts: &[ItemAmount],
    ) -> Result<(), CraftError> {
        let totals = aggregate_amounts(amounts)?;
        for (&item_id, &num) in &totals {
            if catalog.item(item_id).is_none() {
                return Err(CraftError::UnknownItem(item_id));
            }
            self.amount(item_id)
                .checked_add(num)
                .ok_or(CraftError::QuantityOverflow)?;
        }
        for (item_id, num) in totals {
            self.counts.insert(item_id, self.amount(item_id) + num);
        }
        Ok(())
    }

    /// Checks every merged ingredient before removing any amount.
    pub fn consume_atomic(&mut self, amounts: &[ItemAmount]) -> Result<(), CraftError> {
        let totals = aggregate_amounts(amounts)?;
        for (&item_id, &num) in &totals {
            let available = self.amount(item_id);
            if available < num {
                return Err(CraftError::InsufficientMaterials {
                    item_id,
                    required: num,
                    available,
                });
            }
        }
        for (item_id, num) in totals {
            let remaining = self.amount(item_id) - num;
            if remaining == 0 {
                self.counts.remove(&item_id);
            } else {
                self.counts.insert(item_id, remaining);
            }
        }
        Ok(())
    }
}

fn aggregate_amounts(amounts: &[ItemAmount]) -> Result<BTreeMap<i32, i32>, CraftError> {
    let mut totals = BTreeMap::<i32, i32>::new();
    for amount in amounts {
        if amount.item_id <= 0 || amount.num <= 0 {
            return Err(CraftError::InvalidQuantity);
        }
        let total = totals.entry(amount.item_id).or_default();
        *total = total
            .checked_add(amount.num)
            .ok_or(CraftError::QuantityOverflow)?;
    }
    Ok(totals)
}

#[derive(Debug, Clone, Default)]
pub struct CraftCatalog {
    pub items: BTreeMap<i32, ItemCfgEntry>,
    pub drawings: BTreeMap<i32, DrawingCfgEntry>,
}

impl CraftCatalog {
    pub fn from_configs(configs: &GameConfigs) -> Self {
        Self {
            items: configs
                .item_cfgs
                .iter()
                .cloned()
                .map(|r| (r.id, r))
                .collect(),
            drawings: configs
                .drawing_cfgs
                .iter()
                .cloned()
                .map(|r| (r.id, r))
                .collect(),
        }
    }

    pub fn load_default() -> Result<Self, CraftError> {
        let directory = resolve_assets_root().join("config");
        let item_rows: DecodedOcFile<ItemCfgEntry> = serde_json::from_str(
            &std::fs::read_to_string(directory.join("cfg.ItemCfg.json"))
                .map_err(|e| CraftError::Config(e.to_string()))?,
        )
        .map_err(|e| CraftError::Config(e.to_string()))?;
        let drawing_rows: DecodedOcFile<DrawingCfgEntry> = serde_json::from_str(
            &std::fs::read_to_string(directory.join("cfg.DrawingCfg.json"))
                .map_err(|e| CraftError::Config(e.to_string()))?,
        )
        .map_err(|e| CraftError::Config(e.to_string()))?;
        let mut items = BTreeMap::new();
        for item in item_rows.records {
            if item.id <= 0 || items.contains_key(&item.id) {
                return Err(CraftError::Config("Invalid or duplicate ItemCfg ID".into()));
            }
            items.insert(item.id, item);
        }
        let mut drawings = BTreeMap::new();
        for drawing in drawing_rows.records {
            if drawing.id <= 0 || drawings.contains_key(&drawing.id) {
                return Err(CraftError::Config(
                    "Invalid or duplicate DrawingCfg ID".into(),
                ));
            }
            drawings.insert(drawing.id, drawing);
        }
        let catalog = Self { items, drawings };
        for drawing in catalog.drawings.values() {
            catalog.materials_for(drawing, 1)?;
            catalog.output_for(drawing)?;
            if drawing.need_time <= 0 || drawing.once_max_create <= 0 {
                return Err(CraftError::Config(format!(
                    "Invalid production limits for drawing {}",
                    drawing.id
                )));
            }
        }
        Ok(catalog)
    }

    pub fn item(&self, item_id: i32) -> Option<&ItemCfgEntry> {
        self.items.get(&item_id)
    }

    pub fn drawing(&self, drawing_id: i32) -> Option<&DrawingCfgEntry> {
        self.drawings.get(&drawing_id)
    }

    /// Per-stack metadata only: aggregate inventory does not infer native bag slots.
    pub fn stack_limit(&self, item_id: i32) -> Result<i32, CraftError> {
        let item = self.item(item_id).ok_or(CraftError::UnknownItem(item_id))?;
        Ok(if item.is_pile_able {
            item.max_pile_num.max(1)
        } else {
            1
        })
    }

    pub fn materials_for(
        &self,
        drawing: &DrawingCfgEntry,
        create_num: i32,
    ) -> Result<Vec<ItemAmount>, CraftError> {
        if create_num <= 0 || create_num > drawing.once_max_create {
            return Err(CraftError::InvalidQuantity);
        }
        let mut amounts = Vec::with_capacity(drawing.material.len());
        for material in &drawing.material {
            if self.item(material.item_id).is_none() {
                return Err(CraftError::UnknownItem(material.item_id));
            }
            if material.num <= 0 {
                return Err(CraftError::InvalidQuantity);
            }
            amounts.push(ItemAmount {
                item_id: material.item_id,
                num: material
                    .num
                    .checked_mul(create_num)
                    .ok_or(CraftError::QuantityOverflow)?,
            });
        }
        Ok(aggregate_amounts(&amounts)?
            .into_iter()
            .map(|(item_id, num)| ItemAmount { item_id, num })
            .collect())
    }

    fn output_for(&self, drawing: &DrawingCfgEntry) -> Result<ItemAmount, CraftError> {
        if self.item(drawing.target_item_id).is_none() {
            return Err(CraftError::UnknownItem(drawing.target_item_id));
        }
        if drawing.product_num <= 0 {
            return Err(CraftError::InvalidQuantity);
        }
        Ok(ItemAmount {
            item_id: drawing.target_item_id,
            num: drawing.product_num,
        })
    }

    /// Native TypeCheck precedes category filtering, then unlocked drawings sort first.
    pub fn visible_drawings(&self, context: &CraftContext) -> Vec<&DrawingCfgEntry> {
        let mut drawings: Vec<_> = self
            .drawings
            .values()
            .filter(|drawing| self.type_check(drawing, context) && context.is_visible(drawing))
            .collect();
        drawings.sort_by_key(|drawing| (context.is_locked(drawing), drawing.order, drawing.id));
        drawings
    }

    pub fn type_check(&self, drawing: &DrawingCfgEntry, context: &CraftContext) -> bool {
        if context.mode == CraftMode::Nostalgia && !drawing.is_nostalgia_show_locked { return false; }
        let child_type = self.item(drawing.target_item_id).map(|item| item.child_type);
        if child_type == Some(181) { return context.mode == CraftMode::Civilization; }
        if child_type == Some(97) && context.mode == CraftMode::Civilization { return false; }
        drawing.battle_type_limit == 8 || ((drawing.battle_type_limit == 1) == (context.battle_mode == 9))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CraftMode {
    #[default]
    Island,
    Civilization,
    Firepower,
    Nostalgia,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkbenchAccess {
    pub facility_id: i64,
    pub kind: i32,
    pub level: i32,
}

#[derive(Debug, Clone)]
pub struct CraftContext {
    pub mode: CraftMode,
    /// CurBsInfo.mode is independent of the civilization config (native IDs 13/14).
    /// Zero describes the replacement island until original battle startup is restored.
    pub battle_mode: i32,
    pub learned_drawings: BTreeSet<i32>,
    /// Caller supplies facilities already found by the authoritative proximity query.
    pub workbenches: Vec<WorkbenchAccess>,
    pub toolbox_level: i32,
    /// Actual permitted count; native unpatched `Count > 4` permits five entries.
    pub queue_limit: usize,
}

impl Default for CraftContext {
    fn default() -> Self {
        Self {
            mode: CraftMode::Island,
            battle_mode: 0,
            learned_drawings: BTreeSet::new(),
            workbenches: Vec::new(),
            toolbox_level: 0,
            queue_limit: usize::MAX,
        }
    }
}

impl CraftContext {
    pub fn is_default(&self, drawing: &DrawingCfgEntry) -> bool {
        match self.mode {
            CraftMode::Island => drawing.is_default,
            CraftMode::Civilization => drawing.is_civi_default,
            CraftMode::Firepower => drawing.is_fire_power_default,
            CraftMode::Nostalgia => drawing.is_nostalgia_default,
        }
    }

    /// ScProduceMgr.IsLocked ignores roleLevel in its unpatched native branch.
    pub fn is_locked(&self, drawing: &DrawingCfgEntry) -> bool {
        !self.is_default(drawing) && !self.learned_drawings.contains(&drawing.id)
    }

    pub fn is_visible(&self, drawing: &DrawingCfgEntry) -> bool {
        let show_locked = match self.mode {
            CraftMode::Island => drawing.is_show_locked,
            CraftMode::Civilization => drawing.is_civi_show_locked,
            CraftMode::Firepower => drawing.is_fire_power_show_locked,
            CraftMode::Nostalgia => drawing.is_nostalgia_show_locked,
        };
        show_locked || !self.is_locked(drawing)
    }

    pub fn workbench_requirement(&self, drawing: &DrawingCfgEntry) -> (i32, i32) {
        if self.mode == CraftMode::Civilization {
            (
                drawing.need_civilized_workbench,
                drawing.need_civilized_workbench_level,
            )
        } else {
            (drawing.need_workbench, drawing.need_workbench_level)
        }
    }

    pub fn facility_for(&self, drawing: &DrawingCfgEntry) -> Result<i64, CraftError> {
        // Native MeetWorkbenchCondition checks this field before its mode branches.
        if drawing.need_workbench == 0 {
            return Ok(-1);
        }
        let (kind, level) = self.workbench_requirement(drawing);
        self.workbenches
            .iter()
            .find(|facility| facility.kind == kind && facility.level >= level)
            .map(|facility| facility.facility_id)
            .ok_or(CraftError::MissingWorkbench { kind, level })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CraftRequest {
    pub drawing_id: i32,
    pub create_num: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CraftQueueEntry {
    pub drawing_id: i32,
    pub remaining_count: i32,
    pub remaining_seconds: f64,
    pub batch_seconds: f64,
    pub facility_id: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CraftQueue {
    pub entries: VecDeque<CraftQueueEntry>,
}

impl CraftQueue {
    pub fn enqueue(
        &mut self,
        catalog: &CraftCatalog,
        inventory: &mut ItemInventory,
        request: CraftRequest,
        context: &CraftContext,
    ) -> Result<(), CraftError> {
        let drawing = catalog
            .drawing(request.drawing_id)
            .ok_or(CraftError::UnknownDrawing(request.drawing_id))?;
        if !catalog.type_check(drawing, context) { return Err(CraftError::UnavailableInMode(drawing.id)); }
        if context.is_locked(drawing) {
            return Err(CraftError::LockedDrawing(drawing.id));
        }
        if self.entries.len() >= context.queue_limit {
            return Err(CraftError::QueueFull);
        }
        if drawing.need_tool_box_level > context.toolbox_level {
            return Err(CraftError::MissingToolbox(drawing.need_tool_box_level));
        }
        let facility_id = context.facility_for(drawing)?;
        let materials = catalog.materials_for(drawing, request.create_num)?;
        let output = catalog.output_for(drawing)?;
        if drawing.need_time <= 0 {
            return Err(CraftError::InvalidQuantity);
        }
        output
            .num
            .checked_mul(request.create_num)
            .ok_or(CraftError::QuantityOverflow)?;
        inventory.consume_atomic(&materials)?;
        let batch_seconds = f64::from(drawing.need_time);
        self.entries.push_back(CraftQueueEntry {
            drawing_id: drawing.id,
            remaining_count: request.create_num,
            remaining_seconds: batch_seconds,
            batch_seconds,
            facility_id,
        });
        Ok(())
    }

    pub fn validate(&self, catalog: &CraftCatalog) -> Result<(), CraftError> {
        for entry in &self.entries {
            let drawing = catalog
                .drawing(entry.drawing_id)
                .ok_or(CraftError::UnknownDrawing(entry.drawing_id))?;
            if entry.remaining_count <= 0
                || entry.remaining_count > drawing.once_max_create
                || drawing.need_time <= 0
                || entry.batch_seconds != f64::from(drawing.need_time)
                || !entry.remaining_seconds.is_finite()
                || entry.remaining_seconds < 0.0
                || entry.remaining_seconds > entry.batch_seconds
            {
                return Err(CraftError::InvalidQueue);
            }
            catalog.output_for(drawing)?;
        }
        Ok(())
    }

    /// Consumes a supplied simulation interval, preserving leftover time across FIFO items.
    /// Grants and queue progress commit together; a failed interval changes neither.
    pub fn advance(
        &mut self,
        catalog: &CraftCatalog,
        inventory: &mut ItemInventory,
        elapsed_seconds: f64,
    ) -> Result<Vec<ItemAmount>, CraftError> {
        if !elapsed_seconds.is_finite() || elapsed_seconds < 0.0 {
            return Err(CraftError::InvalidElapsed);
        }
        self.validate(catalog)?;
        if self.entries.is_empty() {
            return Ok(Vec::new());
        }
        let mut next_queue = self.clone();
        let mut next_inventory = inventory.clone();
        let mut remaining_time = elapsed_seconds;
        let mut granted = Vec::new();
        while let Some(front) = next_queue.entries.front_mut() {
            if remaining_time < front.remaining_seconds {
                front.remaining_seconds -= remaining_time;
                break;
            }
            let drawing = catalog
                .drawing(front.drawing_id)
                .ok_or(CraftError::UnknownDrawing(front.drawing_id))?;
            let output = catalog.output_for(drawing)?;
            next_inventory.add(catalog, output.item_id, output.num)?;
            remaining_time -= front.remaining_seconds;
            front.remaining_count -= 1;
            granted.push(output);
            if front.remaining_count == 0 {
                next_queue.entries.pop_front();
            } else {
                front.remaining_seconds = front.batch_seconds;
            }
        }
        *self = next_queue;
        *inventory = next_inventory;
        Ok(granted)
    }
}

/// ScProduceMgr.GetCreateNeedTime attributes 80000/80001, and 80002 for childType 143.
/// This client estimate is separate from the packaged mock's raw DrawingCfg.needTime.
#[allow(dead_code)] // Native status modifiers are retained until their live attribute source is restored.
pub fn modified_create_seconds(
    old_time: f32,
    child_type: i32,
    proportional_reduction: f32,
    flat_reduction: f32,
    vehicle_reduction: f32,
) -> f32 {
    let reduction = proportional_reduction
        + if child_type == 143 {
            vehicle_reduction
        } else {
            0.0
        };
    ((1.0 - reduction).max(0.0) * old_time - flat_reduction).max(1.0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CraftError {
    UnknownItem(i32),
    UnknownDrawing(i32),
    DuplicateItem(i32),
    InvalidQuantity,
    QuantityOverflow,
    InsufficientMaterials {
        item_id: i32,
        required: i32,
        available: i32,
    },
    LockedDrawing(i32),
    UnavailableInMode(i32),
    MissingWorkbench {
        kind: i32,
        level: i32,
    },
    MissingToolbox(i32),
    QueueFull,
    InvalidElapsed,
    InvalidQueue,
    Config(String),
}

impl fmt::Display for CraftError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownItem(id) => write!(formatter, "Unknown item {id}"),
            Self::UnknownDrawing(id) => write!(formatter, "Unknown recipe {id}"),
            Self::DuplicateItem(id) => write!(formatter, "Duplicate inventory item {id}"),
            Self::InvalidQuantity => formatter.write_str("Invalid production quantity"),
            Self::QuantityOverflow => formatter.write_str("Item quantity exceeds its limit"),
            Self::InsufficientMaterials {
                item_id,
                required,
                available,
            } => {
                write!(
                    formatter,
                    "Item {item_id}: requires {required}, available {available}"
                )
            }
            Self::LockedDrawing(_) => formatter.write_str("Recipe has not been learned"),
            Self::UnavailableInMode(_) => formatter.write_str("Recipe is unavailable in this battle mode"),
            Self::MissingWorkbench { kind, level } => {
                write!(formatter, "Requires workbench type {kind}, level {level}")
            }
            Self::MissingToolbox(level) => write!(formatter, "Requires toolbox level {level}"),
            Self::QueueFull => formatter.write_str("Production queue is full"),
            Self::InvalidElapsed => formatter.write_str("Invalid production time step"),
            Self::InvalidQueue => formatter.write_str("Invalid production queue"),
            Self::Config(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for CraftError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    fn catalog() -> &'static CraftCatalog {
        static CATALOG: OnceLock<CraftCatalog> = OnceLock::new();
        CATALOG.get_or_init(|| CraftCatalog::load_default().unwrap())
    }

    fn inventory(amounts: &[(i32, i32)]) -> ItemInventory {
        ItemInventory::from_counts(catalog(), amounts.iter().copied()).unwrap()
    }

    fn request(drawing_id: i32, create_num: i32) -> CraftRequest {
        CraftRequest {
            drawing_id,
            create_num,
        }
    }

    #[test]
    fn complete_native_drawing_rows_roundtrip_and_join_item_catalog() {
        let catalog = catalog();
        assert_eq!(catalog.drawings.len(), 315);
        assert!(catalog.items.len() > 16_000);
        let raw: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(resolve_assets_root().join("config/cfg.DrawingCfg.json"))
                .unwrap(),
        )
        .unwrap();
        for row in raw["records"].as_array().unwrap() {
            let drawing = catalog.drawing(row["id"].as_i64().unwrap() as i32).unwrap();
            assert_eq!(serde_json::to_value(drawing).unwrap(), *row);
            assert!(catalog.item(drawing.target_item_id).is_some());
            assert!(
                drawing
                    .material
                    .iter()
                    .all(|m| catalog.item(m.item_id).is_some())
            );
        }
        assert_eq!(
            catalog
                .drawings
                .values()
                .filter(|row| row.is_default)
                .count(),
            42
        );
    }

    #[test]
    fn native_stack_metadata_preserves_per_stack_sizes_without_total_caps() {
        let catalog = catalog();
        assert_eq!(catalog.stack_limit(50003).unwrap(), 1000);
        assert_eq!(catalog.stack_limit(55101).unwrap(), 10);
        assert_eq!(catalog.stack_limit(55001).unwrap(), 1);
        let mut bag = ItemInventory::default();
        bag.add(catalog, 55101, 21).unwrap();
        assert_eq!(bag.amount(55101), 21);
        assert_eq!(bag.amount(50003), 0);
    }

    #[test]
    fn native_axe_and_bandage_costs_complete_once_per_batch_in_fifo_order() {
        let catalog = catalog();
        let context = CraftContext::default();
        let mut bag = inventory(&[(50001, 150), (50006, 75), (50003, 5)]);
        let mut queue = CraftQueue::default();
        queue
            .enqueue(catalog, &mut bag, request(57089, 2), &context)
            .unwrap();
        queue
            .enqueue(catalog, &mut bag, request(57100, 1), &context)
            .unwrap();
        assert_eq!(bag.amount(50001), 50);
        assert_eq!(bag.amount(50006), 25);
        assert_eq!(bag.amount(50003), 0);
        assert_eq!(queue.entries[0].batch_seconds, 8.0);
        assert_eq!(queue.entries[1].batch_seconds, 1.0);
        assert!(queue.advance(catalog, &mut bag, 7.0).unwrap().is_empty());
        assert_eq!(bag.amount(55001), 0);
        assert_eq!(
            queue.advance(catalog, &mut bag, 1.0).unwrap(),
            vec![ItemAmount {
                item_id: 55001,
                num: 1
            }]
        );
        assert_eq!(queue.entries[0].remaining_count, 1);
        assert_eq!(
            queue.advance(catalog, &mut bag, 9.0).unwrap(),
            vec![
                ItemAmount {
                    item_id: 55001,
                    num: 1
                },
                ItemAmount {
                    item_id: 55101,
                    num: 1
                },
            ]
        );
        assert_eq!(bag.amount(55001), 2);
        assert_eq!(bag.amount(55101), 1);
        assert!(queue.entries.is_empty());
        assert!(queue.advance(catalog, &mut bag, 100.0).unwrap().is_empty());
        assert_eq!(bag.amount(55001), 2);
    }

    #[test]
    fn ammo_uses_native_workbench_and_five_item_product_batch() {
        let catalog = catalog();
        let mut bag = inventory(&[(50008, 20), (50017, 10)]);
        let initial = bag.clone();
        let mut queue = CraftQueue::default();
        let mut context = CraftContext::default();
        assert_eq!(
            queue.enqueue(catalog, &mut bag, request(57021, 2), &context),
            Err(CraftError::MissingWorkbench { kind: 1, level: 2 })
        );
        assert_eq!(bag, initial);
        context.workbenches.push(WorkbenchAccess {
            facility_id: 42,
            kind: 1,
            level: 1,
        });
        assert!(
            queue
                .enqueue(catalog, &mut bag, request(57021, 2), &context)
                .is_err()
        );
        context.workbenches[0].level = 2;
        queue
            .enqueue(catalog, &mut bag, request(57021, 2), &context)
            .unwrap();
        assert_eq!(queue.entries[0].facility_id, 42);
        assert_eq!(bag.amount(50008), 0);
        assert_eq!(bag.amount(50017), 0);
        queue.advance(catalog, &mut bag, 1.0).unwrap();
        assert_eq!(bag.amount(53202), 5);
        queue.advance(catalog, &mut bag, 1.0).unwrap();
        assert_eq!(bag.amount(53202), 10);
    }

    #[test]
    fn invalid_requests_locked_drawings_and_shortages_do_not_mutate() {
        let catalog = catalog();
        let context = CraftContext::default();
        let mut bag = inventory(&[(50001, 49), (50006, 25), (50003, 10)]);
        let initial = bag.clone();
        let mut queue = CraftQueue::default();
        for requested in [
            request(1, 1),
            request(57100, 0),
            request(57100, -1),
            request(57100, 1000),
            request(57006, 1),
            request(57089, 1),
        ] {
            assert!(
                queue
                    .enqueue(catalog, &mut bag, requested, &context)
                    .is_err()
            );
            assert_eq!(bag, initial);
            assert!(queue.entries.is_empty());
        }
        assert_eq!(
            queue.enqueue(catalog, &mut bag, request(57089, 1), &context),
            Err(CraftError::InsufficientMaterials {
                item_id: 50001,
                required: 50,
                available: 49
            })
        );
    }

    #[test]
    fn atomic_inventory_combines_duplicate_requirements_and_rejects_overflow() {
        let catalog = catalog();
        let mut bag = inventory(&[(50001, 60)]);
        let initial = bag.clone();
        assert!(
            bag.consume_atomic(&[
                ItemAmount {
                    item_id: 50001,
                    num: 40
                },
                ItemAmount {
                    item_id: 50001,
                    num: 30
                },
            ])
            .is_err()
        );
        assert_eq!(bag, initial);
        bag.add(catalog, 50003, i32::MAX).unwrap();
        let initial = bag.clone();
        assert_eq!(
            bag.add_atomic(
                catalog,
                &[
                    ItemAmount {
                        item_id: 50001,
                        num: 1
                    },
                    ItemAmount {
                        item_id: 50003,
                        num: 1
                    },
                ]
            ),
            Err(CraftError::QuantityOverflow)
        );
        assert_eq!(bag, initial);
        assert!(bag.consume_atomic(&[]).is_ok());
        assert!(bag.add_atomic(catalog, &[]).is_ok());
        for num in [0, -1] {
            assert!(bag.add(catalog, 50001, num).is_err());
            assert!(
                bag.consume_atomic(&[ItemAmount {
                    item_id: 50001,
                    num
                }])
                .is_err()
            );
        }
        assert!(ItemInventory::from_counts(catalog, [(50001, 1), (50001, 2)]).is_err());
    }

    #[test]
    fn lock_predicate_uses_mode_defaults_and_learning_without_role_level_gate() {
        let catalog = catalog();
        let drawing = catalog.drawing(57008).unwrap();
        let mut context = CraftContext::default();
        assert!(context.is_locked(drawing));
        context.mode = CraftMode::Firepower;
        assert!(!context.is_locked(drawing));
        context.mode = CraftMode::Island;
        context.learned_drawings.insert(drawing.id);
        assert!(!context.is_locked(drawing));
        let mut drawing = catalog.drawing(57100).unwrap().clone();
        drawing.level_limit = i32::MAX;
        assert!(!context.is_locked(&drawing));
        for mode in [CraftMode::Civilization, CraftMode::Nostalgia] {
            context.mode = mode;
            assert!(!context.is_locked(&drawing));
        }
    }

    #[test]
    fn mock_guard_permits_fifth_entry_and_rejects_sixth_without_charging() {
        let catalog = catalog();
        let context = CraftContext {
            queue_limit: 5,
            ..CraftContext::default()
        };
        let mut bag = inventory(&[(50003, 30)]);
        let mut queue = CraftQueue::default();
        for _ in 0..5 {
            queue
                .enqueue(catalog, &mut bag, request(57100, 1), &context)
                .unwrap();
        }
        let before = bag.clone();
        assert_eq!(
            queue.enqueue(catalog, &mut bag, request(57100, 1), &context),
            Err(CraftError::QueueFull)
        );
        assert_eq!(bag, before);
        assert_eq!(queue.entries.len(), 5);
        queue.advance(catalog, &mut bag, 5.0).unwrap();
        assert_eq!(bag.amount(55101), 5);
    }

    #[test]
    fn invalid_or_overflowing_queue_interval_is_atomic() {
        let catalog = catalog();
        let mut bag = inventory(&[(50003, 5), (55101, i32::MAX)]);
        let mut queue = CraftQueue::default();
        queue
            .enqueue(
                catalog,
                &mut bag,
                request(57100, 1),
                &CraftContext::default(),
            )
            .unwrap();
        let before_bag = bag.clone();
        let before_queue = queue.clone();
        for elapsed in [1.0, -1.0, f64::INFINITY, f64::NAN] {
            assert!(queue.advance(catalog, &mut bag, elapsed).is_err());
            assert_eq!(bag, before_bag);
            assert_eq!(queue, before_queue);
        }
        queue.entries[0].batch_seconds = 0.5;
        let malformed = queue.clone();
        assert!(queue.advance(catalog, &mut bag, 1.0).is_err());
        assert_eq!(queue, malformed);
        assert_eq!(bag, before_bag);
    }

    #[test]
    fn client_time_estimate_applies_native_attributes_and_one_second_floor() {
        assert_eq!(modified_create_seconds(8.0, 82, 0.25, 1.0, 0.5), 5.0);
        assert_eq!(modified_create_seconds(8.0, 143, 0.25, 1.0, 0.5), 1.0);
        assert_eq!(modified_create_seconds(8.0, 143, 2.0, 0.0, 0.0), 1.0);
    }
}
