//! Pure-Rust decoder for Unity `AnimatorController` (`class_id == 91`) state machines,
//! layers, parameters, transitions, and linked `AnimationClip`s.

use crate::CodecResult;
use devx_plugin_api::AssetRef;
use devx_unity_io::{
    UnityObject, UnityValue, UnityWorkspaceData, extract_i32_field, extract_i64_field,
};
use std::collections::HashMap;

/// Animator controller parameter definition and initial value.
#[derive(Debug, Clone)]
pub struct AnimatorParamDef {
    pub name: String,
    pub kind_label: &'static str,
    pub default_display: String,
}

/// Condition predicate attached to a state transition.
#[derive(Debug, Clone)]
pub struct AnimatorTransitionCondition {
    pub param_name: String,
    pub mode_label: &'static str,
    pub threshold: f32,
}

/// Outgoing state transition within an Animator state machine layer.
#[derive(Debug, Clone)]
pub struct AnimatorTransitionDef {
    pub destination_state_idx: Option<usize>,
    pub is_exit: bool,
    pub is_any_state: bool,
    pub has_exit_time: bool,
    pub exit_time: f32,
    pub duration: f32,
    pub conditions: Vec<AnimatorTransitionCondition>,
}

/// Child blending mode encoded in a Unity `BlendTreeConstant` node (`m_BlendType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendTreeKind {
    #[default]
    SimpleClip,
    Blend1D,
    Blend2DSimpleDirectional,
    Blend2DFreeformDirectional,
    Blend2DFreeformCartesian,
    Direct,
}

impl BlendTreeKind {
    /// Decodes Unity's `m_BlendType` integer constant.
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            1 => Self::Blend1D,
            2 => Self::Blend2DSimpleDirectional,
            3 => Self::Blend2DFreeformDirectional,
            4 => Self::Blend2DFreeformCartesian,
            5 => Self::Direct,
            _ => Self::SimpleClip,
        }
    }

    /// Short display label describing the blend algorithm.
    pub fn label(self) -> &'static str {
        match self {
            Self::SimpleClip => "Clip",
            Self::Blend1D => "1D Blend",
            Self::Blend2DSimpleDirectional => "2D Simple Dir",
            Self::Blend2DFreeformDirectional => "2D Freeform Dir",
            Self::Blend2DFreeformCartesian => "2D Freeform Cart",
            Self::Direct => "Direct",
        }
    }
}

/// Single node inside a state's parsed `m_BlendTreeConstantArray`.
#[derive(Debug, Clone, Default)]
pub struct BlendTreeNodeDef {
    pub kind: BlendTreeKind,
    pub clip_id: Option<usize>,
    pub param_x: Option<String>,
    pub param_y: Option<String>,
    pub child_indices: Vec<usize>,
    pub thresholds_1d: Vec<f32>,
    pub positions_2d: Vec<[f32; 2]>,
    pub direct_params: Vec<Option<String>>,
    pub normalized_direct: bool,
}

/// Single state inside an `AnimatorController` state machine.
#[derive(Debug, Clone)]
pub struct AnimatorStateDef {
    pub index: usize,
    pub name: String,
    pub tag: String,
    pub speed: f32,
    pub is_loop: bool,
    pub is_default: bool,
    pub leaf_clip_indices: Vec<usize>,
    pub blend_tree: Vec<BlendTreeNodeDef>,
    pub transitions: Vec<AnimatorTransitionDef>,
}

impl AnimatorStateDef {
    /// Evaluates normalized `(clip_link_idx, weight)` pairs across the state's BlendTree for `params`.
    pub fn evaluate_clip_weights(
        &self,
        params: &HashMap<String, SimParamValue>,
    ) -> Vec<(usize, f32)> {
        if self.blend_tree.is_empty() {
            return self
                .leaf_clip_indices
                .first()
                .map_or_else(Vec::new, |&c| vec![(c, 1.0)]);
        }
        let mut accum: HashMap<usize, f32> = HashMap::new();
        eval_blend_node(&self.blend_tree, 0, 1.0, params, 0, &mut accum);
        if accum.is_empty() {
            return self
                .leaf_clip_indices
                .first()
                .map_or_else(Vec::new, |&c| vec![(c, 1.0)]);
        }
        let total: f32 = accum.values().sum();
        let mut out: Vec<(usize, f32)> = if total > 1e-6 {
            accum
                .into_iter()
                .filter(|&(_, w)| w > 1e-4)
                .map(|(c, w)| (c, w / total))
                .collect()
        } else {
            accum.into_iter().collect()
        };
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }
}

/// Decoded layer referencing a state machine.
#[derive(Debug, Clone)]
pub struct AnimatorLayerDef {
    pub name: String,
    pub state_machine_idx: usize,
    pub default_weight: f32,
    pub blending_mode: i32,
}

/// Decoded state machine graph.
#[derive(Debug, Clone)]
pub struct AnimatorStateMachineDef {
    pub default_state: usize,
    pub states: Vec<AnimatorStateDef>,
    pub any_state_transitions: Vec<AnimatorTransitionDef>,
}

/// Linked `AnimationClip` reference inside `m_AnimationClips`.
#[derive(Debug, Clone)]
pub struct AnimatorClipLink {
    pub index: usize,
    pub asset_ref: Option<AssetRef>,
    pub name_hint: String,
}

/// Complete decoded `AnimatorController` state machine summary.
#[derive(Debug, Clone)]
pub struct DecodedAnimatorController {
    pub name: String,
    pub parameters: Vec<AnimatorParamDef>,
    pub layers: Vec<AnimatorLayerDef>,
    pub state_machines: Vec<AnimatorStateMachineDef>,
    pub clip_links: Vec<AnimatorClipLink>,
}

/// Decodes a Unity `AnimatorController` (`class_id == 91`) binary TypeTree payload.
pub fn decode_animator_controller(
    obj: &UnityObject,
    file_index: usize,
    workspace: &UnityWorkspaceData,
) -> CodecResult<DecodedAnimatorController> {
    let props = obj.class.properties();
    let name = obj
        .name()
        .unwrap_or_else(|| "AnimatorController".to_owned());

    let mut tos: HashMap<u32, String> = HashMap::new();
    if let Some(arr) = props.get("m_TOS").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(m) = item.as_object()
                && let Some(h) = m.get("first").and_then(extract_i64_field)
                && let Some(s) = m.get("second").and_then(|v| v.as_str())
            {
                tos.insert(h as u32, s.to_owned());
            }
        }
    }

    // Resolve `m_AnimationClips` PPtr array.
    let mut clip_links = Vec::new();
    if let Some(arr) = props.get("m_AnimationClips").and_then(|v| v.as_array()) {
        for (index, item) in arr.iter().enumerate() {
            let asset_ref = item.as_object().and_then(|m| {
                let pid = m.get("m_PathID").and_then(extract_i64_field)?;
                if pid == 0 {
                    return None;
                }
                let fid = m.get("m_FileID").and_then(extract_i32_field).unwrap_or(0);
                let target_file = workspace.resolve_external_file(file_index, fid)?;
                Some(AssetRef {
                    file_index: target_file,
                    path_id: pid,
                })
            });
            let name_hint = asset_ref
                .and_then(|r| workspace.object_info(r.file_index, r.path_id))
                .and_then(|(l, info)| devx_unity_io::read_object(l, info).ok())
                .and_then(|o| o.name())
                .unwrap_or_else(|| format!("Clip[{index}]"));
            clip_links.push(AnimatorClipLink {
                index,
                asset_ref,
                name_hint,
            });
        }
    }

    let ctrl_props = props
        .get("m_Controller")
        .and_then(|v| v.as_object());

    // Default values table
    let defaults_obj = ctrl_props
        .and_then(|c| c.get("m_DefaultValues"))
        .and_then(unwrap_data);
    let bool_defaults = extract_bool_vec(defaults_obj.and_then(|d| d.get("m_BoolValues")));
    let int_defaults = extract_i32_vec(defaults_obj.and_then(|d| d.get("m_IntValues")));
    let float_defaults = extract_f32_vec(defaults_obj.and_then(|d| d.get("m_FloatValues")));

    let mut parameters = Vec::new();
    if let Some(values) = ctrl_props
        .and_then(|c| c.get("m_Values"))
        .and_then(unwrap_data)
        .and_then(|d| d.get("m_ValueArray"))
        .and_then(|v| v.as_array())
    {
        for (idx, item) in values.iter().enumerate() {
            let Some(d) = unwrap_data(item) else {
                continue;
            };
            let id_hash = d.get("m_ID").and_then(extract_i64_field).unwrap_or(0) as u32;
            let p_name = tos
                .get(&id_hash)
                .cloned()
                .unwrap_or_else(|| format!("param_{idx}_{id_hash:#x}"));
            let ty = d.get("m_Type").and_then(extract_i32_field).unwrap_or(1);
            let val_idx = d.get("m_Index").and_then(extract_i32_field).unwrap_or(0) as usize;
            let (kind_label, default_display) = match ty {
                1 => (
                    "Float",
                    format!("{:.3}", float_defaults.get(val_idx).copied().unwrap_or(0.0)),
                ),
                3 => (
                    "Int",
                    format!("{}", int_defaults.get(val_idx).copied().unwrap_or(0)),
                ),
                4 => (
                    "Bool",
                    format!("{}", bool_defaults.get(val_idx).copied().unwrap_or(false)),
                ),
                9 => ("Trigger", "false".to_owned()),
                _ => ("Unknown", format!("ty={ty}")),
            };
            parameters.push(AnimatorParamDef {
                name: p_name,
                kind_label,
                default_display,
            });
        }
    }

    let mut layers = Vec::new();
    if let Some(layer_arr) = ctrl_props
        .and_then(|c| c.get("m_LayerArray"))
        .and_then(|v| v.as_array())
    {
        for (idx, item) in layer_arr.iter().enumerate() {
            let Some(d) = unwrap_data(item) else {
                continue;
            };
            let binding = d.get("m_Binding").and_then(extract_i64_field).unwrap_or(0) as u32;
            let lname = tos
                .get(&binding)
                .cloned()
                .unwrap_or_else(|| format!("Layer {idx}"));
            layers.push(AnimatorLayerDef {
                name: lname,
                state_machine_idx: d
                    .get("m_StateMachineIndex")
                    .and_then(extract_i32_field)
                    .unwrap_or(0) as usize,
                default_weight: d
                    .get("m_DefaultWeight")
                    .and_then(UnityValue::as_f64)
                    .unwrap_or(0.0) as f32,
                blending_mode: d
                    .get("(int&)m_LayerBlendingMode")
                    .or_else(|| d.get("m_LayerBlendingMode"))
                    .and_then(extract_i32_field)
                    .unwrap_or(0),
            });
        }
    }

    let mut state_machines = Vec::new();
    if let Some(sm_arr) = ctrl_props
        .and_then(|c| c.get("m_StateMachineArray"))
        .and_then(|v| v.as_array())
    {
        for sm_item in sm_arr {
            let Some(sm_d) = unwrap_data(sm_item) else {
                continue;
            };
            let default_state = sm_d
                .get("m_DefaultState")
                .and_then(extract_i32_field)
                .unwrap_or(0)
                .max(0) as usize;

            let any_state_transitions = sm_d
                .get("m_AnyStateTransitionConstantArray")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| parse_transition(unwrap_data(t)?, &tos, true))
                        .collect()
                })
                .unwrap_or_default();

            let mut states = Vec::new();
            if let Some(st_arr) = sm_d
                .get("m_StateConstantArray")
                .and_then(|v| v.as_array())
            {
                for (st_idx, st_item) in st_arr.iter().enumerate() {
                    let Some(sd) = unwrap_data(st_item) else {
                        continue;
                    };
                    let name_id = sd.get("m_NameID").and_then(extract_i64_field).unwrap_or(0) as u32;
                    let path_id = sd.get("m_PathID").and_then(extract_i64_field).unwrap_or(0) as u32;
                    let st_name = tos
                        .get(&name_id)
                        .or_else(|| tos.get(&path_id))
                        .cloned()
                        .unwrap_or_else(|| format!("State_{st_idx}"));
                    let tag_id = sd.get("m_TagID").and_then(extract_i64_field).unwrap_or(0) as u32;
                    let tag = tos.get(&tag_id).cloned().unwrap_or_default();
                    let speed = sd.get("m_Speed").and_then(UnityValue::as_f64).unwrap_or(1.0) as f32;
                    let is_loop = sd
                        .get("m_Loop")
                        .and_then(UnityValue::as_bool)
                        .unwrap_or(false);

                    let (leaf_clip_indices, blend_tree) = parse_state_blend_tree(sd, &tos);

                    let transitions = sd
                        .get("m_TransitionConstantArray")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|t| parse_transition(unwrap_data(t)?, &tos, false))
                                .collect()
                        })
                        .unwrap_or_default();

                    states.push(AnimatorStateDef {
                        index: st_idx,
                        name: st_name,
                        tag,
                        speed,
                        is_loop,
                        is_default: st_idx == default_state,
                        leaf_clip_indices,
                        blend_tree,
                        transitions,
                    });
                }
            }

            state_machines.push(AnimatorStateMachineDef {
                default_state,
                states,
                any_state_transitions,
            });
        }
    }

    Ok(DecodedAnimatorController {
        name,
        parameters,
        layers,
        state_machines,
        clip_links,
    })
}

fn parse_transition(
    d: &indexmap::IndexMap<String, UnityValue>,
    tos: &HashMap<u32, String>,
    is_any_state: bool,
) -> Option<AnimatorTransitionDef> {
    let dest = d.get("m_DestinationState").and_then(extract_i32_field).unwrap_or(-1);
    let is_exit = !(0..30000).contains(&dest);
    let destination_state_idx = if is_exit { None } else { Some(dest as usize) };
    let has_exit_time = d
        .get("m_HasExitTime")
        .and_then(UnityValue::as_bool)
        .unwrap_or(false);
    let exit_time = d
        .get("m_ExitTime")
        .and_then(UnityValue::as_f64)
        .unwrap_or(1.0) as f32;
    let duration = d
        .get("m_TransitionDuration")
        .and_then(UnityValue::as_f64)
        .unwrap_or(0.25) as f32;

    let mut conditions = Vec::new();
    if let Some(cond_arr) = d
        .get("m_ConditionConstantArray")
        .and_then(|v| v.as_array())
    {
        for c_item in cond_arr {
            let Some(cd) = unwrap_data(c_item) else {
                continue;
            };
            let ev_id = cd.get("m_EventID").and_then(extract_i64_field).unwrap_or(0) as u32;
            let param_name = tos
                .get(&ev_id)
                .cloned()
                .unwrap_or_else(|| format!("{ev_id:#x}"));
            let mode = cd.get("m_ConditionMode").and_then(extract_i32_field).unwrap_or(0);
            let mode_label = match mode {
                1 => "If (true)",
                2 => "IfNot (false)",
                3 => ">",
                4 => "<",
                6 => "==",
                7 => "!=",
                _ => "cond",
            };
            let threshold = cd
                .get("m_EventThreshold")
                .and_then(UnityValue::as_f64)
                .unwrap_or(0.0) as f32;
            conditions.push(AnimatorTransitionCondition {
                param_name,
                mode_label,
                threshold,
            });
        }
    }

    Some(AnimatorTransitionDef {
        destination_state_idx,
        is_exit,
        is_any_state,
        has_exit_time,
        exit_time,
        duration,
        conditions,
    })
}

fn unwrap_data(v: &UnityValue) -> Option<&indexmap::IndexMap<String, UnityValue>> {
    let obj = v.as_object()?;
    if let Some(inner) = obj.get("data").and_then(|d| d.as_object()) {
        Some(inner)
    } else {
        Some(obj)
    }
}

fn extract_bool_vec(v: Option<&UnityValue>) -> Vec<bool> {
    v.and_then(|a| a.as_array())
        .map(|arr| arr.iter().filter_map(UnityValue::as_bool).collect())
        .unwrap_or_default()
}

fn extract_i32_vec(v: Option<&UnityValue>) -> Vec<i32> {
    v.and_then(|a| a.as_array())
        .map(|arr| arr.iter().filter_map(extract_i32_field).collect())
        .unwrap_or_default()
}

fn extract_f32_vec(v: Option<&UnityValue>) -> Vec<f32> {
    v.and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(UnityValue::as_f64)
                .map(|f| f as f32)
                .collect()
        })
        .unwrap_or_default()
}

/// Live simulation parameter value driving [`AnimatorSimState`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SimParamValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    Trigger(bool),
}

impl SimParamValue {
    fn as_f32(self) -> f32 {
        match self {
            Self::Float(v) => v,
            Self::Int(v) => v as f32,
            Self::Bool(b) | Self::Trigger(b) => {
                if b {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }

    fn as_bool(self) -> bool {
        match self {
            Self::Float(v) => v > 0.5,
            Self::Int(v) => v != 0,
            Self::Bool(b) | Self::Trigger(b) => b,
        }
    }
}

/// Active cross-fade transition inside [`AnimatorSimState`].
#[derive(Debug, Clone, Copy)]
pub struct AnimatorActiveTransition {
    pub target_state_idx: usize,
    pub target_time_secs: f32,
    pub elapsed_secs: f32,
    pub duration_secs: f32,
}

/// Output snapshot produced by [`AnimatorSimState::step`].
#[derive(Debug, Clone)]
pub struct AnimatorSimStep {
    pub current_state_idx: usize,
    pub state_time_secs: f32,
    pub state_clip_weights: Vec<(usize, f32)>,
    pub transition_target_idx: Option<usize>,
    pub transition_time_secs: f32,
    pub transition_alpha: f32,
    pub target_clip_weights: Vec<(usize, f32)>,
}

/// Interactive state-machine simulator for a decoded [`DecodedAnimatorController`].
#[derive(Debug, Clone)]
pub struct AnimatorSimState {
    pub layer_idx: usize,
    pub current_state_idx: usize,
    pub state_time_secs: f32,
    pub active_transition: Option<AnimatorActiveTransition>,
    pub params: HashMap<String, SimParamValue>,
}

impl AnimatorSimState {
    /// Initializes simulation parameters and enters the default state for `layer_idx`.
    pub fn from_controller(ctrl: &DecodedAnimatorController, layer_idx: usize) -> Self {
        let mut params = HashMap::with_capacity(ctrl.parameters.len());
        for p in &ctrl.parameters {
            let val = match p.kind_label {
                "Float" => SimParamValue::Float(p.default_display.trim().parse().unwrap_or(0.0)),
                "Int" => SimParamValue::Int(p.default_display.trim().parse().unwrap_or(0)),
                "Bool" => SimParamValue::Bool(p.default_display.trim() == "true"),
                "Trigger" => SimParamValue::Trigger(false),
                _ => SimParamValue::Bool(false),
            };
            params.insert(p.name.clone(), val);
        }
        let clamped_layer = layer_idx.min(ctrl.layers.len().saturating_sub(1));
        let default_state_idx = ctrl
            .layers
            .get(clamped_layer)
            .and_then(|l| ctrl.state_machines.get(l.state_machine_idx))
            .map_or(0, |sm| {
                sm.states
                    .iter()
                    .find(|s| s.is_default || s.index == sm.default_state)
                    .map_or(0, |s| s.index)
            });

        Self {
            layer_idx: clamped_layer,
            current_state_idx: default_state_idx,
            state_time_secs: 0.0,
            active_transition: None,
            params,
        }
    }

    /// Fires a named `Trigger` parameter (consumed once a matching transition begins).
    pub fn fire_trigger(&mut self, name: &str) {
        self.params
            .insert(name.to_owned(), SimParamValue::Trigger(true));
    }

    /// Resets state machine clock and transitions back to the layer's default state.
    pub fn reset(&mut self, ctrl: &DecodedAnimatorController) {
        let preserved_params = self.params.clone();
        *self = Self::from_controller(ctrl, self.layer_idx);
        for (k, v) in preserved_params {
            self.params.insert(k, match v {
                SimParamValue::Trigger(_) => SimParamValue::Trigger(false),
                other => other,
            });
        }
    }

    /// Advances state machine clocks by `dt` seconds and evaluates transition conditions.
    pub fn step(
        &mut self,
        ctrl: &DecodedAnimatorController,
        dt: f32,
        mut state_duration_fn: impl FnMut(usize) -> f32,
    ) -> AnimatorSimStep {
        let sm = ctrl
            .layers
            .get(self.layer_idx)
            .and_then(|l| ctrl.state_machines.get(l.state_machine_idx));
        let Some(sm) = sm else {
            return AnimatorSimStep {
                current_state_idx: 0,
                state_time_secs: 0.0,
                state_clip_weights: Vec::new(),
                transition_target_idx: None,
                transition_time_secs: 0.0,
                transition_alpha: 0.0,
                target_clip_weights: Vec::new(),
            };
        };

        let cur_state = sm
            .states
            .iter()
            .find(|s| s.index == self.current_state_idx)
            .or_else(|| sm.states.first());
        let Some(cur_state) = cur_state else {
            return AnimatorSimStep {
                current_state_idx: 0,
                state_time_secs: 0.0,
                state_clip_weights: Vec::new(),
                transition_target_idx: None,
                transition_time_secs: 0.0,
                transition_alpha: 0.0,
                target_clip_weights: Vec::new(),
            };
        };
        self.current_state_idx = cur_state.index;

        let cur_dur = state_duration_fn(cur_state.index).max(1e-3);
        let speed = if cur_state.speed.abs() > 1e-4 {
            cur_state.speed
        } else {
            1.0
        };
        self.state_time_secs += dt.max(0.0) * speed;
        let normalized_t = self.state_time_secs / cur_dur;
        if self.state_time_secs > cur_dur {
            if cur_state.is_loop {
                self.state_time_secs %= cur_dur;
            } else {
                self.state_time_secs = cur_dur;
            }
        }

        if let Some(ref mut tr) = self.active_transition {
            let tgt_dur = state_duration_fn(tr.target_state_idx).max(1e-3);
            let tgt_speed = sm
                .states
                .iter()
                .find(|s| s.index == tr.target_state_idx)
                .map_or(1.0, |s| if s.speed.abs() > 1e-4 { s.speed } else { 1.0 });
            tr.target_time_secs = (tr.target_time_secs + dt.max(0.0) * tgt_speed) % tgt_dur;
            tr.elapsed_secs += dt.max(0.0);
            if tr.elapsed_secs >= tr.duration_secs.max(1e-4) {
                self.current_state_idx = tr.target_state_idx;
                self.state_time_secs = tr.target_time_secs;
                self.active_transition = None;
            }
        } else {
            let mut fired: Option<&AnimatorTransitionDef> = None;
            for tr in &sm.any_state_transitions {
                if let Some(dst) = tr.destination_state_idx
                    && dst != self.current_state_idx
                    && transition_matches(tr, normalized_t, &self.params)
                {
                    fired = Some(tr);
                    break;
                }
            }
            if fired.is_none() {
                for tr in &cur_state.transitions {
                    if transition_matches(tr, normalized_t, &self.params) {
                        fired = Some(tr);
                        break;
                    }
                }
            }
            if let Some(tr) = fired {
                for cond in &tr.conditions {
                    if let Some(SimParamValue::Trigger(_)) = self.params.get(&cond.param_name) {
                        self.params
                            .insert(cond.param_name.clone(), SimParamValue::Trigger(false));
                    }
                }
                let next_idx = if tr.is_exit {
                    sm.default_state
                } else {
                    tr.destination_state_idx.unwrap_or(sm.default_state)
                };
                if tr.duration <= 1e-3 {
                    self.current_state_idx = next_idx;
                    self.state_time_secs = 0.0;
                } else {
                    self.active_transition = Some(AnimatorActiveTransition {
                        target_state_idx: next_idx,
                        target_time_secs: 0.0,
                        elapsed_secs: 0.0,
                        duration_secs: tr.duration,
                    });
                }
            }
        }

        let final_cur = sm
            .states
            .iter()
            .find(|s| s.index == self.current_state_idx);
        let state_clip_weights = final_cur
            .map(|s| s.evaluate_clip_weights(&self.params))
            .unwrap_or_default();

        match self.active_transition {
            Some(tr) => {
                let target_clip_weights = sm
                    .states
                    .iter()
                    .find(|s| s.index == tr.target_state_idx)
                    .map(|s| s.evaluate_clip_weights(&self.params))
                    .unwrap_or_default();
                AnimatorSimStep {
                    current_state_idx: self.current_state_idx,
                    state_time_secs: self.state_time_secs,
                    state_clip_weights,
                    transition_target_idx: Some(tr.target_state_idx),
                    transition_time_secs: tr.target_time_secs,
                    transition_alpha: (tr.elapsed_secs / tr.duration_secs.max(1e-4)).clamp(0.0, 1.0),
                    target_clip_weights,
                }
            }
            None => AnimatorSimStep {
                current_state_idx: self.current_state_idx,
                state_time_secs: self.state_time_secs,
                state_clip_weights,
                transition_target_idx: None,
                transition_time_secs: 0.0,
                transition_alpha: 0.0,
                target_clip_weights: Vec::new(),
            },
        }
    }
}

fn parse_state_blend_tree(
    sd: &indexmap::IndexMap<String, UnityValue>,
    tos: &HashMap<u32, String>,
) -> (Vec<usize>, Vec<BlendTreeNodeDef>) {
    let mut leaf_clip_indices = Vec::new();
    let mut blend_tree = Vec::new();
    let Some(bt_arr) = sd
        .get("m_BlendTreeConstantArray")
        .and_then(|v| v.as_array())
    else {
        return (leaf_clip_indices, blend_tree);
    };

    for bt in bt_arr {
        let Some(bt_d) = unwrap_data(bt) else {
            continue;
        };
        let Some(nodes) = bt_d.get("m_NodeArray").and_then(|v| v.as_array()) else {
            continue;
        };
        for node in nodes {
            let Some(nd) = unwrap_data(node) else {
                continue;
            };
            let clip_id = nd
                .get("m_ClipID")
                .and_then(extract_i32_field)
                .filter(|&ci| ci >= 0)
                .map(|ci| {
                    let u = ci as usize;
                    if !leaf_clip_indices.contains(&u) {
                        leaf_clip_indices.push(u);
                    }
                    u
                });

            if blend_tree.len() < nodes.len() {
                let kind = BlendTreeKind::from_raw(
                    nd.get("m_BlendType").and_then(extract_i32_field).unwrap_or(0),
                );
                let resolve_param = |key: &str| -> Option<String> {
                    let hash = nd.get(key).and_then(extract_i64_field)? as u32;
                    (hash != u32::MAX).then(|| tos.get(&hash).cloned()).flatten()
                };
                let child_indices = nd
                    .get("m_ChildIndices")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(extract_i32_field)
                            .filter(|&i| i >= 0)
                            .map(|i| i as usize)
                            .collect()
                    })
                    .unwrap_or_default();
                let thresholds_1d = nd
                    .get("m_Blend1dData")
                    .and_then(unwrap_data)
                    .map(|d| extract_f32_vec(d.get("m_ChildThresholdArray")))
                    .unwrap_or_default();
                let positions_2d = nd
                    .get("m_Blend2dData")
                    .and_then(unwrap_data)
                    .and_then(|d| d.get("m_ChildPositionArray"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|p| {
                                let po = unwrap_data(p)?;
                                let x = po.get("x").and_then(UnityValue::as_f64).unwrap_or(0.0) as f32;
                                let y = po.get("y").and_then(UnityValue::as_f64).unwrap_or(0.0) as f32;
                                Some([x, y])
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let direct_data = nd.get("m_BlendDirectData").and_then(unwrap_data);
                let direct_params = direct_data
                    .and_then(|d| d.get("m_ChildBlendEventIDArray"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|v| {
                                let h = extract_i64_field(v)? as u32;
                                (h != u32::MAX).then(|| tos.get(&h).cloned()).flatten()
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let normalized_direct = direct_data
                    .and_then(|d| d.get("m_NormalizedBlendValues"))
                    .and_then(UnityValue::as_bool)
                    .unwrap_or(false);

                blend_tree.push(BlendTreeNodeDef {
                    kind,
                    clip_id,
                    param_x: resolve_param("m_BlendEventID"),
                    param_y: resolve_param("m_BlendEventYID"),
                    child_indices,
                    thresholds_1d,
                    positions_2d,
                    direct_params,
                    normalized_direct,
                });
            }
        }
    }
    (leaf_clip_indices, blend_tree)
}

fn eval_blend_node(
    nodes: &[BlendTreeNodeDef],
    node_idx: usize,
    weight: f32,
    params: &HashMap<String, SimParamValue>,
    depth: usize,
    accum: &mut HashMap<usize, f32>,
) {
    if weight <= 1e-5 || depth >= 16 {
        return;
    }
    let Some(node) = nodes.get(node_idx) else {
        return;
    };
    if node.child_indices.is_empty() {
        if let Some(cid) = node.clip_id {
            *accum.entry(cid).or_insert(0.0) += weight;
        }
        return;
    }

    let n = node.child_indices.len();
    let mut local_w = vec![0.0f32; n];
    match node.kind {
        BlendTreeKind::SimpleClip => {
            if let Some(cid) = node.clip_id {
                *accum.entry(cid).or_insert(0.0) += weight;
                return;
            }
            local_w[0] = 1.0;
        }
        BlendTreeKind::Blend1D => {
            let p = node
                .param_x
                .as_deref()
                .and_then(|k| params.get(k))
                .map_or(0.0, |v| v.as_f32());
            eval_1d_weights(p, &node.thresholds_1d, &mut local_w);
        }
        BlendTreeKind::Blend2DSimpleDirectional
        | BlendTreeKind::Blend2DFreeformDirectional
        | BlendTreeKind::Blend2DFreeformCartesian => {
            let px = node
                .param_x
                .as_deref()
                .and_then(|k| params.get(k))
                .map_or(0.0, |v| v.as_f32());
            let py = node
                .param_y
                .as_deref()
                .and_then(|k| params.get(k))
                .map_or(0.0, |v| v.as_f32());
            eval_2d_gradient_band_weights([px, py], &node.positions_2d, &mut local_w);
        }
        BlendTreeKind::Direct => {
            let mut sum = 0.0f32;
            for (i, w) in local_w.iter_mut().enumerate() {
                let raw = node
                    .direct_params
                    .get(i)
                    .and_then(|p| p.as_deref())
                    .and_then(|k| params.get(k))
                    .map_or(0.0, |v| v.as_f32().max(0.0));
                *w = raw;
                sum += raw;
            }
            if node.normalized_direct && sum > 1e-5 {
                for w in &mut local_w {
                    *w /= sum;
                }
            } else if sum <= 1e-5 {
                local_w[0] = 1.0;
            }
        }
    }

    for (i, &child_idx) in node.child_indices.iter().enumerate() {
        let cw = local_w[i];
        if cw > 1e-5 {
            eval_blend_node(nodes, child_idx, weight * cw, params, depth + 1, accum);
        }
    }
}

fn eval_1d_weights(p: f32, thresholds: &[f32], out: &mut [f32]) {
    let n = out.len();
    if n == 0 {
        return;
    }
    if n == 1 || thresholds.len() < n {
        out[0] = 1.0;
        return;
    }
    let mut order: Vec<(usize, f32)> = (0..n).map(|i| (i, thresholds[i])).collect();
    order.sort_by(|a, b| a.1.total_cmp(&b.1));
    if p <= order[0].1 {
        out[order[0].0] = 1.0;
        return;
    }
    if p >= order[n - 1].1 {
        out[order[n - 1].0] = 1.0;
        return;
    }
    for win in order.windows(2) {
        let (ia, ta) = win[0];
        let (ib, tb) = win[1];
        if p >= ta && p <= tb {
            let span = tb - ta;
            if span.abs() < 1e-6 {
                out[ia] = 1.0;
            } else {
                let t = ((p - ta) / span).clamp(0.0, 1.0);
                out[ia] = 1.0 - t;
                out[ib] = t;
            }
            return;
        }
    }
    out[order[0].0] = 1.0;
}

fn eval_2d_gradient_band_weights(p: [f32; 2], positions: &[[f32; 2]], out: &mut [f32]) {
    let n = out.len();
    if n == 0 {
        return;
    }
    if n == 1 || positions.len() < n {
        out[0] = 1.0;
        return;
    }
    for (i, pos) in positions.iter().take(n).enumerate() {
        let dx = p[0] - pos[0];
        let dy = p[1] - pos[1];
        if dx * dx + dy * dy < 1e-8 {
            out[i] = 1.0;
            return;
        }
    }
    let mut sum = 0.0f32;
    for i in 0..n {
        let pi = positions[i];
        let dpx = p[0] - pi[0];
        let dpy = p[1] - pi[1];
        let mut w_i = 1.0f32;
        for (j, &pj) in positions.iter().take(n).enumerate() {
            if i == j {
                continue;
            }
            let ex = pj[0] - pi[0];
            let ey = pj[1] - pi[1];
            let len2 = ex * ex + ey * ey;
            if len2 < 1e-8 {
                continue;
            }
            let h = 1.0 - (dpx * ex + dpy * ey) / len2;
            w_i = w_i.min(h.clamp(0.0, 1.0));
        }
        out[i] = w_i.max(0.0);
        sum += out[i];
    }
    if sum > 1e-6 {
        for w in out.iter_mut() {
            *w /= sum;
        }
    } else {
        let mut best_i = 0;
        let mut best_d2 = f32::INFINITY;
        for (i, &pos) in positions.iter().take(n).enumerate() {
            let d2 = (p[0] - pos[0]).powi(2) + (p[1] - pos[1]).powi(2);
            if d2 < best_d2 {
                best_d2 = d2;
                best_i = i;
            }
        }
        out[best_i] = 1.0;
    }
}

fn transition_matches(
    tr: &AnimatorTransitionDef,
    normalized_time: f32,
    params: &HashMap<String, SimParamValue>,
) -> bool {
    if tr.has_exit_time && normalized_time < tr.exit_time {
        return false;
    }
    if tr.conditions.is_empty() {
        return tr.has_exit_time;
    }
    tr.conditions.iter().all(|c| {
        let Some(&val) = params.get(&c.param_name) else {
            return false;
        };
        match c.mode_label {
            "If (true)" => val.as_bool(),
            "IfNot (false)" => !val.as_bool(),
            ">" => val.as_f32() > c.threshold,
            "<" => val.as_f32() < c.threshold,
            "==" => (val.as_f32() - c.threshold).abs() < 1e-4,
            "!=" => (val.as_f32() - c.threshold).abs() >= 1e-4,
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animator_simulator_evaluates_conditions_and_crossfades() {
        let ctrl = DecodedAnimatorController {
            name: "HeroCtrl".into(),
            parameters: vec![
                AnimatorParamDef {
                    name: "Speed".into(),
                    kind_label: "Float",
                    default_display: "0.000".into(),
                },
                AnimatorParamDef {
                    name: "Attack".into(),
                    kind_label: "Trigger",
                    default_display: "false".into(),
                },
            ],
            layers: vec![AnimatorLayerDef {
                name: "Base".into(),
                state_machine_idx: 0,
                default_weight: 1.0,
                blending_mode: 0,
            }],
            state_machines: vec![AnimatorStateMachineDef {
                default_state: 0,
                states: vec![
                    AnimatorStateDef {
                        index: 0,
                        name: "Idle".into(),
                        tag: String::new(),
                        speed: 1.0,
                        is_loop: true,
                        is_default: true,
                        leaf_clip_indices: vec![0],
                        blend_tree: Vec::new(),
                        transitions: vec![AnimatorTransitionDef {
                            destination_state_idx: Some(1),
                            is_exit: false,
                            is_any_state: false,
                            has_exit_time: false,
                            exit_time: 1.0,
                            duration: 0.2,
                            conditions: vec![AnimatorTransitionCondition {
                                param_name: "Speed".into(),
                                mode_label: ">",
                                threshold: 0.5,
                            }],
                        }],
                    },
                    AnimatorStateDef {
                        index: 1,
                        name: "Run".into(),
                        tag: String::new(),
                        speed: 1.0,
                        is_loop: true,
                        is_default: false,
                        leaf_clip_indices: vec![1],
                        blend_tree: Vec::new(),
                        transitions: vec![AnimatorTransitionDef {
                            destination_state_idx: Some(2),
                            is_exit: false,
                            is_any_state: false,
                            has_exit_time: false,
                            exit_time: 1.0,
                            duration: 0.0,
                            conditions: vec![AnimatorTransitionCondition {
                                param_name: "Attack".into(),
                                mode_label: "If (true)",
                                threshold: 0.0,
                            }],
                        }],
                    },
                    AnimatorStateDef {
                        index: 2,
                        name: "Slash".into(),
                        tag: String::new(),
                        speed: 1.0,
                        is_loop: false,
                        is_default: false,
                        leaf_clip_indices: vec![2],
                        blend_tree: Vec::new(),
                        transitions: vec![AnimatorTransitionDef {
                            destination_state_idx: Some(0),
                            is_exit: false,
                            is_any_state: false,
                            has_exit_time: true,
                            exit_time: 0.9,
                            duration: 0.0,
                            conditions: vec![],
                        }],
                    },
                ],
                any_state_transitions: Vec::new(),
            }],
            clip_links: Vec::new(),
        };

        let mut sim = AnimatorSimState::from_controller(&ctrl, 0);
        let s0 = sim.step(&ctrl, 0.1, |_| 1.0);
        assert_eq!(s0.current_state_idx, 0);
        assert!(s0.transition_target_idx.is_none());

        sim.params.insert("Speed".into(), SimParamValue::Float(1.0));
        let s1 = sim.step(&ctrl, 0.05, |_| 1.0);
        assert_eq!(s1.transition_target_idx, Some(1));

        let s2 = sim.step(&ctrl, 0.25, |_| 1.0);
        assert_eq!(s2.current_state_idx, 1);
        assert!(s2.transition_target_idx.is_none());

        sim.fire_trigger("Attack");
        let s3 = sim.step(&ctrl, 0.05, |_| 1.0);
        assert_eq!(s3.current_state_idx, 2);
        assert_eq!(sim.params.get("Attack"), Some(&SimParamValue::Trigger(false)));

        let s4 = sim.step(&ctrl, 0.95, |_| 1.0);
        assert_eq!(s4.current_state_idx, 0);
    }

    #[test]
    fn blend_tree_evaluates_1d_and_2d_child_weights() {
        let bt_state = AnimatorStateDef {
            index: 0,
            name: "Locomotion1D".into(),
            tag: String::new(),
            speed: 1.0,
            is_loop: true,
            is_default: true,
            leaf_clip_indices: vec![0, 1, 2],
            blend_tree: vec![
                BlendTreeNodeDef {
                    kind: BlendTreeKind::Blend1D,
                    param_x: Some("Speed".into()),
                    child_indices: vec![1, 2, 3],
                    thresholds_1d: vec![0.0, 1.0, 3.0],
                    ..Default::default()
                },
                BlendTreeNodeDef {
                    clip_id: Some(0),
                    ..Default::default()
                },
                BlendTreeNodeDef {
                    clip_id: Some(1),
                    ..Default::default()
                },
                BlendTreeNodeDef {
                    clip_id: Some(2),
                    ..Default::default()
                },
            ],
            transitions: Vec::new(),
        };

        let mut params = HashMap::new();
        params.insert("Speed".into(), SimParamValue::Float(2.0));
        let w = bt_state.evaluate_clip_weights(&params);
        assert_eq!(w.len(), 2);
        let w1 = w.iter().find(|x| x.0 == 1).map_or(0.0, |x| x.1);
        let w2 = w.iter().find(|x| x.0 == 2).map_or(0.0, |x| x.1);
        assert!((w1 - 0.5).abs() < 1e-4 && (w2 - 0.5).abs() < 1e-4);

        let bt2d = AnimatorStateDef {
            index: 1,
            name: "Strafe2D".into(),
            tag: String::new(),
            speed: 1.0,
            is_loop: true,
            is_default: false,
            leaf_clip_indices: vec![0, 1, 2],
            blend_tree: vec![
                BlendTreeNodeDef {
                    kind: BlendTreeKind::Blend2DFreeformDirectional,
                    param_x: Some("VelX".into()),
                    param_y: Some("VelY".into()),
                    child_indices: vec![1, 2, 3],
                    positions_2d: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                    ..Default::default()
                },
                BlendTreeNodeDef { clip_id: Some(0), ..Default::default() },
                BlendTreeNodeDef { clip_id: Some(1), ..Default::default() },
                BlendTreeNodeDef { clip_id: Some(2), ..Default::default() },
            ],
            transitions: Vec::new(),
        };
        params.insert("VelX".into(), SimParamValue::Float(0.5));
        params.insert("VelY".into(), SimParamValue::Float(0.5));
        let w2d = bt2d.evaluate_clip_weights(&params);
        let sum: f32 = w2d.iter().map(|x| x.1).sum();
        assert!((sum - 1.0).abs() < 1e-4);
        assert!(w2d.iter().any(|&(cid, wt)| (cid == 1 || cid == 2) && wt > 0.3));
    }
}
