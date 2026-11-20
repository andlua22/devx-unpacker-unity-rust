//! Retained BagProducePage, using the APK controller arrays and cloned source cells.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::crafting::CraftMode;
use crate::ui::panel_loader::{UiAssetRefDto, UiPanelDoc};
use crate::ui::retained_cells::{CloneSpec, clone_cell, node_by_ref, node_by_ref_mut};
use crate::ui::widgets::{
    UiTextInput, UiTextInputChanged, UiTextInputPlaceholder, attach_text_input_observers,
};

const PAGE_CONTROLLER: i64 = 3_631_171_954_743_106_442;
const QUEUE_CONTROLLER: i64 = 954_703_693_475_046_728;
const REQUIREMENT_CONTROLLER: i64 = -2_297_361_939_496_587_935;
const RECIPE_CONTROLLER: i64 = -8_760_752_787_502_724_994;
const INPUT_TAG: &str = "native_craft_quantity";

// BagProducePage.OnInit registrations. Car17 is authored but not registered in native code.
const PRIMARY_TABS: [(i32, usize, usize); 6] = [
    (1, 16, 29),
    (12, 11, 36),
    (4, 57, 33),
    (13, 1, 38),
    (14, 22, 37),
    (18, 55, 56),
];
const SECONDARY_TABS: [(i32, usize); 19] = [
    (1, 17),
    (2, 20),
    (3, 13),
    (21, 19),
    (5, 4),
    (6, 7),
    (7, 8),
    (8, 23),
    (9, 2),
    (10, 21),
    (15, 15),
    (16, 6),
    (19, 54),
    (20, 53),
    (24, 59),
    (23, 58),
    (25, 63),
    (26, 62),
    (27, 64),
];

pub(super) fn secondary_tabs(primary: i32) -> &'static [i32] {
    match primary {
        1 => &[1],
        12 => &[2, 5, 6, 3],
        4 => &[4, 21, 16],
        13 => &[7, 8, 9, 10, 27],
        14 => &[15, 17, 23, 24],
        18 => &[19, 20, 25, 26],
        _ => &[],
    }
}

fn category_matches(drawing: &DrawingCfgEntry, context: &CraftContext, tab: i32) -> bool {
    if tab == 1 {
        match context.mode {
            CraftMode::Island => drawing.is_normal,
            CraftMode::Civilization => drawing.is_civi_normal,
            CraftMode::Firepower => drawing.is_fire_power_normal,
            CraftMode::Nostalgia => drawing.is_nostalgia,
        }
    } else {
        drawing.big_type == tab || drawing.small_type == tab
    }
}

pub(super) fn reset_category_selection(
    catalog: &CraftCatalog,
    survival: &mut SurvivalSessionState,
    inventory: &BTreeMap<i32, i32>,
) {
    let context = survival.craft_context();
    let selected = catalog
        .visible_drawings(&context)
        .into_iter()
        .find(|drawing| category_matches(drawing, &context, survival.craft_secondary_tab));
    survival.craft_selected_drawing = selected.map(|drawing| drawing.id);
    survival.craft_quantity = selected.map_or(0, |drawing| {
        maximum_craft_quantity(drawing, inventory).min(1)
    });
    survival.craft_selected_queue = None;
}

#[derive(Clone)]
struct Cell {
    root: Entity,
    slots: Vec<Entity>,
}

struct RecipeCell {
    drawing_id: i32,
    cell: Cell,
}

struct Radio {
    tab: i32,
    primary: bool,
    on: Entity,
    off: Entity,
}

#[derive(Component)]
pub(super) struct NativeCraftPanel {
    bound: BoundUiPanel,
    page: Vec<Entity>,
    recipes: Vec<RecipeCell>,
    fixed_materials: Vec<Cell>,
    scroll_materials: Vec<Cell>,
    queue: Vec<Cell>,
    radios: Vec<Radio>,
    recipe_content: Entity,
    material_content: Entity,
    recipe_view: [f32; 2],
    material_view: [f32; 2],
    recipe_step: [f32; 2],
    material_step: [f32; 2],
    recipe_art: [f32; 2],
    material_art: [f32; 2],
    columns: usize,
    last_tab: i32,
}

struct PreparedPanel {
    doc: UiPanelDoc,
    page_controller: UiAssetRefDto,
    recipes: Vec<(i32, CloneSpec)>,
    materials: Vec<CloneSpec>,
    recipe_content: UiAssetRefDto,
    material_content: UiAssetRefDto,
    recipe_view: [f32; 2],
    material_view: [f32; 2],
    recipe_step: [f32; 2],
    material_step: [f32; 2],
    recipe_art: [f32; 2],
    material_art: [f32; 2],
    columns: usize,
}

fn controller_ref(doc: &UiPanelDoc, path_id: i64) -> Result<UiAssetRefDto, String> {
    doc.script_bindings
        .iter()
        .filter_map(|binding| binding.owner_component_ref)
        .find(|reference| reference.path_id == path_id)
        .ok_or_else(|| format!("BagAndBuildPanel controller {path_id} is missing"))
}

fn slot_ref(
    doc: &UiPanelDoc,
    controller: UiAssetRefDto,
    slot: usize,
) -> Result<UiAssetRefDto, String> {
    doc.script_bindings
        .iter()
        .find(|binding| {
            binding.owner_component_ref == Some(controller)
                && binding.field_name == "GameObjects"
                && binding.slot_index == Some(slot)
        })
        .and_then(|binding| binding.target_go_ref)
        .ok_or_else(|| {
            format!(
                "BagAndBuildPanel controller {} slot {slot} is missing",
                controller.path_id
            )
        })
}

fn cell_controller(doc: &UiPanelDoc, root: UiAssetRefDto) -> Result<UiAssetRefDto, String> {
    doc.script_bindings
        .iter()
        .find(|binding| binding.owner_go_ref == Some(root) && binding.field_name == "GameObjects")
        .and_then(|binding| binding.owner_component_ref)
        .ok_or_else(|| {
            format!(
                "BagAndBuildPanel cell {} controller is missing",
                root.path_id
            )
        })
}

fn prepare_panel(mut doc: UiPanelDoc, catalog: &CraftCatalog) -> Result<PreparedPanel, String> {
    let page_controller = controller_ref(&doc, PAGE_CONTROLLER)?;
    let root_controller = cell_controller(
        &doc,
        doc.root
            .go_ref
            .ok_or("BagAndBuildPanel root PPtr is missing")?,
    )?;
    let recipe_controller = controller_ref(&doc, RECIPE_CONTROLLER)?;
    // File ordinals are catalog-local; use each current document's full PPtr.
    for slot in [1, 5, 7, 9, 10, 12] {
        let reference = slot_ref(&doc, root_controller, slot)?;
        if let Some(node) = node_by_ref_mut(&mut doc.root, reference) {
            node.active_self = false;
        }
    }
    let recipe_ref = slot_ref(&doc, page_controller, 26)?;
    let material_ref = slot_ref(&doc, page_controller, 65)?;
    let recipe_template = node_by_ref(&doc.root, recipe_ref)
        .cloned()
        .ok_or("Native recipe template is missing")?;
    let material_template = node_by_ref(&doc.root, material_ref)
        .cloned()
        .ok_or("Native material template is missing")?;
    let material_bg = material_template
        .children
        .first()
        .and_then(|node| node.go_ref)
        .ok_or("Material frame PPtr is missing")?;
    let material_controller = cell_controller(&doc, material_bg)?;
    let recipe_targets = (0..11)
        .map(|slot| slot_ref(&doc, recipe_controller, slot))
        .collect::<Result<Vec<_>, _>>()?;
    let material_targets = (0..5)
        .map(|slot| slot_ref(&doc, material_controller, slot))
        .collect::<Result<Vec<_>, _>>()?;
    let scroll = |slot| -> Result<_, String> {
        let reference = slot_ref(&doc, page_controller, slot)?;
        let node = node_by_ref(&doc.root, reference).ok_or("Native scroll node is missing")?;
        let props = node
            .widget_props
            .as_ref()
            .ok_or("Native UIScrollPanel metadata is missing; re-export the panel with DevX")?;
        let step = props
            .scroll_cell_size
            .ok_or("Native UIScrollPanel cell size is missing; re-export the panel with DevX")?;
        let columns = props
            .scroll_cols
            .ok_or("Native UIScrollPanel columns are missing")?;
        let content = node
            .children
            .first()
            .and_then(|child| child.go_ref)
            .ok_or("Native scroll Content PPtr is missing")?;
        if step.iter().any(|value| !value.is_finite() || *value <= 0.0) || columns < 1 {
            return Err("Invalid native UIScrollPanel dimensions".into());
        }
        Ok((
            content,
            [node.canvas_rect[2], node.canvas_rect[3]],
            step,
            usize::try_from(columns).map_err(|_| "Invalid columns")?,
        ))
    };
    let (recipe_content, recipe_view, recipe_step, columns) = scroll(39)?;
    let (material_content, material_view, material_step, _) = scroll(60)?;
    let mut recipes = Vec::new();
    let mut recipe_nodes = Vec::new();
    for drawing in catalog.drawings.values() {
        let (node, spec) = clone_cell(
            &recipe_template,
            &recipe_targets,
            format!("native_recipe_{}", drawing.id),
        )?;
        recipe_nodes.push(node);
        recipes.push((drawing.id, spec));
    }
    let mut materials = Vec::new();
    let mut material_nodes = Vec::new();
    let material_count = catalog
        .drawings
        .values()
        .map(|drawing| drawing.material.len())
        .max()
        .unwrap_or(0);
    for index in 0..material_count {
        let (node, spec) = clone_cell(
            &material_template,
            &material_targets,
            format!("native_material_{index}"),
        )?;
        material_nodes.push(node);
        materials.push(spec);
    }
    node_by_ref_mut(&mut doc.root, recipe_content)
        .ok_or("Recipe Content is missing")?
        .children = recipe_nodes;
    node_by_ref_mut(&mut doc.root, material_content)
        .ok_or("Material Content is missing")?
        .children = material_nodes;
    Ok(PreparedPanel {
        doc,
        page_controller,
        recipes,
        materials,
        recipe_content,
        material_content,
        recipe_view,
        material_view,
        recipe_step,
        material_step,
        columns,
        recipe_art: [
            recipe_template.canvas_rect[2],
            recipe_template.canvas_rect[3],
        ],
        material_art: [
            material_template.canvas_rect[2],
            material_template.canvas_rect[3],
        ],
    })
}

fn bound_cell(
    bound: &BoundUiPanel,
    doc: &UiPanelDoc,
    root: Entity,
    controller: UiAssetRefDto,
    count: usize,
) -> Result<Cell, String> {
    let slots = (0..count)
        .map(|index| {
            let target = slot_ref(doc, controller, index)?;
            bound.by_go_ref.get(&target).copied().ok_or_else(|| {
                format!(
                    "Native cell {} slot {index} did not spawn",
                    controller.path_id
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Cell { root, slots })
}

fn cloned_binding(bound: &BoundUiPanel, spec: &CloneSpec) -> Result<Cell, String> {
    Ok(Cell {
        root: bound
            .first(&spec.name)
            .ok_or_else(|| format!("Native cloned cell {} did not spawn", spec.name))?,
        slots: spec
            .slots
            .iter()
            .map(|name| {
                bound
                    .first(name)
                    .ok_or_else(|| format!("Native cloned slot {name} did not spawn"))
            })
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn attach_action(commands: &mut Commands, entity: Entity, action: InGameHudAction) {
    commands.entity(entity).insert((
        Pickable::default(),
        Button,
        Hovered::default(),
        action,
        observe(on_ingame_hud_action),
    ));
}

fn radio_binding(
    bound: &BoundUiPanel,
    doc: &UiPanelDoc,
    root_ref: UiAssetRefDto,
    tab: i32,
    primary: bool,
    commands: &mut Commands,
) -> Result<Radio, String> {
    let source = node_by_ref(&doc.root, root_ref).ok_or("Native radio source node is missing")?;
    let child = |name: &str| {
        source
            .children
            .iter()
            .find(|child| child.name == name)
            .and_then(|child| child.go_ref)
            .and_then(|reference| bound.by_go_ref.get(&reference).copied())
            .ok_or_else(|| format!("Native radio {tab} {name} is missing"))
    };
    let on = child("On")?;
    let off = child("Off")?;
    let action = if primary {
        InGameHudAction::SelectCraftPrimary(tab)
    } else {
        InGameHudAction::SelectCraftSecondary(tab)
    };
    // Radio roots have zero-size RectTransforms; the authored On/Off art supplies hit boxes.
    attach_action(commands, on, action);
    attach_action(commands, off, action);
    for name in ["m_red_dot", "m_new"] {
        if let Ok(entity) = child(name) {
            hide_entity(commands, entity);
        }
    }
    Ok(Radio {
        tab,
        primary,
        on,
        off,
    })
}

fn hide_entity(commands: &mut Commands, entity: Entity) {
    commands
        .entity(entity)
        .insert(Visibility::Hidden)
        .entry::<Node>()
        .and_modify(|mut node| node.display = Display::None);
}

pub(super) fn spawn_crafting_panel(
    root: &mut ChildSpawnerCommands,
    assets: &AssetServer,
    font: &FontSource,
    catalog: &CraftCatalog,
    l10n: &LocalizationDb,
    window_size: Vec2,
) {
    let prepared = load_ui_panel_doc("ui_panels/BagAndBuildPanel.json")
        .and_then(|doc| prepare_panel(doc, catalog));
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            warn!("Native production panel: {error}");
            root.spawn((
                CraftOverlayRoot,
                GlobalZIndex(200),
                Node {
                    display: Display::None,
                    position_type: PositionType::Absolute,
                    left: Val::Percent(12.0),
                    top: Val::Percent(20.0),
                    width: Val::Percent(76.0),
                    flex_direction: FlexDirection::Column,
                    ..default()
                },
            ))
            .with_children(|panel| {
                panel.spawn((
                    Text::new(format!("Production panel could not be loaded: {error}")),
                    TextFont {
                        font: font.clone(),
                        font_size: FontSize::Px(18.0),
                        ..default()
                    },
                ));
                spawn_ingame_btn(panel, "Close", InGameHudAction::ToggleCraftMenu, font);
            });
            return;
        }
    };
    root.spawn((
        CraftOverlayRoot,
        Name::new("NativeProductionOverlay"),
        GlobalZIndex(200),
        Pickable::IGNORE,
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        },
    ))
    .with_children(|overlay| {
        let scale = compute_panel_canvas_metrics(&prepared.doc, window_size, true).scale;
        let opts = UiRetainedOptions {
            fit_scale: scale,
            fullscreen: true,
            viewport_size: Some(window_size),
            localize_text: true,
            animate: false,
            interactive_whitelist: Some(HashSet::new()),
            ..default()
        };
        let (stage, bound) = spawn_retained_panel_stage(
            overlay,
            &prepared.doc,
            assets,
            Some(l10n),
            font,
            scale,
            &opts,
        );
        match bind_panel(&prepared, bound, &mut overlay.commands()) {
            Ok(panel) => {
                overlay.commands().entity(stage).insert(panel);
            }
            Err(error) => {
                warn!("Native production bindings: {error}");
                overlay.spawn((
                    Text::new(format!("Production panel binding failed: {error}")),
                    TextFont {
                        font: font.clone(),
                        font_size: FontSize::Px(18.0),
                        ..default()
                    },
                    GlobalZIndex(201),
                ));
            }
        }
    });
}

fn bind_panel(
    prepared: &PreparedPanel,
    bound: BoundUiPanel,
    commands: &mut Commands,
) -> Result<NativeCraftPanel, String> {
    let doc = &prepared.doc;
    let page = (0..67)
        .map(|slot| {
            bound
                .controller_slot(prepared.page_controller, "GameObjects", slot)
                .or_else(|| {
                    // The source templates are intentionally removed when their runtime clones are built.
                    [26, 65].contains(&slot).then_some(Entity::PLACEHOLDER)
                })
                .ok_or_else(|| format!("Production page slot {slot} did not spawn"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for slot in [9, 10, 24, 34, 35, 40, 52, 66] {
        hide_entity(commands, page[slot]);
    }
    hide_entity(commands, page[4]); // Native OnInit hides the cooking category.
    let back = bound
        .game_object_slot(0)
        .ok_or("Native production Back button is missing")?;
    attach_action(commands, back, InGameHudAction::ToggleCraftMenu);
    for (slot, action) in [
        (0, InGameHudAction::AdjustCraftQuantity(1)),
        (18, InGameHudAction::AdjustCraftQuantity(-1)),
        (14, InGameHudAction::MaxCraftQuantity),
        (5, InGameHudAction::SubmitCraftRecipe),
        (3, InGameHudAction::CancelCraftQueue),
        (12, InGameHudAction::LearnCraftDrawing),
    ] {
        attach_action(commands, page[slot], action);
    }
    let mut radios = Vec::new();
    for (tab, slot, _) in PRIMARY_TABS {
        radios.push(radio_binding(
            &bound,
            doc,
            slot_ref(doc, prepared.page_controller, slot)?,
            tab,
            true,
            commands,
        )?);
    }
    for (tab, slot) in SECONDARY_TABS {
        radios.push(radio_binding(
            &bound,
            doc,
            slot_ref(doc, prepared.page_controller, slot)?,
            tab,
            false,
            commands,
        )?);
    }
    let recipes = prepared
        .recipes
        .iter()
        .map(|(drawing_id, spec)| {
            let cell = cloned_binding(&bound, spec)?;
            attach_action(
                commands,
                cell.root,
                InGameHudAction::SelectCraftRecipe(*drawing_id),
            );
            for slot in [4, 6, 8, 9] {
                hide_entity(commands, cell.slots[slot]);
            }
            Ok(RecipeCell {
                drawing_id: *drawing_id,
                cell,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let scroll_materials = prepared
        .materials
        .iter()
        .map(|spec| cloned_binding(&bound, spec))
        .collect::<Result<Vec<_>, _>>()?;
    let list_cells = |path_id, count, cell_slots| -> Result<Vec<Cell>, String> {
        let controller = controller_ref(doc, path_id)?;
        (0..count)
            .map(|index| {
                let reference = slot_ref(doc, controller, index)?;
                let root = bound
                    .by_go_ref
                    .get(&reference)
                    .copied()
                    .ok_or("Native fixed cell did not spawn")?;
                bound_cell(
                    &bound,
                    doc,
                    root,
                    cell_controller(doc, reference)?,
                    cell_slots,
                )
            })
            .collect()
    };
    let fixed_materials = list_cells(REQUIREMENT_CONTROLLER, 4, 5)?;
    let queue = list_cells(QUEUE_CONTROLLER, 4, 5)?;
    for (index, cell) in queue.iter().enumerate() {
        // Native queue Toggle roots are zero sized; Background is the actual authored hit box.
        let source = bound
            .by_go_ref
            .iter()
            .find_map(|(reference, &entity)| (entity == cell.root).then_some(*reference))
            .and_then(|reference| node_by_ref(&doc.root, reference))
            .ok_or("Native queue source is missing")?;
        let hitbox = source
            .children
            .iter()
            .find(|child| child.name == "Background")
            .and_then(|child| child.go_ref)
            .and_then(|reference| bound.by_go_ref.get(&reference).copied())
            .ok_or("Native queue Background is missing")?;
        attach_action(commands, hitbox, InGameHudAction::SelectCraftQueue(index));
    }
    for cell in fixed_materials.iter().chain(&scroll_materials) {
        hide_entity(commands, cell.slots[0]);
    }
    let input_ref = slot_ref(doc, prepared.page_controller, 25)?;
    let input_source =
        node_by_ref(&doc.root, input_ref).ok_or("Native quantity InputField is missing")?;
    let child = |name: &str| {
        input_source
            .children
            .iter()
            .find(|child| child.name == name)
            .and_then(|child| child.go_ref)
            .and_then(|reference| bound.by_go_ref.get(&reference).copied())
    };
    let text = child("Text")
        .and_then(|entity| bound.node_text_child.get(&entity).copied())
        .ok_or("Native quantity Text is missing")?;
    let max_len = input_source
        .widget_props
        .as_ref()
        .and_then(|props| props.char_limit)
        .and_then(|limit| usize::try_from(limit).ok())
        .filter(|&limit| limit > 0)
        .ok_or("Native InputField character limit is missing")?;
    let mut input = commands.entity(page[25]);
    input.insert((
        Pickable::default(),
        UiTextInput {
            tag: INPUT_TAG.to_owned(),
            value: "0".to_owned(),
            placeholder: String::new(),
            max_len,
            count_utf16_units: true,
            text_entity: Some(text),
            ..default()
        },
    ));
    if let Some(placeholder) = child("Placeholder") {
        input.insert(UiTextInputPlaceholder(placeholder));
    }
    attach_text_input_observers(&mut input);
    let recipe_content = bound
        .by_go_ref
        .get(&prepared.recipe_content)
        .copied()
        .ok_or("Native recipe Content did not spawn")?;
    let material_content = bound
        .by_go_ref
        .get(&prepared.material_content)
        .copied()
        .ok_or("Native material Content did not spawn")?;
    Ok(NativeCraftPanel {
        bound,
        page,
        recipes,
        fixed_materials,
        scroll_materials,
        queue,
        radios,
        recipe_content,
        material_content,
        recipe_view: prepared.recipe_view,
        material_view: prepared.material_view,
        recipe_step: prepared.recipe_step,
        material_step: prepared.material_step,
        recipe_art: prepared.recipe_art,
        material_art: prepared.material_art,
        columns: prepared.columns,
        last_tab: -1,
    })
}

pub(super) fn update_crafting_quantity_input(
    mut changes: MessageReader<UiTextInputChanged>,
    client: Res<NetClientState>,
    ui: Res<CraftUiCatalog>,
    mut survival: ResMut<SurvivalSessionState>,
) {
    for change in changes.read().filter(|change| change.tag == INPUT_TAG) {
        let maximum = survival
            .craft_selected_drawing
            .and_then(|id| ui.catalog.drawing(id))
            .map_or(0, |drawing| {
                maximum_craft_quantity(drawing, &client.inventory)
            });
        survival.craft_quantity = change
            .value
            .trim()
            .parse::<i32>()
            .unwrap_or(0)
            .clamp(0, maximum);
    }
}

fn active(
    entity: Entity,
    visible: bool,
    nodes: &mut Query<&mut Node>,
    visibility: &mut Query<&mut Visibility>,
) {
    BoundUiPanel::set_entity_active(entity, visible, visibility, nodes);
}

fn image(
    entity: Entity,
    item_id: i32,
    ui: &CraftUiCatalog,
    assets: &AssetServer,
    images: &mut Query<&mut ImageNode>,
) {
    if let Ok(mut image) = images.get_mut(entity) {
        if let Some(path) = ui.icons.get(&item_id) {
            image.image = assets.load(path.clone());
            image.color = Color::WHITE;
        } else {
            image.color = Color::NONE;
        }
    }
}

fn place_cell(
    entity: Entity,
    position: [f32; 2],
    art: [f32; 2],
    content: [f32; 2],
    nodes: &mut Query<&mut Node>,
) {
    if let Ok(mut node) = nodes.get_mut(entity) {
        node.left = Val::Percent(position[0] / content[0] * 100.0);
        node.top = Val::Percent(position[1] / content[1] * 100.0);
        node.width = Val::Percent(art[0] / content[0] * 100.0);
        node.height = Val::Percent(art[1] / content[1] * 100.0);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn update_crafting_panel(
    client: Res<NetClientState>,
    mut survival: ResMut<SurvivalSessionState>,
    ui: Res<CraftUiCatalog>,
    l10n: Res<LocalizationDb>,
    assets: Res<AssetServer>,
    mut panels: Query<&mut NativeCraftPanel>,
    mut nodes: Query<&mut Node>,
    mut visibility: Query<&mut Visibility>,
    mut texts: Query<&mut Text>,
    mut images: Query<&mut ImageNode>,
    mut inputs: Query<&mut UiTextInput>,
    mut scrolls: Query<&mut bevy::ui::ScrollPosition>,
) {
    if !survival.craft_menu_open {
        return;
    }
    let context = survival.craft_context();
    let catalog = &ui.catalog;
    let recipes = catalog
        .visible_drawings(&context)
        .into_iter()
        .filter(|drawing| category_matches(drawing, &context, survival.craft_secondary_tab))
        .collect::<Vec<_>>();
    let positions = recipes
        .iter()
        .enumerate()
        .map(|(index, drawing)| (drawing.id, index))
        .collect::<HashMap<_, _>>();
    let selected = survival
        .craft_selected_drawing
        .and_then(|id| catalog.drawing(id));
    let maximum = selected.map_or(0, |drawing| {
        maximum_craft_quantity(drawing, &client.inventory)
    });
    survival.craft_quantity = survival.craft_quantity.clamp(0, maximum);
    let queue_selected = survival
        .craft_selected_queue
        .filter(|&index| index < client.craft_queue.len());
    survival.craft_selected_queue = queue_selected;
    for mut panel in &mut panels {
        if panel.last_tab != survival.craft_secondary_tab {
            if let Ok(mut scroll) = scrolls.get_mut(panel.page[39]) {
                *scroll = default();
            }
            panel.last_tab = survival.craft_secondary_tab;
        }
        for (tab, _, group) in PRIMARY_TABS {
            active(
                panel.page[group],
                tab == survival.craft_primary_tab,
                &mut nodes,
                &mut visibility,
            );
        }
        for radio in &panel.radios {
            let selected = radio.tab
                == if radio.primary {
                    survival.craft_primary_tab
                } else {
                    survival.craft_secondary_tab
                };
            active(radio.on, selected, &mut nodes, &mut visibility);
            active(radio.off, !selected, &mut nodes, &mut visibility);
        }
        let recipe_content = [
            panel.recipe_step[0] * panel.columns as f32,
            (recipes.len().div_ceil(panel.columns) as f32 * panel.recipe_step[1])
                .max(panel.recipe_view[1]),
        ];
        if let Ok(mut node) = nodes.get_mut(panel.recipe_content) {
            node.width = Val::Percent(recipe_content[0] / panel.recipe_view[0] * 100.0);
            node.height = Val::Percent(recipe_content[1] / panel.recipe_view[1] * 100.0);
        }
        for recipe in &panel.recipes {
            let cell = &recipe.cell;
            let position = positions.get(&recipe.drawing_id).copied();
            active(cell.root, position.is_some(), &mut nodes, &mut visibility);
            let Some(index) = position else {
                continue;
            };
            let Some(drawing) = catalog.drawing(recipe.drawing_id) else {
                continue;
            };
            place_cell(
                cell.root,
                [
                    (index % panel.columns) as f32 * panel.recipe_step[0],
                    (index / panel.columns) as f32 * panel.recipe_step[1],
                ],
                panel.recipe_art,
                recipe_content,
                &mut nodes,
            );
            let locked = context.is_locked(drawing);
            let can_produce = !locked
                && maximum_craft_quantity(drawing, &client.inventory) > 0
                && drawing.need_workbench == 0
                && drawing.need_tool_box_level == 0;
            active(
                cell.slots[1],
                !locked && !can_produce,
                &mut nodes,
                &mut visibility,
            );
            active(cell.slots[3], locked, &mut nodes, &mut visibility);
            active(
                cell.slots[7],
                Some(drawing.id) == survival.craft_selected_drawing && queue_selected.is_none(),
                &mut nodes,
                &mut visibility,
            );
            let queued = client
                .craft_queue
                .iter()
                .position(|entry| entry.drawing_id == drawing.id);
            active(cell.slots[5], queued.is_some(), &mut nodes, &mut visibility);
            panel.bound.set_entity_text(
                cell.slots[10],
                l10n.tr(if queued == Some(0) {
                    "制造中"
                } else {
                    "排队中"
                }),
                &mut texts,
            );
            image(
                cell.slots[2],
                drawing.target_item_id,
                &ui,
                &assets,
                &mut images,
            );
        }
        active(
            panel.page[32],
            selected.is_some(),
            &mut nodes,
            &mut visibility,
        );
        let locked = selected.is_some_and(|drawing| context.is_locked(drawing));
        active(panel.page[43], locked, &mut nodes, &mut visibility);
        active(
            panel.page[12],
            locked && queue_selected.is_none(),
            &mut nodes,
            &mut visibility,
        );
        active(
            panel.page[5],
            selected.is_some() && !locked && queue_selected.is_none(),
            &mut nodes,
            &mut visibility,
        );
        active(
            panel.page[3],
            queue_selected.is_some(),
            &mut nodes,
            &mut visibility,
        );
        active(
            panel.page[61],
            selected.is_some() && queue_selected.is_none(),
            &mut nodes,
            &mut visibility,
        );
        active(panel.page[27], locked, &mut nodes, &mut visibility);
        if let Some(drawing) = selected {
            let name = catalog
                .item(drawing.target_item_id)
                .map(|item| strip_rich_text_tags(l10n.tr(&item.name)))
                .unwrap_or_else(|| drawing.target_item_id.to_string());
            let description = catalog
                .item(drawing.target_item_id)
                .map(|item| strip_rich_text_tags(l10n.tr(&item.desc)))
                .unwrap_or_default();
            panel
                .bound
                .set_entity_text(panel.page[44], &name, &mut texts);
            panel
                .bound
                .set_entity_text(panel.page[42], &description, &mut texts);
            panel.bound.set_entity_text(
                panel.page[48],
                &format!(
                    "{}s",
                    (f64::from(drawing.need_time) * f64::from(survival.craft_quantity)).ceil()
                ),
                &mut texts,
            );
            let (kind, level) = context.workbench_requirement(drawing);
            let condition = if kind > 0 {
                format!("{} {level}", l10n.tr("工作台"))
            } else if locked {
                l10n.tr("学习图纸后解锁").to_owned()
            } else {
                String::new()
            };
            panel
                .bound
                .set_entity_text(panel.page[49], &condition, &mut texts);
            image(
                panel.page[28],
                drawing.target_item_id,
                &ui,
                &assets,
                &mut images,
            );
        }
        if let Ok(mut input) = inputs.get_mut(panel.page[25]) {
            input.value = survival.craft_quantity.to_string();
        }
        let material_count = selected.map_or(0, |drawing| drawing.material.len());
        active(
            panel.page[31],
            material_count <= 4,
            &mut nodes,
            &mut visibility,
        );
        active(
            panel.page[60],
            material_count > 4,
            &mut nodes,
            &mut visibility,
        );
        let material_content = [
            (material_count as f32 * panel.material_step[0]).max(panel.material_view[0]),
            panel.material_view[1],
        ];
        if let Ok(mut node) = nodes.get_mut(panel.material_content) {
            node.width = Val::Percent(material_content[0] / panel.material_view[0] * 100.0);
        }
        for (cells, scroll) in [
            (&panel.fixed_materials, false),
            (&panel.scroll_materials, true),
        ] {
            for (index, cell) in cells.iter().enumerate() {
                let material = selected.and_then(|drawing| drawing.material.get(index));
                active(cell.root, material.is_some(), &mut nodes, &mut visibility);
                let Some(material) = material else {
                    continue;
                };
                if scroll {
                    place_cell(
                        cell.root,
                        [index as f32 * panel.material_step[0], 0.0],
                        panel.material_art,
                        material_content,
                        &mut nodes,
                    );
                }
                let required = i64::from(material.num) * i64::from(survival.craft_quantity);
                let stock = i64::from(
                    client
                        .inventory
                        .get(&material.item_id)
                        .copied()
                        .unwrap_or(0),
                );
                panel.bound.set_entity_text(
                    cell.slots[2],
                    &format!("{stock}/{required}"),
                    &mut texts,
                );
                active(
                    cell.slots[3],
                    stock >= required,
                    &mut nodes,
                    &mut visibility,
                );
                active(cell.slots[4], stock < required, &mut nodes, &mut visibility);
                image(cell.slots[1], material.item_id, &ui, &assets, &mut images);
            }
        }
        for (index, cell) in panel.queue.iter().enumerate() {
            let entry = client.craft_queue.get(index);
            for slot in [0, 2, 3, 4] {
                active(
                    cell.slots[slot],
                    entry.is_some(),
                    &mut nodes,
                    &mut visibility,
                );
            }
            active(
                cell.slots[1],
                queue_selected == Some(index),
                &mut nodes,
                &mut visibility,
            );
            if let Some(entry) = entry {
                if let Some(drawing) = catalog.drawing(entry.drawing_id) {
                    image(
                        cell.slots[0],
                        drawing.target_item_id,
                        &ui,
                        &assets,
                        &mut images,
                    );
                }
                panel.bound.set_entity_text(
                    cell.slots[2],
                    &entry.remaining_count.to_string(),
                    &mut texts,
                );
                let status = if index == 0 {
                    format!("{:.1}s", f64::from(entry.remaining_ms) / 1000.0)
                } else {
                    l10n.tr("排队中").to_owned()
                };
                panel
                    .bound
                    .set_entity_text(cell.slots[3], &status, &mut texts);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_controller_slots_bind_queue_materials_and_quantity_input() {
        let catalog = CraftCatalog::load_default().unwrap();
        let mut doc = load_ui_panel_doc("ui_panels/BagAndBuildPanel.json").unwrap();
        doc.textures.clear();
        let prepared = prepare_panel(doc, &catalog).unwrap();
        let mut app = App::new();
        app.add_plugins((bevy::app::TaskPoolPlugin::default(), AssetPlugin::default()))
            .add_systems(
                Startup,
                move |mut commands: Commands, assets: Res<AssetServer>| {
                    commands.spawn(Node::default()).with_children(|root| {
                        let bound = crate::ui::panel_loader::spawn_retained_panel_into(
                            root,
                            &prepared.doc,
                            &assets,
                            None,
                            &FontSource::sans_serif(),
                            &UiRetainedOptions {
                                animate: false,
                                interactive_whitelist: Some(HashSet::new()),
                                ..default()
                            },
                        );
                        let panel = bind_panel(&prepared, bound, &mut root.commands()).unwrap();
                        root.commands().spawn(panel);
                    });
                },
            );
        app.update();
        let mut panels = app.world_mut().query::<&NativeCraftPanel>();
        let panel = panels.single(app.world()).unwrap();
        assert_eq!(panel.queue.len(), 4);
        assert_eq!(panel.fixed_materials.len(), 4);
        assert_eq!(panel.recipes.len(), 315);
        let input = app.world().get::<UiTextInput>(panel.page[25]).unwrap();
        assert_eq!(input.max_len, 3);
        assert!(input.count_utf16_units);
        assert!(matches!(
            app.world().get::<InGameHudAction>(panel.page[5]),
            Some(&InGameHudAction::SubmitCraftRecipe)
        ));
        let roots = panel
            .queue
            .iter()
            .map(|cell| cell.root)
            .collect::<HashSet<_>>();
        assert_eq!(
            roots.len(),
            4,
            "Repeated Toggle paths must retain distinct native PPtrs"
        );
    }

    #[test]
    fn source_cells_keep_geometry_and_get_independent_runtime_identities() {
        let doc = load_ui_panel_doc("ui_panels/BagAndBuildPanel.json").unwrap();
        let controller = controller_ref(&doc, PAGE_CONTROLLER).unwrap();
        let template = node_by_ref(&doc.root, slot_ref(&doc, controller, 26).unwrap())
            .unwrap()
            .clone();
        let catalog = CraftCatalog::load_default().unwrap();
        let prepared = prepare_panel(doc, &catalog).unwrap();
        assert_eq!(prepared.recipe_step, [145.0, 145.0]);
        assert_eq!(prepared.material_step, [156.0, 146.0]);
        assert_eq!(prepared.recipes.len(), 315);
        let content = node_by_ref(&prepared.doc.root, prepared.recipe_content).unwrap();
        assert_eq!(content.children[0].canvas_rect, template.canvas_rect);
        assert_eq!(
            content.children[0].children[0].canvas_rect,
            template.children[0].canvas_rect
        );
        let mut names = HashSet::new();
        fn check(node: &UiPanelNodeDto, names: &mut HashSet<String>) {
            assert!(node.go_ref.is_none());
            assert!(names.insert(node.name.clone()));
            for child in &node.children {
                check(child, names);
            }
        }
        for cell in &content.children {
            check(cell, &mut names);
        }
    }

    #[test]
    fn native_basic_tab_includes_flagged_recipes_across_categories() {
        let catalog = CraftCatalog::load_default().unwrap();
        let context = CraftContext::default();
        assert!(category_matches(
            catalog.drawing(57100).unwrap(),
            &context,
            1
        ));
        assert!(!category_matches(
            catalog.drawing(57089).unwrap(),
            &context,
            1
        ));
        assert!(category_matches(
            catalog.drawing(57089).unwrap(),
            &context,
            2
        ));
        assert_eq!(secondary_tabs(13), [7, 8, 9, 10, 27]);
    }
}
