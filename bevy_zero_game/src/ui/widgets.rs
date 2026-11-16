//! Pure-Bevy 0.20.1 retained interactive UI widgets for replicated Unity uGUI panels:
//! selectable color state machine, text input field, horizontal slider, toggle/radio state,
//! mouse-wheel scroll container, and filled progress bar.

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::input_focus::{FocusCause, InputFocus};
use bevy::picking::Pickable;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::{Pressed, ScrollPosition, UiGlobalTransform};
use bevy::ui_widgets::{Activate, Button, observe};

/// Registers all Pure-Bevy widget interaction and synchronization systems.
pub struct UiWidgetsRuntimePlugin;

impl Plugin for UiWidgetsRuntimePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<UiTextInputChanged>()
            .add_message::<UiSliderChanged>()
            .add_message::<UiToggleChanged>()
            .add_systems(
                Update,
                (
                    style_selectable_colors,
                    handle_text_input_typing,
                    sync_text_input_visuals,
                    sync_toggle_visuals,
                    sync_filled_bars,
                    update_scroll_areas,
                ),
            );
    }
}

/// Unity `Selectable` `[normal, highlighted, pressed, disabled]` RGBA colors `[r, g, b, a]`.
#[derive(Component, Clone, Copy, Debug)]
pub struct UiSelectableColors {
    pub colors: [[f32; 4]; 4],
    pub base_tint: Color,
}

impl UiSelectableColors {
    /// Constructs selectable tinting from extracted APK `ColorBlock` arrays.
    pub fn from_unity_block(colors: [[f32; 4]; 4], base_tint: Color) -> Self {
        Self { colors, base_tint }
    }
}

/// Pure-Bevy text input box (`InputField` equivalent).
#[derive(Component, Clone, Debug)]
pub struct UiTextInput {
    pub tag: String,
    pub value: String,
    pub placeholder: String,
    pub max_len: usize,
    pub is_password: bool,
    pub focused: bool,
    pub submit_on_enter: bool,
    pub text_entity: Option<Entity>,
    pub normal_border: Color,
    pub focused_border: Color,
}

impl Default for UiTextInput {
    fn default() -> Self {
        Self {
            tag: String::new(),
            value: String::new(),
            placeholder: "Enter text...".to_owned(),
            max_len: 32,
            is_password: false,
            focused: false,
            submit_on_enter: true,
            text_entity: None,
            normal_border: Color::srgba(0.45, 0.56, 0.72, 0.85),
            focused_border: Color::srgba(0.95, 0.72, 0.25, 1.0),
        }
    }
}

/// Message emitted when a [`UiTextInput`] value changes or receives `Enter`.
#[allow(dead_code)]
#[derive(Message, Clone, Debug)]
pub struct UiTextInputChanged {
    pub entity: Entity,
    pub tag: String,
    pub value: String,
    pub submitted: bool,
}

/// Pure-Bevy continuous horizontal slider (`Slider` equivalent).
#[derive(Component, Clone, Debug)]
pub struct UiSlider {
    pub tag: String,
    pub min: f32,
    pub max: f32,
    pub value: f32,
    pub fill_entity: Option<Entity>,
    pub handle_entity: Option<Entity>,
    pub label_entity: Option<Entity>,
}

impl UiSlider {
    /// Returns the slider's normalized progress in `0.0..=1.0`.
    pub fn normalized(&self) -> f32 {
        let span = (self.max - self.min).abs();
        if span <= 1e-5 {
            return 0.0;
        }
        ((self.value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
    }

    /// Sets value from normalized `t` (`0.0..=1.0`) and returns whether it changed noticeably.
    pub fn set_normalized(&mut self, t: f32) -> bool {
        let clamped = t.clamp(0.0, 1.0);
        let next = self.min + clamped * (self.max - self.min);
        let changed = (next - self.value).abs() > 1e-4;
        self.value = next;
        changed
    }
}

/// Message emitted when a user clicks or drags a [`UiSlider`].
#[allow(dead_code)]
#[derive(Message, Clone, Debug)]
pub struct UiSliderChanged {
    pub entity: Entity,
    pub tag: String,
    pub value: f32,
    pub normalized: f32,
}

/// Pure-Bevy binary toggle (`Toggle` / `RadioButton` equivalent) controlling `On`/`Off` sub-nodes.
#[allow(dead_code)]
#[derive(Component, Clone, Debug)]
pub struct UiToggle {
    pub tag: String,
    pub active: bool,
    pub on_entity: Option<Entity>,
    pub off_entity: Option<Entity>,
}

/// Message emitted when a user clicks a [`UiToggle`].
#[allow(dead_code)]
#[derive(Message, Clone, Debug)]
pub struct UiToggleChanged {
    pub entity: Entity,
    pub tag: String,
    pub active: bool,
}

/// Scrollable vertical viewport container (`ScrollRect` equivalent).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct UiScrollArea {
    pub speed_px: f32,
}

/// Normalized fill bar (`Image.Type.Filled` equivalent) driving `Node::width` or `Node::height`.
#[derive(Component, Clone, Copy, Debug)]
pub struct UiFilledBar {
    pub frac: f32,
    pub horizontal: bool,
}

/// Attaches [`Button`], [`Hovered`], click-to-focus [`UiTextInput`] observers, and border tinting.
pub fn attach_text_input_observers(ec: &mut EntityCommands) {
    ec.insert((
        Button,
        Hovered::default(),
        observe(
            |activate: On<Activate>,
             mut focus: ResMut<InputFocus>,
             mut inputs: Query<(Entity, &mut UiTextInput)>| {
                focus.set(activate.entity, FocusCause::Pressed);
                for (e, mut inp) in &mut inputs {
                    inp.focused = e == activate.entity;
                }
            },
        ),
    ));
}

/// Attaches pointer press/drag normalized evaluation observers to a [`UiSlider`] entity.
pub fn attach_slider_observers(ec: &mut EntityCommands) {
    ec.insert((
        Button,
        Hovered::default(),
        observe(
            |press: On<PointerPress>,
             mut sliders: Query<(&mut UiSlider, &ComputedNode, &UiGlobalTransform)>,
             mut out: MessageWriter<UiSliderChanged>,
             mut nodes: Query<&mut Node>,
             mut texts: Query<&mut Text>| {
                apply_slider_pointer(
                    press.entity,
                    press.pointer.position,
                    &mut sliders,
                    &mut out,
                    &mut nodes,
                    &mut texts,
                );
            },
        ),
        observe(
            |drag: On<PointerDrag>,
             mut sliders: Query<(&mut UiSlider, &ComputedNode, &UiGlobalTransform)>,
             mut out: MessageWriter<UiSliderChanged>,
             mut nodes: Query<&mut Node>,
             mut texts: Query<&mut Text>| {
                apply_slider_pointer(
                    drag.entity,
                    drag.pointer.position,
                    &mut sliders,
                    &mut out,
                    &mut nodes,
                    &mut texts,
                );
            },
        ),
    ));
}

/// Attaches click toggle behavior to a [`UiToggle`] entity.
pub fn attach_toggle_observer(ec: &mut EntityCommands) {
    ec.insert((
        Button,
        Hovered::default(),
        observe(
            |activate: On<Activate>,
             mut toggles: Query<&mut UiToggle>,
             mut out: MessageWriter<UiToggleChanged>| {
                if let Ok(mut tog) = toggles.get_mut(activate.entity) {
                    tog.active = !tog.active;
                    out.write(UiToggleChanged {
                        entity: activate.entity,
                        tag: tog.tag.clone(),
                        active: tog.active,
                    });
                }
            },
        ),
    ));
}

fn apply_slider_pointer(
    entity: Entity,
    pointer_logical: Vec2,
    sliders: &mut Query<(&mut UiSlider, &ComputedNode, &UiGlobalTransform)>,
    out: &mut MessageWriter<UiSliderChanged>,
    nodes: &mut Query<&mut Node>,
    texts: &mut Query<&mut Text>,
) {
    let Ok((mut slider, computed, gtf)) = sliders.get_mut(entity) else {
        return;
    };
    let Some(inv) = gtf.try_inverse() else {
        return;
    };
    let inv_scale = computed.inverse_scale_factor.max(1e-4);
    let phys_pos = pointer_logical / inv_scale;
    let local = inv.transform_point2(phys_pos);
    let norm = (local.x / computed.size().x.max(1.0) + 0.5).clamp(0.0, 1.0);
    let changed = slider.set_normalized(norm);
    sync_one_slider_visuals(&slider, nodes, texts);
    if changed {
        out.write(UiSliderChanged {
            entity,
            tag: slider.tag.clone(),
            value: slider.value,
            normalized: slider.normalized(),
        });
    }
}

/// Pushes current `slider` value to its fill bar, thumb handle, and optional percentage text.
pub fn sync_one_slider_visuals(
    slider: &UiSlider,
    nodes: &mut Query<&mut Node>,
    texts: &mut Query<&mut Text>,
) {
    let pct = (slider.normalized() * 100.0).clamp(0.0, 100.0);
    if let Some(fill_e) = slider.fill_entity
        && let Ok(mut n) = nodes.get_mut(fill_e)
    {
        n.width = Val::Percent(pct);
    }
    if let Some(handle_e) = slider.handle_entity
        && let Ok(mut n) = nodes.get_mut(handle_e)
    {
        n.left = Val::Percent(pct);
    }
    if let Some(lbl_e) = slider.label_entity
        && let Ok(mut t) = texts.get_mut(lbl_e)
    {
        let display = if slider.max <= 1.01 {
            format!("{:.0}%", pct)
        } else {
            format!("{:.0}", slider.value)
        };
        if t.0 != display {
            t.0 = display;
        }
    }
}

/// Spawns a complete Pure-Bevy styled slider widget bar inside a parent UI container.
#[allow(clippy::too_many_arguments)]
pub fn spawn_clean_slider(
    parent: &mut ChildSpawnerCommands,
    tag: impl Into<String>,
    min: f32,
    max: f32,
    value: f32,
    width: Val,
    height: Val,
    fill_color: Color,
    track_tex: Option<Handle<Image>>,
    fill_tex: Option<Handle<Image>>,
) -> Entity {
    let pct = if (max - min).abs() > 1e-5 {
        ((value - min) / (max - min)).clamp(0.0, 1.0) * 100.0
    } else {
        0.0
    };

    let mut fill_entity = None;
    let mut handle_entity = None;
    let mut track_ec = parent.spawn((
        Node {
            width,
            height,
            position_type: PositionType::Relative,
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(4.0)),
            overflow: Overflow::clip(),
            justify_content: JustifyContent::FlexStart,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::srgba(0.08, 0.11, 0.17, 0.92)),
        BorderColor::all(Color::srgba(0.38, 0.54, 0.78, 0.85)),
    ));
    if let Some(t_tex) = track_tex {
        track_ec.insert(ImageNode::new(t_tex));
    }
    track_ec.with_children(|track| {
        let mut fe = track.spawn((
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(pct),
                height: Val::Percent(100.0),
                ..default()
            },
            BackgroundColor(fill_color),
        ));
        if let Some(f_tex) = fill_tex {
            fe.insert(ImageNode::new(f_tex).with_color(fill_color));
        }
        fill_entity = Some(fe.id());

        let he = track
            .spawn((
                Pickable::IGNORE,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(pct),
                    top: Val::Px(-1.0),
                    width: Val::Px(12.0),
                    height: Val::Percent(110.0),
                    margin: UiRect::left(Val::Px(-6.0)),
                    border_radius: BorderRadius::all(Val::Px(3.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.95, 0.96, 0.99)),
                BorderColor::all(Color::srgb(0.20, 0.30, 0.45)),
            ))
            .id();
        handle_entity = Some(he);
    });
    track_ec.insert(UiSlider {
        tag: tag.into(),
        min,
        max,
        value: value.clamp(min.min(max), min.max(max)),
        fill_entity,
        handle_entity,
        label_entity: None,
    });
    attach_slider_observers(&mut track_ec);
    track_ec.id()
}

type SelectableColorsItem<'w> = (
    &'w Hovered,
    Has<Pressed>,
    &'w UiSelectableColors,
    Option<&'w mut BackgroundColor>,
    Option<&'w mut ImageNode>,
);

fn style_selectable_colors(mut q: Query<SelectableColorsItem, With<Button>>) {
    for (hovered, pressed, sel, bg_opt, img_opt) in &mut q {
        let idx = if pressed {
            2
        } else if hovered.get() {
            1
        } else {
            0
        };
        let c = sel.colors[idx];
        let base = sel.base_tint.to_srgba();
        let combined = Color::srgba(
            c[0] * base.red,
            c[1] * base.green,
            c[2] * base.blue,
            (c[3] * base.alpha).clamp(0.0, 1.0),
        );
        if let Some(mut img) = img_opt {
            if img.color != combined {
                img.color = combined;
            }
        } else if let Some(mut bg) = bg_opt
            && base.alpha > 0.004
            && bg.0.alpha() > 0.004
            && bg.0 != combined
        {
            bg.0 = combined;
        }
    }
}

fn handle_text_input_typing(
    mut focus: ResMut<InputFocus>,
    mut key_events: MessageReader<KeyboardInput>,
    mut inputs: Query<(Entity, &mut UiTextInput)>,
    mut out: MessageWriter<UiTextInputChanged>,
) {
    let focused_entity = focus.get();
    for (e, mut inp) in &mut inputs {
        inp.focused = focused_entity == Some(e);
    }

    let Some(target) = focused_entity else {
        key_events.clear();
        return;
    };
    let Ok((_, mut inp)) = inputs.get_mut(target) else {
        key_events.clear();
        return;
    };

    for ev in key_events.read() {
        if ev.state != ButtonState::Pressed {
            continue;
        }
        match &ev.logical_key {
            Key::Backspace => {
                if inp.value.pop().is_some() {
                    out.write(UiTextInputChanged {
                        entity: target,
                        tag: inp.tag.clone(),
                        value: inp.value.clone(),
                        submitted: false,
                    });
                }
            }
            Key::Enter => {
                if inp.submit_on_enter {
                    inp.focused = false;
                    focus.clear();
                    out.write(UiTextInputChanged {
                        entity: target,
                        tag: inp.tag.clone(),
                        value: inp.value.clone(),
                        submitted: true,
                    });
                }
            }
            Key::Escape => {
                inp.focused = false;
                focus.clear();
            }
            Key::Space => {
                push_char_if_fits(&mut inp, ' ', target, &mut out);
            }
            _ => {
                if let Some(ref txt) = ev.text {
                    for ch in txt.chars() {
                        if !ch.is_control() {
                            push_char_if_fits(&mut inp, ch, target, &mut out);
                        }
                    }
                }
            }
        }
    }
}

fn push_char_if_fits(
    inp: &mut UiTextInput,
    ch: char,
    entity: Entity,
    out: &mut MessageWriter<UiTextInputChanged>,
) {
    let char_count = inp.value.chars().count();
    if char_count >= inp.max_len.max(1) {
        return;
    }
    inp.value.push(ch);
    out.write(UiTextInputChanged {
        entity,
        tag: inp.tag.clone(),
        value: inp.value.clone(),
        submitted: false,
    });
}

fn sync_text_input_visuals(
    mut inputs: Query<(&UiTextInput, Option<&mut BorderColor>), Changed<UiTextInput>>,
    mut texts: Query<(&mut Text, &mut TextColor)>,
) {
    for (inp, border_opt) in &mut inputs {
        if let Some(mut bc) = border_opt {
            bc.set_all(if inp.focused {
                inp.focused_border
            } else {
                inp.normal_border
            });
        }
        if let Some(text_e) = inp.text_entity
            && let Ok((mut t, mut tc)) = texts.get_mut(text_e)
        {
            let (display, color) = if inp.value.is_empty() && !inp.focused {
                (inp.placeholder.clone(), Color::srgba(0.66, 0.72, 0.82, 0.85))
            } else {
                let core = if inp.is_password {
                    "*".repeat(inp.value.chars().count())
                } else {
                    inp.value.clone()
                };
                let shown = if inp.focused {
                    format!("{core}|")
                } else {
                    core
                };
                (shown, Color::WHITE)
            };
            if t.0 != display {
                t.0 = display;
            }
            tc.0 = color;
        }
    }
}

fn sync_toggle_visuals(
    toggles: Query<&UiToggle, Changed<UiToggle>>,
    mut vis_q: Query<&mut Visibility>,
) {
    for tog in &toggles {
        if let Some(on_e) = tog.on_entity
            && let Ok(mut v) = vis_q.get_mut(on_e)
        {
            *v = if tog.active {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
        }
        if let Some(off_e) = tog.off_entity
            && let Ok(mut v) = vis_q.get_mut(off_e)
        {
            *v = if tog.active {
                Visibility::Hidden
            } else {
                Visibility::Inherited
            };
        }
    }
}

fn sync_filled_bars(mut bars: Query<(&UiFilledBar, &mut Node), Changed<UiFilledBar>>) {
    for (bar, mut node) in &mut bars {
        let pct = (bar.frac.clamp(0.0, 1.0)) * 100.0;
        if bar.horizontal {
            node.width = Val::Percent(pct);
        } else {
            node.height = Val::Percent(pct);
        }
    }
}

fn update_scroll_areas(
    mut wheels: MessageReader<MouseWheel>,
    mut areas: Query<(&UiScrollArea, &Hovered, &ComputedNode, &mut ScrollPosition)>,
) {
    let mut delta_y = 0.0_f32;
    for w in wheels.read() {
        let mult = match w.unit {
            MouseScrollUnit::Line => 36.0,
            MouseScrollUnit::Pixel => 1.0,
        };
        delta_y += w.y * mult;
    }
    if delta_y.abs() <= 1e-4 {
        return;
    }

    for (area, hovered, computed, mut pos) in &mut areas {
        if !hovered.get() {
            continue;
        }
        let inv = computed.inverse_scale_factor.max(1e-4);
        let max_scroll_y = ((computed.content_size().y - computed.size().y) * inv).max(0.0);
        let max_scroll_x = ((computed.content_size().x - computed.size().x) * inv).max(0.0);
        let step = delta_y * area.speed_px.max(1.0);
        if max_scroll_y > 0.5 {
            pos.y = (pos.y - step).clamp(0.0, max_scroll_y);
        } else if max_scroll_x > 0.5 {
            pos.x = (pos.x - step).clamp(0.0, max_scroll_x);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::input::keyboard::KeyCode;

    #[test]
    fn slider_normalization_clamps_to_bounds() {
        let mut s = UiSlider {
            tag: "music".into(),
            min: 0.0,
            max: 1.0,
            value: 0.5,
            fill_entity: None,
            handle_entity: None,
            label_entity: None,
        };
        assert!((s.normalized() - 0.5).abs() < 1e-4);
        assert!(s.set_normalized(1.4));
        assert!((s.value - 1.0).abs() < 1e-4);
        assert!(s.set_normalized(-0.5));
        assert!(s.value.abs() < 1e-4);
    }

    #[test]
    fn text_input_accepts_typing_and_backspace_when_focused() {
        let mut app = App::new();
        app.init_resource::<InputFocus>()
            .add_message::<KeyboardInput>()
            .add_message::<UiTextInputChanged>()
            .add_message::<UiSliderChanged>()
            .add_message::<UiToggleChanged>()
            .add_systems(Update, (handle_text_input_typing, sync_text_input_visuals).chain());

        let text_e = app.world_mut().spawn((Text::default(), TextColor::WHITE)).id();
        let inp_e = app
            .world_mut()
            .spawn(UiTextInput {
                tag: "callsign".into(),
                value: "Hi".into(),
                max_len: 8,
                text_entity: Some(text_e),
                ..default()
            })
            .id();
        app.world_mut()
            .resource_mut::<InputFocus>()
            .set(inp_e, FocusCause::Pressed);

        app.world_mut().write_message(KeyboardInput {
            key_code: KeyCode::KeyA,
            logical_key: Key::Character("!".into()),
            state: ButtonState::Pressed,
            text: Some("!".into()),
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        app.update();

        assert_eq!(app.world().get::<UiTextInput>(inp_e).unwrap().value, "Hi!");
        assert_eq!(app.world().get::<Text>(text_e).unwrap().0, "Hi!|");

        app.world_mut().write_message(KeyboardInput {
            key_code: KeyCode::Backspace,
            logical_key: Key::Backspace,
            state: ButtonState::Pressed,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        app.update();

        assert_eq!(app.world().get::<UiTextInput>(inp_e).unwrap().value, "Hi");
    }
}
