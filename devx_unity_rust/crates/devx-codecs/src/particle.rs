//! Pure-Rust decoder and 3D emitter/particle-cloud preview generator for Unity
//! `ParticleSystem` (`class_id == 198`).

use crate::{CodecError, CodecResult};
use devx_plugin_api::{DecodedMesh, SubMeshSlice};
use devx_unity_io::{UnityObject, UnityValue, extract_i32_field};
use indexmap::IndexMap;

/// Decoded Unity `MinMaxCurve` summary (`scalar` / `minScalar` + curve state).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParticleCurveSummary {
    pub scalar_min: f32,
    pub scalar_max: f32,
    pub min_max_state: i32,
}

impl ParticleCurveSummary {
    /// Human-readable value summary (`5.0` or `1.0 .. 5.0`).
    pub fn display_value(self) -> String {
        if self.min_max_state == 3 && (self.scalar_max - self.scalar_min).abs() > 1e-4 {
            format!("{:.2} .. {:.2}", self.scalar_min, self.scalar_max)
        } else if matches!(self.min_max_state, 1 | 2) {
            format!("Curve ({:.2})", self.scalar_max)
        } else {
            format!("{:.2}", self.scalar_max)
        }
    }

    /// Average representative scalar useful for emitter preview geometry.
    pub fn midpoint(self) -> f32 {
        if self.min_max_state == 3 {
            0.5 * (self.scalar_min + self.scalar_max)
        } else {
            self.scalar_max
        }
    }
}

/// Decoded Unity `ShapeModule` emitter geometry descriptor.
#[derive(Debug, Clone)]
pub struct ParticleShapeInfo {
    pub enabled: bool,
    pub shape_type: i32,
    pub radius: f32,
    pub radius_thickness: f32,
    pub angle_deg: f32,
    pub length: f32,
    pub position: [f32; 3],
    pub rotation_deg: [f32; 3],
    pub scale: [f32; 3],
}

impl ParticleShapeInfo {
    /// Human-readable emitter shape name matching `ParticleSystemShapeType`.
    pub fn shape_label(&self) -> &'static str {
        match self.shape_type {
            0 => "Sphere",
            2 => "Hemisphere",
            4 => "Cone",
            5 => "Box",
            6 => "Mesh",
            10 => "Circle",
            12 => "Edge",
            13 => "MeshRenderer",
            14 => "SkinnedMeshRenderer",
            15 => "BoxShell",
            16 => "BoxEdge",
            17 => "Donut",
            18 => "Rectangle",
            19 => "Sprite",
            20 => "SpriteRenderer",
            _ => "CustomShape",
        }
    }
}

/// Decoded emission burst entry (`EmissionModule.m_Bursts`).
#[derive(Debug, Clone, Copy)]
pub struct ParticleBurstInfo {
    pub time: f32,
    pub min_count: i32,
    pub max_count: i32,
    pub cycle_count: i32,
    pub repeat_interval: f32,
}

/// Summary status of one `ParticleSystem` sub-module.
#[derive(Debug, Clone)]
pub struct ParticleModuleEntry {
    pub name: &'static str,
    pub enabled: bool,
    pub detail: String,
}

/// Decoded metadata and procedural geometry from a Unity `ParticleSystem` (`class_id == 198`).
#[derive(Debug, Clone)]
pub struct DecodedParticleSystem {
    pub name: String,
    pub duration_secs: f32,
    pub looping: bool,
    pub prewarm: bool,
    pub play_on_awake: bool,
    pub simulation_speed: f32,
    pub max_particles: i32,
    pub start_delay: ParticleCurveSummary,
    pub start_lifetime: ParticleCurveSummary,
    pub start_speed: ParticleCurveSummary,
    pub start_size: ParticleCurveSummary,
    pub gravity_modifier: ParticleCurveSummary,
    pub start_color_min: [f32; 4],
    pub start_color_max: [f32; 4],
    pub emission_enabled: bool,
    pub rate_over_time: ParticleCurveSummary,
    pub rate_over_distance: ParticleCurveSummary,
    pub bursts: Vec<ParticleBurstInfo>,
    pub shape: ParticleShapeInfo,
    pub modules: Vec<ParticleModuleEntry>,
}

impl DecodedParticleSystem {
    /// Builds a clean 3D preview pair `(emitter_wireframe_mesh, billboard_particles_mesh)`
    /// visualizing the emission volume and seeded sample particle quads.
    pub fn build_preview_meshes(&self) -> (DecodedMesh, DecodedMesh) {
        let emitter = build_emitter_shape_mesh(&self.shape);
        let particles = build_sample_particle_quads(self);
        (emitter, particles)
    }
}

/// Decodes a Unity `ParticleSystem` (`class_id == 198`).
pub fn decode_particle_system(obj: &UnityObject) -> CodecResult<DecodedParticleSystem> {
    let props = obj.class.properties();
    let initial = props
        .get("InitialModule")
        .and_then(|v| v.as_object())
        .ok_or_else(|| {
            CodecError::UnsupportedParticle("ParticleSystem missing `InitialModule`".into())
        })?;

    let name = props
        .get("m_Name")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .or_else(|| obj.name())
        .unwrap_or_else(|| "ParticleSystem".to_owned());

    let duration_secs = prop_f32(props, "lengthInSec", 5.0);
    let looping = prop_bool(props, "looping", true);
    let prewarm = prop_bool(props, "prewarm", false);
    let play_on_awake = prop_bool(props, "playOnAwake", true);
    let simulation_speed = prop_f32(props, "simulationSpeed", 1.0);
    let start_delay = read_min_max_curve(props.get("startDelay"), 0.0);

    let max_particles = initial
        .get("maxNumParticles")
        .and_then(extract_i32_field)
        .unwrap_or(1000);
    let start_lifetime = read_min_max_curve(initial.get("startLifetime"), 5.0);
    let start_speed = read_min_max_curve(initial.get("startSpeed"), 5.0);
    let start_size = read_min_max_curve(initial.get("startSize"), 1.0);
    let gravity_modifier = read_min_max_curve(initial.get("gravityModifier"), 0.0);
    let (start_color_min, start_color_max) = read_min_max_color(initial.get("startColor"));

    let emission_obj = props.get("EmissionModule").and_then(|v| v.as_object());
    let emission_enabled = emission_obj.is_some_and(|e| prop_bool(e, "enabled", true));
    let rate_over_time = emission_obj
        .map(|e| read_min_max_curve(e.get("rateOverTime"), 10.0))
        .unwrap_or(ParticleCurveSummary {
            scalar_min: 10.0,
            scalar_max: 10.0,
            min_max_state: 0,
        });
    let rate_over_distance = emission_obj
        .map(|e| read_min_max_curve(e.get("rateOverDistance"), 0.0))
        .unwrap_or(ParticleCurveSummary {
            scalar_min: 0.0,
            scalar_max: 0.0,
            min_max_state: 0,
        });

    let bursts: Vec<ParticleBurstInfo> = emission_obj
        .and_then(|e| e.get("m_Bursts"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    let bo = b.as_object()?;
                    let count_curve = bo.get("countCurve");
                    let (min_count, max_count) = if count_curve.is_some() {
                        let c = read_min_max_curve(count_curve, 10.0);
                        (c.scalar_min.round() as i32, c.scalar_max.round() as i32)
                    } else {
                        let mn = bo.get("minCount").and_then(extract_i32_field).unwrap_or(10);
                        let mx = bo.get("maxCount").and_then(extract_i32_field).unwrap_or(mn);
                        (mn, mx)
                    };
                    Some(ParticleBurstInfo {
                        time: prop_f32(bo, "time", 0.0),
                        min_count,
                        max_count,
                        cycle_count: bo.get("cycleCount").and_then(extract_i32_field).unwrap_or(1),
                        repeat_interval: prop_f32(bo, "repeatInterval", 0.01),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let shape_obj = props.get("ShapeModule").and_then(|v| v.as_object());
    let shape = ParticleShapeInfo {
        enabled: shape_obj.is_some_and(|s| prop_bool(s, "enabled", true)),
        shape_type: shape_obj
            .and_then(|s| s.get("type").and_then(extract_i32_field))
            .unwrap_or(4),
        radius: shape_obj
            .and_then(|s| s.get("radius"))
            .and_then(|r| {
                r.as_f64()
                    .map(|f| f as f32)
                    .or_else(|| r.as_object().map(|o| prop_f32(o, "value", 1.0)))
            })
            .unwrap_or(1.0),
        radius_thickness: shape_obj.map_or(1.0, |s| prop_f32(s, "radiusThickness", 1.0)),
        angle_deg: shape_obj.map_or(25.0, |s| prop_f32(s, "angle", 25.0)),
        length: shape_obj.map_or(5.0, |s| prop_f32(s, "length", 5.0)),
        position: shape_obj.map_or([0.0; 3], |s| read_vec3(s.get("m_Position"), [0.0; 3])),
        rotation_deg: shape_obj.map_or([0.0; 3], |s| read_vec3(s.get("m_Rotation"), [0.0; 3])),
        scale: shape_obj.map_or([1.0; 3], |s| read_vec3(s.get("m_Scale"), [1.0; 3])),
    };

    let module_specs: [(&'static str, &'static str); 19] = [
        ("EmissionModule", "Emission"),
        ("ShapeModule", "Shape"),
        ("VelocityModule", "Velocity over Lifetime"),
        ("InheritVelocityModule", "Inherit Velocity"),
        ("LifetimeByEmitterSpeedModule", "Lifetime by Emitter Speed"),
        ("ForceModule", "Force over Lifetime"),
        ("ColorModule", "Color over Lifetime"),
        ("ColorBySpeedModule", "Color by Speed"),
        ("SizeModule", "Size over Lifetime"),
        ("SizeBySpeedModule", "Size by Speed"),
        ("RotationModule", "Rotation over Lifetime"),
        ("RotationBySpeedModule", "Rotation by Speed"),
        ("ExternalForcesModule", "External Forces"),
        ("NoiseModule", "Noise"),
        ("CollisionModule", "Collision"),
        ("TriggerModule", "Triggers"),
        ("SubModule", "Sub Emitters"),
        ("UVModule", "Texture Sheet Animation"),
        ("TrailModule", "Trails"),
    ];

    let mut modules = Vec::with_capacity(module_specs.len());
    for (key, label) in module_specs {
        let mo = props.get(key).and_then(|v| v.as_object());
        let enabled = mo.is_some_and(|o| prop_bool(o, "enabled", false));
        let detail = match key {
            "EmissionModule" => {
                format!(
                    "rate/s: {} | bursts: {}",
                    rate_over_time.display_value(),
                    bursts.len()
                )
            }
            "ShapeModule" => {
                format!(
                    "{} (r={:.2}, ang={:.0} deg)",
                    shape.shape_label(),
                    shape.radius,
                    shape.angle_deg
                )
            }
            "UVModule" => {
                let tx = mo.and_then(|o| o.get("tilesX").and_then(extract_i32_field)).unwrap_or(1);
                let ty = mo.and_then(|o| o.get("tilesY").and_then(extract_i32_field)).unwrap_or(1);
                format!("grid {tx}x{ty}")
            }
            _ => String::new(),
        };
        modules.push(ParticleModuleEntry {
            name: label,
            enabled,
            detail,
        });
    }

    Ok(DecodedParticleSystem {
        name,
        duration_secs,
        looping,
        prewarm,
        play_on_awake,
        simulation_speed,
        max_particles,
        start_delay,
        start_lifetime,
        start_speed,
        start_size,
        gravity_modifier,
        start_color_min,
        start_color_max,
        emission_enabled,
        rate_over_time,
        rate_over_distance,
        bursts,
        shape,
        modules,
    })
}

fn build_emitter_shape_mesh(shape: &ParticleShapeInfo) -> DecodedMesh {
    let mut buf = EmitterMeshBuf::default();

    let r0 = shape.radius.max(0.08);
    let len = shape.length.max(0.4);
    let segs = 20usize;

    match shape.shape_type {
        5 | 15 | 16 | 18 => {
            // Box / Rectangle
            let hx = (shape.scale[0].abs() * 0.5).max(0.1);
            let hy = (shape.scale[1].abs() * 0.5).max(0.1);
            let hz = if shape.shape_type == 18 {
                0.03
            } else {
                (shape.scale[2].abs() * 0.5).max(0.1)
            };
            buf.push_box_frame([hx, hy, hz]);
        }
        0 | 2 | 10 | 17 => {
            // Sphere / Hemisphere / Circle / Donut ring + meridian
            buf.push_ring(r0, 0.0, segs, 0.03);
            if shape.shape_type == 0 || shape.shape_type == 2 {
                let top_r = (r0 * 0.68).max(0.05);
                buf.push_ring(top_r, r0 * 0.65, segs, 0.025);
            }
        }
        _ => {
            // Cone / generic forward plume cone
            let top_r = r0 + len * shape.angle_deg.clamp(1.0, 82.0).to_radians().tan();
            buf.push_ring(r0, 0.0, segs, 0.025);
            buf.push_ring(top_r, len, segs, 0.025);
        }
    }

    for p in &mut buf.positions {
        p[0] = p[0] * shape.scale[0].max(0.1) + shape.position[0];
        p[1] = p[1] * shape.scale[1].max(0.1) + shape.position[1];
        p[2] = p[2] * shape.scale[2].max(0.1) + shape.position[2];
    }

    let (bounds_min, bounds_max) = compute_bounds(&buf.positions);
    let idx_count = buf.indices.len() as u32;
    DecodedMesh {
        name: format!("Emitter ({})", shape.shape_label()),
        positions: buf.positions,
        normals: buf.normals,
        uvs: buf.uvs,
        indices: buf.indices,
        submeshes: vec![SubMeshSlice {
            first_index: 0,
            index_count: idx_count,
            topology: 0,
        }],
        bounds_min,
        bounds_max,
    }
}

#[derive(Default)]
struct EmitterMeshBuf {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl EmitterMeshBuf {
    fn push_ring(&mut self, radius: f32, y: f32, segs: usize, tube_half: f32) {
        let base = self.positions.len() as u32;
        for i in 0..segs {
            let ang = (i as f32 / segs as f32) * std::f32::consts::TAU;
            let (s, c) = ang.sin_cos();
            self.positions
                .push([(radius - tube_half) * c, y, (radius - tube_half) * s]);
            self.positions
                .push([(radius + tube_half) * c, y, (radius + tube_half) * s]);
            self.normals.extend_from_slice(&[[0.0, 1.0, 0.0]; 2]);
            let u = i as f32 / segs as f32;
            self.uvs.push([u, 0.0]);
            self.uvs.push([u, 1.0]);
        }
        for i in 0..segs {
            let n = (i + 1) % segs;
            let a = base + (i * 2) as u32;
            let b = a + 1;
            let c = base + (n * 2) as u32;
            let d = c + 1;
            self.indices.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }

    fn push_box_frame(&mut self, half: [f32; 3]) {
        let base = self.positions.len() as u32;
        let [hx, hy, hz] = half;
        let corners = [
            [-hx, -hy, -hz],
            [hx, -hy, -hz],
            [hx, hy, -hz],
            [-hx, hy, -hz],
            [-hx, -hy, hz],
            [hx, -hy, hz],
            [hx, hy, hz],
            [-hx, hy, hz],
        ];
        for c in corners {
            self.positions.push(c);
            self.normals.push([0.0, 1.0, 0.0]);
            self.uvs.push([0.5, 0.5]);
        }
        let tris = [
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [3, 6, 2],
            [3, 7, 6],
        ];
        for t in tris {
            self.indices
                .extend_from_slice(&[base + t[0], base + t[1], base + t[2]]);
        }
    }
}

fn build_sample_particle_quads(ps: &DecodedParticleSystem) -> DecodedMesh {
    let count = ((ps.rate_over_time.midpoint() * ps.start_lifetime.midpoint())
        .clamp(12.0, 48.0) as usize)
        .min(ps.max_particles.max(8) as usize);
    let q_size = (ps.start_size.midpoint() * 0.25).clamp(0.06, 0.45);
    let r = ps.shape.radius.max(0.1);
    let spread_h = (ps.start_speed.midpoint().abs() * ps.start_lifetime.midpoint().clamp(0.2, 2.5))
        .max(ps.shape.length * 0.6)
        .clamp(0.5, 6.0);
    let angle_rad = ps.shape.angle_deg.clamp(2.0, 75.0).to_radians();

    let mut positions = Vec::with_capacity(count * 4);
    let mut normals = Vec::with_capacity(count * 4);
    let mut uvs = Vec::with_capacity(count * 4);
    let mut indices = Vec::with_capacity(count * 6);

    for i in 0..count {
        // Low-discrepancy golden angle spiral so preview clouds never alias into a line.
        let t = (i as f32 + 0.5) / count as f32;
        let theta = i as f32 * 2.399_963_2;
        let radial = match ps.shape.shape_type {
            5 | 15 => (i % 5) as f32 * 0.18 * ps.shape.scale[0].max(0.2),
            _ => r * (0.25 + 0.75 * t.sqrt()) + t * spread_h * angle_rad.tan() * 0.6,
        };
        let (sx, cx) = theta.sin_cos();
        let center = match ps.shape.shape_type {
            0 => {
                let phi = (1.0 - 2.0 * t).acos();
                let sp = phi.sin();
                [r * sp * cx, r * phi.cos(), r * sp * sx]
            }
            10 | 17 | 18 => [radial * cx, (i % 3) as f32 * 0.06, radial * sx],
            _ => [radial * cx, t * spread_h, radial * sx],
        };
        let s = q_size * (1.1 - 0.55 * t);
        let base = positions.len() as u32;
        let cxp = center[0] + ps.shape.position[0];
        let cyp = center[1] + ps.shape.position[1];
        let czp = center[2] + ps.shape.position[2];

        positions.extend_from_slice(&[
            [cxp - s, cyp - s, czp],
            [cxp + s, cyp - s, czp],
            [cxp + s, cyp + s, czp],
            [cxp - s, cyp + s, czp],
        ]);
        normals.extend_from_slice(&[[0.0, 0.0, 1.0]; 4]);
        uvs.extend_from_slice(&[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    let (bounds_min, bounds_max) = compute_bounds(&positions);
    DecodedMesh {
        name: format!("{} Cloud", ps.name),
        positions,
        normals,
        uvs,
        indices: indices.clone(),
        submeshes: vec![SubMeshSlice {
            first_index: 0,
            index_count: indices.len() as u32,
            topology: 0,
        }],
        bounds_min,
        bounds_max,
    }
}

fn compute_bounds(positions: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    if positions.is_empty() {
        return ([0.0; 3], [1.0; 3]);
    }
    let mut mn = [f32::INFINITY; 3];
    let mut mx = [f32::NEG_INFINITY; 3];
    for p in positions {
        for i in 0..3 {
            mn[i] = mn[i].min(p[i]);
            mx[i] = mx[i].max(p[i]);
        }
    }
    (mn, mx)
}

fn prop_f32(map: &IndexMap<String, UnityValue>, key: &str, default: f32) -> f32 {
    map.get(key)
        .and_then(UnityValue::as_f64)
        .unwrap_or(f64::from(default)) as f32
}

fn prop_bool(map: &IndexMap<String, UnityValue>, key: &str, default: bool) -> bool {
    map.get(key).and_then(UnityValue::as_bool).unwrap_or(default)
}

fn read_vec3(val: Option<&UnityValue>, default: [f32; 3]) -> [f32; 3] {
    let Some(o) = val.and_then(|v| v.as_object()) else {
        return default;
    };
    [
        prop_f32(o, "x", default[0]),
        prop_f32(o, "y", default[1]),
        prop_f32(o, "z", default[2]),
    ]
}

fn read_min_max_curve(val: Option<&UnityValue>, default: f32) -> ParticleCurveSummary {
    let Some(o) = val.and_then(|v| v.as_object()) else {
        return ParticleCurveSummary {
            scalar_min: default,
            scalar_max: default,
            min_max_state: 0,
        };
    };
    let scalar_max = prop_f32(o, "scalar", default);
    let scalar_min = prop_f32(o, "minScalar", scalar_max);
    let min_max_state = o
        .get("minMaxState")
        .and_then(extract_i32_field)
        .unwrap_or(0);
    ParticleCurveSummary {
        scalar_min,
        scalar_max,
        min_max_state,
    }
}

fn read_min_max_color(val: Option<&UnityValue>) -> ([f32; 4], [f32; 4]) {
    let Some(o) = val.and_then(|v| v.as_object()) else {
        return ([1.0; 4], [1.0; 4]);
    };
    let read_c = |k: &str| -> [f32; 4] {
        o.get(k).and_then(|v| v.as_object()).map_or([1.0; 4], |c| {
            [
                prop_f32(c, "r", 1.0),
                prop_f32(c, "g", 1.0),
                prop_f32(c, "b", 1.0),
                prop_f32(c, "a", 1.0),
            ]
        })
    };
    (read_c("minColor"), read_c("maxColor"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use unity_asset_binary::asset::types::ObjectInfo;
    use unity_asset_core::UnityClass;

    #[test]
    fn decodes_particle_system_modules_and_builds_meshes() {
        let mut init = IndexMap::new();
        init.insert("maxNumParticles".into(), UnityValue::Integer(500));
        let mut lt = IndexMap::new();
        lt.insert("scalar".into(), UnityValue::Float(2.5));
        lt.insert("minScalar".into(), UnityValue::Float(1.0));
        lt.insert("minMaxState".into(), UnityValue::Integer(3));
        init.insert("startLifetime".into(), UnityValue::Object(lt));

        let mut shape = IndexMap::new();
        shape.insert("enabled".into(), UnityValue::Bool(true));
        shape.insert("type".into(), UnityValue::Integer(4));
        shape.insert("angle".into(), UnityValue::Float(30.0));
        shape.insert("length".into(), UnityValue::Float(4.0));

        let mut em = IndexMap::new();
        em.insert("enabled".into(), UnityValue::Bool(true));

        let mut class = UnityClass::new(198, "ParticleSystem".into(), "0".into());
        class.set("m_Name".into(), UnityValue::String("SparksFX".into()));
        class.set("looping".into(), UnityValue::Bool(true));
        class.set("lengthInSec".into(), UnityValue::Float(3.0));
        class.set("InitialModule".into(), UnityValue::Object(init));
        class.set("ShapeModule".into(), UnityValue::Object(shape));
        class.set("EmissionModule".into(), UnityValue::Object(em));
        let obj = UnityObject::from_info_and_class(ObjectInfo::new(1, 0, 0, 198, 0), class);

        let ps = decode_particle_system(&obj).expect("decode particle");
        assert_eq!(ps.name, "SparksFX");
        assert_eq!(ps.max_particles, 500);
        assert_eq!(ps.shape.shape_label(), "Cone");
        assert_eq!(ps.start_lifetime.display_value(), "1.00 .. 2.50");
        let (emitter, cloud) = ps.build_preview_meshes();
        assert!(!emitter.positions.is_empty());
        assert!(!cloud.positions.is_empty());
    }
}
