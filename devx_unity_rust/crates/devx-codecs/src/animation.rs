//! Pure-Rust Unity `AnimationClip` (`class_id == 74`) unpacker covering
//! `m_MuscleClip` (`StreamedClip` + `DenseClip` + `ConstantClip`), legacy keyframe curves,
//! and Unity hierarchy path CRC32 hashes.

use crate::{CodecResult};
use devx_unity_io::{UnityValue, extract_i32_field};
use serde::{Deserialize, Serialize};
use unity_asset_binary::object::UnityObject;

/// Transform channel driven by an animation track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackProperty {
    Translation,
    Rotation,
    Scale,
}

impl TrackProperty {
    pub fn label(self) -> &'static str {
        match self {
            Self::Translation => "Translation",
            Self::Rotation => "Rotation",
            Self::Scale => "Scale",
        }
    }
}

/// Decoded keyframed Transform curve track in Unity left-handed space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedAnimationTrack {
    pub path_hash: u32,
    pub path_name: Option<String>,
    pub property: TrackProperty,
    pub times: Vec<f32>,
    /// 3 floats per key for Translation/Scale; 4 `[x, y, z, w]` floats per key for Rotation.
    pub values: Vec<f32>,
}

/// Decoded Unity `AnimationClip` (`class_id == 74`) summary and sampled TRS tracks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedAnimationClip {
    pub name: String,
    pub duration_secs: f32,
    pub sample_rate: f32,
    pub wrap_mode: i32,
    pub tracks: Vec<DecodedAnimationTrack>,
    pub total_curves: usize,
    pub streamed_curves: usize,
    pub dense_curves: usize,
    pub constant_curves: usize,
    pub events: Vec<(f32, String)>,
}

/// Scalar/vector UI property driven by a keyframed uGUI `AnimationClip` track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAnimChannel {
    LocalEulerZDeg,
    LocalScaleX,
    LocalScaleY,
    AnchoredPosX,
    AnchoredPosY,
    SizeDeltaX,
    SizeDeltaY,
    ColorAlpha,
    Active,
}

/// Decoded single-channel UI property track inside a [`DecodedUiAnimationClip`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedUiAnimTrack {
    pub path_hash: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_name: Option<String>,
    pub channel: UiAnimChannel,
    pub times: Vec<f32>,
    pub values: Vec<f32>,
}

/// Decoded UI `AnimationClip` (`RectTransform` / `Graphic` / `Transform` channels) for canvas playback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedUiAnimationClip {
    pub name: String,
    pub duration_secs: f32,
    pub sample_rate: f32,
    pub wrap_mode: i32,
    pub tracks: Vec<DecodedUiAnimTrack>,
}

/// Standard IEEE 802.3 CRC32 hash used by Unity `Animator` / `AnimationClip` hierarchy paths.
pub fn unity_path_crc32(path: &str) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in path.as_bytes() {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Decodes a Unity `AnimationClip` (`class_id == 74`) into sampled transform tracks.
pub fn decode_animation_clip(obj: &UnityObject) -> CodecResult<DecodedAnimationClip> {
    let props = obj.class.properties();
    let name = props
        .get("m_Name")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .or_else(|| obj.name())
        .unwrap_or_else(|| "AnimationClip".to_owned());

    let sample_rate = props
        .get("m_SampleRate")
        .and_then(UnityValue::as_f64)
        .unwrap_or(30.0)
        .max(1.0) as f32;
    let wrap_mode = props
        .get("m_WrapMode")
        .and_then(extract_i32_field)
        .unwrap_or(0);

    let mut events = Vec::new();
    if let Some(arr) = props.get("m_Events").and_then(UnityValue::as_array) {
        for ev in arr {
            if let Some(m) = ev.as_object() {
                let t = m.get("time").and_then(UnityValue::as_f64).unwrap_or(0.0) as f32;
                let fn_name = m
                    .get("functionName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Event")
                    .to_owned();
                events.push((t, fn_name));
            }
        }
    }

    // 1. Try modern Mecanim `m_MuscleClip` + `m_ClipBindingConstant`.
    if let (Some(mc), Some(bindings_obj)) = (
        props.get("m_MuscleClip").and_then(UnityValue::as_object),
        props.get("m_ClipBindingConstant").and_then(UnityValue::as_object),
    ) {
        let start_time = mc.get("m_StartTime").and_then(UnityValue::as_f64).unwrap_or(0.0) as f32;
        let stop_time = mc.get("m_StopTime").and_then(UnityValue::as_f64).unwrap_or(0.0) as f32;
        let duration = (stop_time - start_time).max(0.0);

        if let Some(clip_data) = mc
            .get("m_Clip")
            .and_then(UnityValue::as_object)
            .and_then(|c| c.get("data"))
            .and_then(UnityValue::as_object)
        {
            let streamed = parse_streamed_clip(clip_data.get("m_StreamedClip"));
            let dense = parse_dense_clip(clip_data.get("m_DenseClip"));
            let constants = parse_constant_clip(clip_data.get("m_ConstantClip"));

            let streamed_curves = streamed.curve_count;
            let dense_curves = dense.curve_count;
            let constant_curves = constants.len();

            let mut scalars: Vec<ScalarCurve> =
                Vec::with_capacity(streamed_curves + dense_curves + constant_curves);
            scalars.extend(streamed.curves);
            scalars.extend(dense.curves);
            for v in constants {
                scalars.push(ScalarCurve::Constant(v));
            }
            let total_curves = scalars.len();

            let tracks = decode_generic_bindings(
                bindings_obj.get("genericBindings"),
                &scalars,
                duration,
                sample_rate,
            );

            if !tracks.is_empty() || total_curves > 0 {
                return Ok(DecodedAnimationClip {
                    name,
                    duration_secs: duration,
                    sample_rate,
                    wrap_mode,
                    tracks,
                    total_curves,
                    streamed_curves,
                    dense_curves,
                    constant_curves,
                    events,
                });
            }
        }
    }

    // 2. Fallback: legacy `m_PositionCurves`, `m_RotationCurves`, `m_ScaleCurves`.
    let (legacy_tracks, legacy_duration) = decode_legacy_curves(props);
    Ok(DecodedAnimationClip {
        name,
        duration_secs: legacy_duration,
        sample_rate,
        wrap_mode,
        total_curves: legacy_tracks.len(),
        tracks: legacy_tracks,
        streamed_curves: 0,
        dense_curves: 0,
        constant_curves: 0,
        events,
    })
}

#[derive(Debug, Clone)]
enum ScalarCurve {
    Streamed(Vec<StreamedKey>),
    Dense {
        begin_time: f32,
        sample_rate: f32,
        samples: Vec<f32>,
    },
    Constant(f32),
}

#[derive(Debug, Clone, Copy)]
struct StreamedKey {
    time: f32,
    coeff: [f32; 4], // a*dt^3 + b*dt^2 + c*dt + d
}

impl ScalarCurve {
    fn is_constant(&self) -> bool {
        match self {
            Self::Constant(_) => true,
            Self::Streamed(keys) => keys.len() <= 1,
            Self::Dense { samples, .. } => samples.len() <= 1,
        }
    }

    fn eval(&self, t: f32) -> f32 {
        match self {
            Self::Constant(v) => *v,
            Self::Dense {
                begin_time,
                sample_rate,
                samples,
            } => {
                if samples.is_empty() {
                    return 0.0;
                }
                if samples.len() == 1 || *sample_rate <= 0.0 {
                    return samples[0];
                }
                let fpos = ((t - *begin_time) * *sample_rate).max(0.0);
                let i0 = (fpos.floor() as usize).min(samples.len() - 1);
                let i1 = (i0 + 1).min(samples.len() - 1);
                let alpha = (fpos - i0 as f32).clamp(0.0, 1.0);
                samples[i0] + (samples[i1] - samples[i0]) * alpha
            }
            Self::Streamed(keys) => {
                if keys.is_empty() {
                    return 0.0;
                }
                // Find the latest key with key.time <= t.
                let idx = keys.partition_point(|k| k.time <= t).saturating_sub(1);
                let key = &keys[idx];
                if !key.time.is_finite() || key.time < -1e10 {
                    // Sentinel pre-start key holds initial constant value in coeff[3].
                    return key.coeff[3];
                }
                let dt = (t - key.time).max(0.0);
                let [a, b, c, d] = key.coeff;
                ((a * dt + b) * dt + c) * dt + d
            }
        }
    }
}

struct ParsedStreamed {
    curve_count: usize,
    curves: Vec<ScalarCurve>,
}

fn parse_streamed_clip(val: Option<&UnityValue>) -> ParsedStreamed {
    let Some(obj) = val.and_then(UnityValue::as_object) else {
        return ParsedStreamed {
            curve_count: 0,
            curves: Vec::new(),
        };
    };
    let curve_count = obj
        .get("curveCount")
        .and_then(extract_i32_field)
        .unwrap_or(0)
        .max(0) as usize;
    let Some(data_arr) = obj.get("data").and_then(UnityValue::as_array) else {
        return ParsedStreamed {
            curve_count,
            curves: vec![ScalarCurve::Constant(0.0); curve_count],
        };
    };

    let words: Vec<u32> = data_arr
        .iter()
        .filter_map(|v| v.as_i64().map(|n| n as u32))
        .collect();

    let mut per_curve_keys: Vec<Vec<StreamedKey>> = vec![Vec::new(); curve_count];
    let mut cursor = 0usize;
    while cursor + 2 <= words.len() {
        let time = f32::from_bits(words[cursor]);
        let key_count = words[cursor + 1] as usize;
        cursor += 2;
        if key_count > 100_000 || cursor + key_count * 5 > words.len() {
            break;
        }
        for _ in 0..key_count {
            let curve_idx = words[cursor] as usize;
            let coeff = [
                f32::from_bits(words[cursor + 1]),
                f32::from_bits(words[cursor + 2]),
                f32::from_bits(words[cursor + 3]),
                f32::from_bits(words[cursor + 4]),
            ];
            cursor += 5;
            if let Some(bucket) = per_curve_keys.get_mut(curve_idx) {
                bucket.push(StreamedKey { time, coeff });
            }
        }
    }

    let curves = per_curve_keys
        .into_iter()
        .map(ScalarCurve::Streamed)
        .collect();
    ParsedStreamed {
        curve_count,
        curves,
    }
}

struct ParsedDense {
    curve_count: usize,
    curves: Vec<ScalarCurve>,
}

fn parse_dense_clip(val: Option<&UnityValue>) -> ParsedDense {
    let Some(obj) = val.and_then(UnityValue::as_object) else {
        return ParsedDense {
            curve_count: 0,
            curves: Vec::new(),
        };
    };
    let begin_time = obj
        .get("m_BeginTime")
        .and_then(UnityValue::as_f64)
        .unwrap_or(0.0) as f32;
    let sample_rate = obj
        .get("m_SampleRate")
        .and_then(UnityValue::as_f64)
        .unwrap_or(30.0) as f32;
    let frame_count = obj
        .get("m_FrameCount")
        .and_then(extract_i32_field)
        .unwrap_or(0)
        .max(0) as usize;
    let curve_count = obj
        .get("m_CurveCount")
        .and_then(extract_i32_field)
        .unwrap_or(0)
        .max(0) as usize;

    let samples_flat: Vec<f32> = obj
        .get("m_SampleArray")
        .and_then(UnityValue::as_array)
        .map(|arr| {
            arr.iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect()
        })
        .unwrap_or_default();

    let mut curves = Vec::with_capacity(curve_count);
    for c in 0..curve_count {
        let mut col = Vec::with_capacity(frame_count);
        for f in 0..frame_count {
            let idx = f * curve_count + c;
            if let Some(&s) = samples_flat.get(idx) {
                col.push(s);
            }
        }
        curves.push(ScalarCurve::Dense {
            begin_time,
            sample_rate,
            samples: col,
        });
    }

    ParsedDense {
        curve_count,
        curves,
    }
}

fn parse_constant_clip(val: Option<&UnityValue>) -> Vec<f32> {
    val.and_then(UnityValue::as_object)
        .and_then(|o| o.get("data"))
        .and_then(UnityValue::as_array)
        .map(|arr| {
            arr.iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect()
        })
        .unwrap_or_default()
}

fn binding_width(type_id: i32, attribute: i32) -> usize {
    if type_id == 4 {
        match attribute {
            1 | 3 | 4 => 3, // localPosition, localScale, localEulerAngles
            2 => 4,         // localRotation quaternion
            _ => 1,
        }
    } else {
        1
    }
}

fn build_sample_times(duration: f32, sample_rate: f32, all_constant: bool) -> Vec<f32> {
    if all_constant || duration <= 1e-5 {
        return vec![0.0];
    }
    let dt = (1.0 / sample_rate.max(24.0)).max(1.0 / 120.0);
    let steps = (duration / dt).ceil() as usize;
    let steps = steps.clamp(1, 2000);
    let mut times = Vec::with_capacity(steps + 1);
    for i in 0..steps {
        times.push((i as f32 / steps as f32) * duration);
    }
    times.push(duration);
    times
}

fn decode_generic_bindings(
    bindings_val: Option<&UnityValue>,
    scalars: &[ScalarCurve],
    duration: f32,
    sample_rate: f32,
) -> Vec<DecodedAnimationTrack> {
    let Some(arr) = bindings_val.and_then(UnityValue::as_array) else {
        return Vec::new();
    };
    let mut tracks = Vec::new();
    let mut curve_cursor = 0usize;

    for b in arr {
        let Some(m) = b.as_object() else {
            continue;
        };
        let type_id = m.get("typeID").and_then(extract_i32_field).unwrap_or(0);
        let attribute = m.get("attribute").and_then(extract_i32_field).unwrap_or(0);
        let is_pptr = m.get("isPPtrCurve").and_then(extract_i32_field).unwrap_or(0) != 0;
        let path_hash = m.get("path").and_then(UnityValue::as_i64).unwrap_or(0) as u32;
        let width = binding_width(type_id, attribute);

        if is_pptr || curve_cursor + width > scalars.len() {
            curve_cursor += width;
            continue;
        }

        let slice = &scalars[curve_cursor..curve_cursor + width];
        curve_cursor += width;

        if type_id != 4 || !matches!(attribute, 1..=4) {
            continue;
        }

        let all_const = slice.iter().all(ScalarCurve::is_constant);
        let times = build_sample_times(duration, sample_rate, all_const);
        let (property, values) = match attribute {
            1 => {
                let mut vals = Vec::with_capacity(times.len() * 3);
                for &t in &times {
                    vals.push(sanitize_f32(slice[0].eval(t), 0.0));
                    vals.push(sanitize_f32(slice[1].eval(t), 0.0));
                    vals.push(sanitize_f32(slice[2].eval(t), 0.0));
                }
                (TrackProperty::Translation, vals)
            }
            2 => {
                let mut vals = Vec::with_capacity(times.len() * 4);
                let mut prev = [0.0f32, 0.0, 0.0, 1.0];
                for &t in &times {
                    let mut q = normalize_quat([
                        sanitize_f32(slice[0].eval(t), 0.0),
                        sanitize_f32(slice[1].eval(t), 0.0),
                        sanitize_f32(slice[2].eval(t), 0.0),
                        sanitize_f32(slice[3].eval(t), 1.0),
                    ]);
                    if dot4(prev, q) < 0.0 {
                        q = [-q[0], -q[1], -q[2], -q[3]];
                    }
                    prev = q;
                    vals.extend_from_slice(&q);
                }
                (TrackProperty::Rotation, vals)
            }
            3 => {
                let mut vals = Vec::with_capacity(times.len() * 3);
                for &t in &times {
                    vals.push(sanitize_f32(slice[0].eval(t), 1.0));
                    vals.push(sanitize_f32(slice[1].eval(t), 1.0));
                    vals.push(sanitize_f32(slice[2].eval(t), 1.0));
                }
                (TrackProperty::Scale, vals)
            }
            4 => {
                let mut vals = Vec::with_capacity(times.len() * 4);
                let mut prev = [0.0f32, 0.0, 0.0, 1.0];
                for &t in &times {
                    let ex = sanitize_f32(slice[0].eval(t), 0.0).to_radians();
                    let ey = sanitize_f32(slice[1].eval(t), 0.0).to_radians();
                    let ez = sanitize_f32(slice[2].eval(t), 0.0).to_radians();
                    let mut q = euler_zxy_to_quat(ex, ey, ez);
                    if dot4(prev, q) < 0.0 {
                        q = [-q[0], -q[1], -q[2], -q[3]];
                    }
                    prev = q;
                    vals.extend_from_slice(&q);
                }
                (TrackProperty::Rotation, vals)
            }
            _ => continue,
        };

        tracks.push(DecodedAnimationTrack {
            path_hash,
            path_name: None,
            property,
            times,
            values,
        });
    }

    tracks
}

fn decode_legacy_curves(
    props: &indexmap::IndexMap<String, UnityValue>,
) -> (Vec<DecodedAnimationTrack>, f32) {
    let mut tracks = Vec::new();
    let mut max_time = 0.0f32;

    for (field, property) in [
        ("m_PositionCurves", TrackProperty::Translation),
        ("m_RotationCurves", TrackProperty::Rotation),
        ("m_ScaleCurves", TrackProperty::Scale),
    ] {
        let Some(arr) = props.get(field).and_then(UnityValue::as_array) else {
            continue;
        };
        for item in arr {
            let Some(m) = item.as_object() else {
                continue;
            };
            let path = m.get("path").and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let path_hash = unity_path_crc32(&path);
            let Some(keys) = m
                .get("curve")
                .and_then(UnityValue::as_object)
                .and_then(|c| c.get("m_Curve"))
                .and_then(UnityValue::as_array)
            else {
                continue;
            };

            let mut times = Vec::with_capacity(keys.len());
            let mut values = Vec::new();
            let mut prev_q = [0.0f32, 0.0, 0.0, 1.0];
            for k in keys {
                let Some(km) = k.as_object() else {
                    continue;
                };
                let t = km.get("time").and_then(UnityValue::as_f64).unwrap_or(0.0) as f32;
                max_time = max_time.max(t);
                times.push(t);
                if let Some(val_map) = km.get("value").and_then(UnityValue::as_object) {
                    let x = val_map.get("x").and_then(UnityValue::as_f64).unwrap_or(0.0) as f32;
                    let y = val_map.get("y").and_then(UnityValue::as_f64).unwrap_or(0.0) as f32;
                    let z = val_map.get("z").and_then(UnityValue::as_f64).unwrap_or(0.0) as f32;
                    if property == TrackProperty::Rotation {
                        let w = val_map.get("w").and_then(UnityValue::as_f64).unwrap_or(1.0) as f32;
                        let mut q = normalize_quat([x, y, z, w]);
                        if dot4(prev_q, q) < 0.0 {
                            q = [-q[0], -q[1], -q[2], -q[3]];
                        }
                        prev_q = q;
                        values.extend_from_slice(&q);
                    } else {
                        values.extend_from_slice(&[x, y, z]);
                    }
                }
            }

            if !times.is_empty() {
                tracks.push(DecodedAnimationTrack {
                    path_hash,
                    path_name: (!path.is_empty()).then_some(path),
                    property,
                    times,
                    values,
                });
            }
        }
    }

    (tracks, max_time)
}

fn sanitize_f32(v: f32, fallback: f32) -> f32 {
    if v.is_finite() { v } else { fallback }
}

fn dot4(a: [f32; 4], b: [f32; 4]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]
}

fn normalize_quat(q: [f32; 4]) -> [f32; 4] {
    let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if len > 1e-6 {
        [q[0] / len, q[1] / len, q[2] / len, q[3] / len]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// Unity applies Euler angles in Z-X-Y intrinsic order.
fn euler_zxy_to_quat(rx: f32, ry: f32, rz: f32) -> [f32; 4] {
    let (sx, cx) = (rx * 0.5).sin_cos();
    let (sy, cy) = (ry * 0.5).sin_cos();
    let (sz, cz) = (rz * 0.5).sin_cos();
    normalize_quat([
        cy * sx * cz + sy * cx * sz,
        sy * cx * cz - cy * sx * sz,
        cy * cx * sz - sy * sx * cz,
        cy * cx * cz + sy * sx * sz,
    ])
}

/// Extracts Euler Z angle (in degrees) from a unit quaternion `[x, y, z, w]`.
fn quat_to_z_deg(q: [f32; 4]) -> f32 {
    let siny_cosp = 2.0 * (q[3] * q[2] + q[0] * q[1]);
    let cosy_cosp = 1.0 - 2.0 * (q[1] * q[1] + q[2] * q[2]);
    siny_cosp.atan2(cosy_cosp).to_degrees()
}

/// Maps a Unity `GenericBinding` `(type_id, attribute)` hash to UI animation channels + scalar offsets.
fn ui_channels_for_binding(type_id: i32, attribute: i32) -> &'static [(usize, UiAnimChannel)] {
    // Verified CRC32 property hashes in Unity 2019-2022 GenericBinding constants.
    const CRC_ANCHORED_POS_X: i32 = 1460864421;
    const CRC_ANCHORED_POS_Y: i32 = 538195251;
    const CRC_SIZE_DELTA_X: i32 = 1967290853;
    const CRC_SIZE_DELTA_Y: i32 = 38095219;
    const CRC_LOCAL_POS_X: i32 = -1757729409_i32; // 2537237887u32 as i32
    const CRC_LOCAL_POS_Y: i32 = -532931095_i32; // 3762036201u32 as i32
    const CRC_COLOR_A: i32 = 304273561;
    const CRC_CANVAS_ALPHA: i32 = 1574349066;
    const CRC_IS_ACTIVE: i32 = 2086281974_u32 as i32;

    match (type_id, attribute) {
        (4, 3) => &[(0, UiAnimChannel::LocalScaleX), (1, UiAnimChannel::LocalScaleY)],
        (4, 4) => &[(2, UiAnimChannel::LocalEulerZDeg)],
        (224, CRC_ANCHORED_POS_X) | (224, CRC_LOCAL_POS_X) => &[(0, UiAnimChannel::AnchoredPosX)],
        (224, CRC_ANCHORED_POS_Y) | (224, CRC_LOCAL_POS_Y) => &[(0, UiAnimChannel::AnchoredPosY)],
        (224, CRC_SIZE_DELTA_X) => &[(0, UiAnimChannel::SizeDeltaX)],
        (224, CRC_SIZE_DELTA_Y) => &[(0, UiAnimChannel::SizeDeltaY)],
        (114 | 225, CRC_COLOR_A) | (225 | 114, CRC_CANVAS_ALPHA) => &[(0, UiAnimChannel::ColorAlpha)],
        (1, CRC_IS_ACTIVE) => &[(0, UiAnimChannel::Active)],
        _ => &[],
    }
}

/// Decodes a Unity `AnimationClip` (`class_id == 74`) into sampled uGUI animation tracks.
pub fn decode_ui_animation_clip(obj: &UnityObject) -> CodecResult<DecodedUiAnimationClip> {
    let props = obj.class.properties();
    let name = props
        .get("m_Name")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .or_else(|| obj.name())
        .unwrap_or_else(|| "AnimationClip".to_owned());

    let sample_rate = props
        .get("m_SampleRate")
        .and_then(UnityValue::as_f64)
        .unwrap_or(60.0)
        .max(1.0) as f32;
    let wrap_mode = props
        .get("m_WrapMode")
        .and_then(extract_i32_field)
        .unwrap_or(0);

    let mut tracks = Vec::new();
    let mut duration_secs = 0.0f32;

    if let (Some(mc), Some(bindings_obj)) = (
        props.get("m_MuscleClip").and_then(UnityValue::as_object),
        props.get("m_ClipBindingConstant").and_then(UnityValue::as_object),
    ) {
        let start_time = mc.get("m_StartTime").and_then(UnityValue::as_f64).unwrap_or(0.0) as f32;
        let stop_time = mc.get("m_StopTime").and_then(UnityValue::as_f64).unwrap_or(0.0) as f32;
        duration_secs = (stop_time - start_time).max(0.0);

        if let Some(clip_data) = mc
            .get("m_Clip")
            .and_then(UnityValue::as_object)
            .and_then(|c| c.get("data"))
            .and_then(UnityValue::as_object)
        {
            let streamed = parse_streamed_clip(clip_data.get("m_StreamedClip"));
            let dense = parse_dense_clip(clip_data.get("m_DenseClip"));
            let constants = parse_constant_clip(clip_data.get("m_ConstantClip"));
            let mut scalars: Vec<ScalarCurve> =
                Vec::with_capacity(streamed.curve_count + dense.curve_count + constants.len());
            scalars.extend(streamed.curves);
            scalars.extend(dense.curves);
            for v in constants {
                scalars.push(ScalarCurve::Constant(v));
            }

            if let Some(arr) = bindings_obj.get("genericBindings").and_then(UnityValue::as_array) {
                let mut cursor = 0usize;
                for b in arr {
                    let Some(m) = b.as_object() else {
                        continue;
                    };
                    let type_id = m.get("typeID").and_then(extract_i32_field).unwrap_or(0);
                    let attribute = m.get("attribute").and_then(extract_i32_field).unwrap_or(0);
                    let is_pptr = m.get("isPPtrCurve").and_then(extract_i32_field).unwrap_or(0) != 0;
                    let path_hash = m.get("path").and_then(UnityValue::as_i64).unwrap_or(0) as u32;
                    let width = binding_width(type_id, attribute);
                    if is_pptr || cursor + width > scalars.len() {
                        cursor += width;
                        continue;
                    }
                    let slice = &scalars[cursor..cursor + width];
                    cursor += width;

                    if type_id == 4 && attribute == 2 && width == 4 {
                        let all_const = slice.iter().all(ScalarCurve::is_constant);
                        let times = build_sample_times(duration_secs, sample_rate, all_const);
                        let values = times
                            .iter()
                            .map(|&t| {
                                quat_to_z_deg(normalize_quat([
                                    sanitize_f32(slice[0].eval(t), 0.0),
                                    sanitize_f32(slice[1].eval(t), 0.0),
                                    sanitize_f32(slice[2].eval(t), 0.0),
                                    sanitize_f32(slice[3].eval(t), 1.0),
                                ]))
                            })
                            .collect();
                        tracks.push(DecodedUiAnimTrack {
                            path_hash,
                            path_name: None,
                            channel: UiAnimChannel::LocalEulerZDeg,
                            times,
                            values,
                        });
                        continue;
                    }

                    for &(slot, channel) in ui_channels_for_binding(type_id, attribute) {
                        if let Some(curve) = slice.get(slot) {
                            let all_const = curve.is_constant();
                            let times = build_sample_times(duration_secs, sample_rate, all_const);
                            let default_v = match channel {
                                UiAnimChannel::LocalScaleX | UiAnimChannel::LocalScaleY | UiAnimChannel::ColorAlpha | UiAnimChannel::Active => 1.0,
                                _ => 0.0,
                            };
                            let values = times
                                .iter()
                                .map(|&t| sanitize_f32(curve.eval(t), default_v))
                                .collect();
                            tracks.push(DecodedUiAnimTrack {
                                path_hash,
                                path_name: None,
                                channel,
                                times,
                                values,
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(DecodedUiAnimationClip {
        name,
        duration_secs,
        sample_rate,
        wrap_mode,
        tracks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_constant_dense_and_streamed_curves() {
        let c = ScalarCurve::Constant(2.5);
        assert_eq!(c.eval(0.5), 2.5);

        let d = ScalarCurve::Dense {
            begin_time: 0.0,
            sample_rate: 10.0,
            samples: vec![0.0, 10.0],
        };
        assert!((d.eval(0.05) - 5.0).abs() < 1e-4);

        let s = ScalarCurve::Streamed(vec![
            StreamedKey {
                time: -3e38,
                coeff: [0.0, 0.0, 0.0, 1.0],
            },
            StreamedKey {
                time: 0.0,
                coeff: [0.0, 0.0, 2.0, 1.0],
            },
        ]);
        assert!((s.eval(0.5) - 2.0).abs() < 1e-5);
    }

    #[test]
    fn crc32_matches_known_standard_vector_and_ui_attributes() {
        assert_eq!(unity_path_crc32("123456789"), 0xCBF4_3926);
        assert_eq!(unity_path_crc32("m_AnchoredPosition.x"), 1460864421);
        assert_eq!(unity_path_crc32("m_AnchoredPosition.y"), 538195251);
        assert_eq!(unity_path_crc32("m_Color.a"), 304273561);
        assert_eq!(unity_path_crc32("m_SizeDelta.x"), 1967290853);
        assert_eq!(unity_path_crc32("m_SizeDelta.y"), 38095219);
        assert_eq!(unity_path_crc32("m_Alpha"), 1574349066);
        assert_eq!(unity_path_crc32("m_LocalPosition.x"), 2537237887);
        assert_eq!(unity_path_crc32("m_LocalPosition.y"), 3762036201);
        assert_eq!(unity_path_crc32("m_IsActive"), 2086281974);
    }
}
