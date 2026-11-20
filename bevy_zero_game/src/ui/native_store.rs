//! StoreMgr realm: native Goods cells, instance selection, and owned model equipment.

use super::*;
use crate::ui::retained_cells::{CloneSpec, clone_cell, node_by_ref, node_by_ref_mut};

const GOODS_CONTROLLER: i64 = 5_542_449_047_099_618_739;
const STORE_TABS: [(i32, usize); 11] = [
    (5, 0),
    (6, 158),
    (7, 162),
    (10, 166),
    (8, 170),
    (9, 174),
    (11, 178),
    (12, 217),
    (2, 105),
    (13, 251),
    (14, 255),
];

struct StoreCell {
    root: Entity,
    slots: Vec<Entity>,
    instance: Option<i32>,
}

#[derive(Component)]
pub(in crate::ui::main_lobby) struct NativeStorePanel {
    bound: BoundUiPanel,
    cells: Vec<StoreCell>,
    columns: usize,
    cell_size: [f32; 2],
    cell_art: [f32; 2],
}

fn prepare_store_rows(
    doc: &mut UiPanelDoc,
    item_count: usize,
) -> Result<(Vec<CloneSpec>, usize, [f32; 2], [f32; 2]), String> {
    let viewport = root_slot_ref(doc, 5).ok_or("StorePanel Goods viewport slot 5 is missing")?;
    let template_ref =
        root_slot_ref(doc, 144).ok_or("StorePanel Goods template slot 144 is missing")?;
    let template = node_by_ref(&doc.root, template_ref)
        .cloned()
        .ok_or("StorePanel Goods template is missing")?;
    let controller =
        controller_ref(doc, GOODS_CONTROLLER).ok_or("StorePanel Goods controller is missing")?;
    let mut targets: Vec<_> = doc
        .script_bindings
        .iter()
        .filter(|binding| {
            binding.owner_component_ref == Some(controller) && binding.field_name == "GameObjects"
        })
        .collect();
    targets.sort_by_key(|binding| binding.slot_index);
    let targets = targets
        .into_iter()
        .map(|binding| binding.target_go_ref.ok_or("Store cell has a null target"))
        .collect::<Result<Vec<_>, _>>()?;
    let view = node_by_ref(&doc.root, viewport).ok_or("Goods viewport is missing")?;
    let props = view.widget_props.as_ref();
    let columns = props
        .and_then(|props| props.scroll_cols)
        .unwrap_or(2)
        .max(1) as usize;
    let minimum = columns
        * props
            .and_then(|props| props.scroll_rows)
            .unwrap_or(2)
            .max(1) as usize;
    let step = props
        .and_then(|props| props.scroll_cell_size)
        .unwrap_or([230.0, 250.0]);
    let art = [template.canvas_rect[2], template.canvas_rect[3]];
    let mut specs = Vec::new();
    let mut rows = Vec::new();
    for index in 0..item_count.max(minimum) {
        let (row, spec) = clone_cell(&template, &targets, format!("native_store_goods_{index}"))?;
        rows.push(row);
        specs.push(spec);
    }
    let content = node_by_ref_mut(&mut doc.root, viewport)
        .and_then(|view| {
            view.children
                .iter_mut()
                .find(|child| child.name == "content")
        })
        .ok_or("StorePanel Goods content is missing")?;
    content.children = rows;
    Ok((specs, columns, step, art))
}

pub(super) fn spawn_native_store(
    root: &mut ChildSpawnerCommands,
    ui_state: &MainLobbyUiState,
    ctx: &LobbyUiReadCtx,
    win_size: Vec2,
    font: &FontSource,
) {
    let Ok(mut doc) = load_ui_panel_doc("ui_panels/StorePanel.json") else {
        return;
    };
    let owned: Vec<_> = ctx
        .profile
        .lobby_store_items
        .iter()
        .filter(|item| item.count > 0)
        .collect();
    let (specs, columns, cell_size, cell_art) = match prepare_store_rows(&mut doc, owned.len()) {
        Ok(rows) => rows,
        Err(error) => {
            warn!("Native StorePanel binding: {error}");
            return;
        }
    };
    // Native SetDate hides every content controller before the chosen page is initialized.
    for slot in [
        2, 9, 18, 24, 28, 31, 33, 45, 50, 60, 77, 94, 99, 103, 108, 155, 182, 193, 196, 227,
    ] {
        if let Some(reference) = root_slot_ref(&doc, slot) {
            set_doc_active(&mut doc.root, reference, true);
        }
    }
    for slot in [5, 6, 42, 101] {
        if let Some(reference) = root_slot_ref(&doc, slot) {
            set_doc_active(&mut doc.root, reference, true);
        }
    }
    let options = UiRetainedOptions {
        fullscreen: true,
        viewport_size: Some(win_size),
        fit_scale: compute_panel_canvas_metrics(&doc, win_size, true).scale,
        localize_text: true,
        preview_mode: false,
        ..default()
    };
    let bound = spawn_retained_panel_into(
        root,
        &doc,
        &ctx.asset_server,
        Some(&ctx.l10n),
        font,
        &options,
    );
    let mut bindings = NativeLobbyBindings::new(&bound);
    bindings.bind_root_slot(&bound, 7, LobbyAction::SwitchTab(LobbyTab::Overview));
    bindings.bind_root_slot(&bound, 13, LobbyAction::UseStoreItem);
    bindings.bind_root_slot(&bound, 73, LobbyAction::CloseStoreDescription);
    bindings.bind_root_slot(&bound, 8, LobbyAction::SwitchTab(LobbyTab::Establish));
    for (page, slot) in STORE_TABS {
        bindings.bind_root_slot(&bound, slot, LobbyAction::SetNativePanelPage(page));
    }
    let mut cells = Vec::new();
    for (index, spec) in specs.into_iter().enumerate() {
        let Some(root_entity) = bound.first(&spec.name) else {
            continue;
        };
        let slots: Vec<_> = spec
            .slots
            .iter()
            .filter_map(|name| bound.first(name))
            .collect();
        if slots.len() != 24 {
            continue;
        }
        let instance = owned.get(index).map(|item| item.instance_id);
        if let Some(instance) = instance {
            bindings.bind(
                &bound,
                slots[16],
                LobbyAction::SelectStoreInstance(instance),
            );
        }
        cells.push(StoreCell {
            root: root_entity,
            slots,
            instance,
        });
    }
    // Only authoritative stock is rendered. Empty source cells retain the native grid shape.
    for slot in [
        2, 6, 18, 24, 28, 31, 33, 45, 50, 60, 77, 94, 99, 103, 108, 155, 182, 193, 196, 227,
    ] {
        if let Some(entity) = bound.game_object_slot(slot) {
            set_active(&mut root.commands(), entity, false);
        }
    }
    if let Some(entity) = bound.game_object_slot(9) {
        set_active(
            &mut root.commands(),
            entity,
            ui_state.store_selected_instance.is_some(),
        );
    }
    if let Some(root_entity) = bound.by_canvas_path("") {
        root.commands().entity(root_entity).insert((
            bindings,
            NativeStorePanel {
                bound,
                cells,
                columns,
                cell_size,
                cell_art,
            },
        ));
    }
}

pub(in crate::ui::main_lobby) fn sync_native_store(
    panels: Query<&NativeStorePanel>,
    profile: Res<PlayerProfileSave>,
    ui_state: Res<MainLobbyUiState>,
    configs: Res<GameConfigs>,
    l10n: Res<LocalizationDb>,
    assets: Res<AssetServer>,
    mut nodes: Query<&mut Node>,
    mut visibility: Query<&mut Visibility>,
    mut texts: Query<&mut Text>,
    mut images: Query<&mut ImageNode>,
) {
    for panel in &panels {
        let active =
            |slot, show, nodes: &mut Query<&mut Node>, visibility: &mut Query<&mut Visibility>| {
                if let Some(entity) = panel.bound.game_object_slot(slot) {
                    BoundUiPanel::set_entity_active(entity, show, visibility, nodes);
                }
            };
        for (page, slot) in STORE_TABS {
            if let Some(button) = panel.bound.game_object_slot(slot) {
                for (name, show) in [
                    ("On", page == ui_state.native_panel_page),
                    ("Off", page != ui_state.native_panel_page),
                ] {
                    if let Some(entity) = child_entity(&panel.bound, button, name) {
                        BoundUiPanel::set_entity_active(entity, show, &mut visibility, &mut nodes);
                    }
                }
            }
        }
        for (slot, value) in [
            (86, profile.diamond),
            (88, profile.coupons),
            (90, profile.gold),
        ] {
            if let Some(entity) = panel.bound.game_object_slot(slot) {
                panel
                    .bound
                    .set_entity_text(entity, &value.to_string(), &mut texts);
            }
        }
        active(
            5,
            ui_state.native_panel_page == 5,
            &mut nodes,
            &mut visibility,
        );
        active(
            30,
            ui_state.native_panel_page == 5,
            &mut nodes,
            &mut visibility,
        );
        for (index, cell) in panel.cells.iter().enumerate() {
            if let Ok(mut node) = nodes.get_mut(cell.root) {
                node.left = Val::Px((index % panel.columns) as f32 * panel.cell_size[0]);
                node.top = Val::Px((index / panel.columns) as f32 * panel.cell_size[1]);
                node.width = Val::Px(panel.cell_art[0]);
                node.height = Val::Px(panel.cell_art[1]);
            }
            let item = cell.instance.and_then(|instance| {
                profile
                    .lobby_store_items
                    .iter()
                    .find(|item| item.instance_id == instance && item.count > 0)
            });
            let cfg =
                item.and_then(|item| configs.item_cfgs.iter().find(|cfg| cfg.id == item.item_id));
            for slot in 0..cell.slots.len() {
                if [0, 5, 16].contains(&slot) {
                    continue;
                }
                let show = match slot {
                    3 => item.is_some_and(|item| {
                        Some(item.instance_id) == ui_state.store_selected_instance
                    }),
                    7 => cfg.is_some(),
                    9 => item.is_some_and(|item| {
                        Some(item.instance_id) == profile.lobby_equipped_instance_id
                    }),
                    _ => false,
                };
                BoundUiPanel::set_entity_active(
                    cell.slots[slot],
                    show,
                    &mut visibility,
                    &mut nodes,
                );
            }
            BoundUiPanel::set_entity_active(
                cell.slots[0],
                cfg.is_some(),
                &mut visibility,
                &mut nodes,
            );
            panel.bound.set_entity_text(
                cell.slots[5],
                &item.map(|item| item.count.to_string()).unwrap_or_default(),
                &mut texts,
            );
            if let Some(cfg) = cfg
                && let Ok(mut image) = images.get_mut(cell.slots[0])
            {
                image.image = assets.load(configs.starter_icon_asset_path(&cfg.icon));
                image.color = Color::WHITE;
            }
        }
        let selected = ui_state.store_selected_instance.and_then(|instance| {
            profile
                .lobby_store_items
                .iter()
                .find(|item| item.instance_id == instance && item.count > 0)
        });
        let cfg =
            selected.and_then(|item| configs.item_cfgs.iter().find(|cfg| cfg.id == item.item_id));
        active(9, cfg.is_some(), &mut nodes, &mut visibility);
        active(
            13,
            cfg.is_some_and(|cfg| !cfg.model_path.is_empty()),
            &mut nodes,
            &mut visibility,
        );
        active(14, false, &mut nodes, &mut visibility);
        active(15, false, &mut nodes, &mut visibility);
        for (slot, value) in [
            (
                10,
                cfg.map(|cfg| l10n.tr(&cfg.name).to_owned())
                    .unwrap_or_default(),
            ),
            (
                11,
                cfg.map(|cfg| l10n.tr(&cfg.desc).to_owned())
                    .unwrap_or_default(),
            ),
            (12, String::new()),
            (
                16,
                selected
                    .map(|item| item.count.to_string())
                    .unwrap_or_default(),
            ),
            (
                84,
                selected
                    .map(|item| item.count.to_string())
                    .unwrap_or_default(),
            ),
        ] {
            if let Some(entity) = panel.bound.game_object_slot(slot) {
                panel.bound.set_entity_text(entity, &value, &mut texts);
            }
        }
    }
}
