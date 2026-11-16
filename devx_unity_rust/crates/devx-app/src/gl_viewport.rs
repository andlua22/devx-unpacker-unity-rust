//! Hardware-accelerated OpenGL (`eframe::glow` / `egui_glow::CallbackFn`) 3D viewport renderer
//! with exact camera parity to the software fallback, hardware 24-bit depth buffering,
//! wireframe edge overlay, 3D reference grid, and skeleton bone rendering.

use crate::scene_preview::{ViewportMaterialSlot, ViewportMeshPart, ViewportSubMesh};
use crate::state::MeshViewportState;
use devx_codecs::material::MaterialAlphaMode;
use eframe::egui_glow::{self, ShaderVersion};
use eframe::glow::{self, HasContext as _};
use egui::{Color32, Pos2, Rect, Stroke, pos2};
use parking_lot::Mutex;
use std::sync::Arc;

/// Thread-safe handle holding lazily initialized GPU resources inside `MeshViewportState`.
#[derive(Clone, Default)]
pub struct GlViewportHandle {
    inner: Arc<Mutex<Option<GlViewportResources>>>,
}

impl GlViewportHandle {
    /// Destroys allocated GL programs, buffers, textures, and VAOs when exiting `eframe`.
    pub fn destroy(&self, gl: &glow::Context) {
        if let Some(res) = self.inner.lock().take() {
            res.destroy(gl);
        }
    }
}

struct GlSubMeshMaterial {
    index_start: usize,
    index_count: usize,
    albedo_tex: Option<egui::TextureId>,
    normal_tex: Option<egui::TextureId>,
    emissive_tex: Option<egui::TextureId>,
    metallic_gloss_tex: Option<egui::TextureId>,
    base_color: [f32; 4],
    emissive_color: [f32; 3],
    metallic: f32,
    roughness: f32,
    alpha_mode: u8,
    alpha_cutoff: f32,
    double_sided: bool,
}

struct GlMeshDraw {
    /// Interleaved vertices: `[px, py, pz, nx, ny, nz, u, v]` (8 floats = 32 bytes).
    vertices: Vec<f32>,
    tri_indices: Vec<u32>,
    edge_indices: Vec<u32>,
    submeshes: Vec<GlSubMeshMaterial>,
}

struct GlRenderPacket {
    mvp: [f32; 16],
    cam_world_pos: [f32; 3],
    parts: Vec<GlMeshDraw>,
    grid_line_verts: Vec<f32>,
    bone_line_verts: Vec<f32>,
    shaded: bool,
    textured: bool,
    wireframe: bool,
    backface_cull: bool,
    show_bones: bool,
}

struct GlViewportResources {
    mesh_program: glow::Program,
    line_program: glow::Program,
    mesh_vao: Option<glow::VertexArray>,
    mesh_vbo: glow::Buffer,
    mesh_ebo: glow::Buffer,
    line_vao: Option<glow::VertexArray>,
    line_vbo: glow::Buffer,
    white_tex: glow::Texture,
    u_mesh_mvp: Option<glow::UniformLocation>,
    u_mesh_cam_pos: Option<glow::UniformLocation>,
    u_mesh_base_color: Option<glow::UniformLocation>,
    u_mesh_emissive: Option<glow::UniformLocation>,
    u_mesh_metallic: Option<glow::UniformLocation>,
    u_mesh_roughness: Option<glow::UniformLocation>,
    u_mesh_alpha_mode: Option<glow::UniformLocation>,
    u_mesh_alpha_cutoff: Option<glow::UniformLocation>,
    u_mesh_use_tex: Option<glow::UniformLocation>,
    u_mesh_use_normal: Option<glow::UniformLocation>,
    u_mesh_use_emissive: Option<glow::UniformLocation>,
    u_mesh_use_met: Option<glow::UniformLocation>,
    u_mesh_shaded: Option<glow::UniformLocation>,
    u_mesh_wire_tint: Option<glow::UniformLocation>,
    u_mesh_depth_bias: Option<glow::UniformLocation>,
    u_mesh_tex: Option<glow::UniformLocation>,
    u_mesh_normal_tex: Option<glow::UniformLocation>,
    u_mesh_emissive_tex: Option<glow::UniformLocation>,
    u_mesh_met_tex: Option<glow::UniformLocation>,
    u_line_mvp: Option<glow::UniformLocation>,
    u_line_depth_bias: Option<glow::UniformLocation>,
    compiled_ok: bool,
    last_error: Option<String>,
}

impl GlViewportResources {
    fn create(gl: &glow::Context) -> Self {
        match unsafe { Self::create_inner(gl) } {
            Ok(res) => res,
            Err(err) => {
                eprintln!(
                    "devx-app GPU 3D viewport shader setup failed (falling back to CPU): {err}"
                );
                // Allocate dummy invalid handles only if compilation failed; flag prevents GL draws.
                unsafe {
                    Self {
                        mesh_program: gl.create_program().unwrap_or_else(|_| panic!("gl program")),
                        line_program: gl.create_program().unwrap_or_else(|_| panic!("gl program")),
                        mesh_vao: None,
                        mesh_vbo: gl.create_buffer().unwrap_or_else(|_| panic!("gl buffer")),
                        mesh_ebo: gl.create_buffer().unwrap_or_else(|_| panic!("gl buffer")),
                        line_vao: None,
                        line_vbo: gl.create_buffer().unwrap_or_else(|_| panic!("gl buffer")),
                        white_tex: gl.create_texture().unwrap_or_else(|_| panic!("gl tex")),
                        u_mesh_mvp: None,
                        u_mesh_cam_pos: None,
                        u_mesh_base_color: None,
                        u_mesh_emissive: None,
                        u_mesh_metallic: None,
                        u_mesh_roughness: None,
                        u_mesh_alpha_mode: None,
                        u_mesh_alpha_cutoff: None,
                        u_mesh_use_tex: None,
                        u_mesh_use_normal: None,
                        u_mesh_use_emissive: None,
                        u_mesh_use_met: None,
                        u_mesh_shaded: None,
                        u_mesh_wire_tint: None,
                        u_mesh_depth_bias: None,
                        u_mesh_tex: None,
                        u_mesh_normal_tex: None,
                        u_mesh_emissive_tex: None,
                        u_mesh_met_tex: None,
                        u_line_mvp: None,
                        u_line_depth_bias: None,
                        compiled_ok: false,
                        last_error: Some(err),
                    }
                }
            }
        }
    }

    unsafe fn create_inner(gl: &glow::Context) -> Result<Self, String> {
        let ver = ShaderVersion::get(gl);
        let is_new = ver.is_new_shader_interface();
        let header = ver.version_declaration();
        let precision = if ver.is_embedded() {
            "precision mediump float;\n"
        } else {
            ""
        };

        let (v_in, v_out, f_in, f_out, tex_fn) = if is_new {
            ("in", "out", "in", "out vec4 frag_color;\n", "texture")
        } else {
            ("attribute", "varying", "varying", "", "texture2D")
        };
        let frag_target = if is_new { "frag_color" } else { "gl_FragColor" };

        let mesh_vs = format!(
            "{header}{precision}
uniform mat4 u_mvp;
uniform float u_depth_bias;
{v_in} vec3 a_pos;
{v_in} vec3 a_normal;
{v_in} vec2 a_uv;
{v_out} vec3 v_pos;
{v_out} vec3 v_normal;
{v_out} vec2 v_uv;
void main() {{
    vec4 clip = u_mvp * vec4(a_pos, 1.0);
    clip.z += u_depth_bias * clip.w;
    gl_Position = clip;
    v_pos = a_pos;
    v_normal = a_normal;
    v_uv = a_uv;
}}
"
        );
        let normal_perturb_fn = if is_new || !ver.is_embedded() {
            "vec3 perturb_normal(vec3 n, vec3 view_dir, vec2 uv, vec3 map_n) {
    vec3 dp1 = dFdx(-view_dir);
    vec3 dp2 = dFdy(-view_dir);
    vec2 duv1 = dFdx(uv);
    vec2 duv2 = dFdy(uv);
    vec3 dp2perp = cross(dp2, n);
    vec3 dp1perp = cross(n, dp1);
    vec3 t = dp2perp * duv1.x + dp1perp * duv2.x;
    vec3 b = dp2perp * duv1.y + dp1perp * duv2.y;
    float invmax = inversesqrt(max(dot(t, t), dot(b, b)) + 1e-8);
    return normalize(t * (map_n.x * invmax) + b * (map_n.y * invmax) + n * map_n.z);
}"
        } else {
            "vec3 perturb_normal(vec3 n, vec3 view_dir, vec2 uv, vec3 map_n) {
    return n;
}"
        };
        let mesh_fs = format!(
            "{header}{precision}
uniform sampler2D u_tex;
uniform sampler2D u_normal_tex;
uniform sampler2D u_emissive_tex;
uniform sampler2D u_met_tex;
uniform vec3 u_cam_pos;
uniform vec4 u_base_color;
uniform vec3 u_emissive;
uniform float u_metallic;
uniform float u_roughness;
uniform int u_alpha_mode;
uniform float u_alpha_cutoff;
uniform int u_use_tex;
uniform int u_use_normal;
uniform int u_use_emissive;
uniform int u_use_met;
uniform int u_shaded;
uniform vec4 u_wire_tint;
{f_in} vec3 v_pos;
{f_in} vec3 v_normal;
{f_in} vec2 v_uv;
{f_out}
{normal_perturb_fn}
vec3 decode_normal_sample(vec4 nm) {{
    if (abs(nm.r - nm.b) < 0.02 && (nm.r > 0.95 || nm.r < 0.05) && nm.a < 0.995) {{
        vec2 xy = nm.ag * 2.0 - 1.0;
        float z = sqrt(max(0.0, 1.0 - dot(xy, xy)));
        return vec3(xy, z);
    }}
    return normalize(nm.xyz * 2.0 - 1.0);
}}
void main() {{
    if (u_wire_tint.a > 0.001) {{
        {frag_target} = u_wire_tint;
        return;
    }}
    vec4 texel = (u_use_tex != 0) ? {tex_fn}(u_tex, v_uv) : vec4(1.0);
    vec4 tint = (u_use_tex != 0)
        ? clamp(u_base_color, 0.0, 1.0)
        : vec4(clamp(u_base_color.rgb, 0.05, 1.0), clamp(u_base_color.a, 0.0, 1.0));
    vec4 base = texel * tint;
    if (u_alpha_mode == 1 && base.a < u_alpha_cutoff) {{
        discard;
    }}
    vec3 albedo = base.rgb;
    if (u_use_tex == 0 && length(u_emissive) < 0.01) {{
        albedo *= vec3(0.78, 0.87, 1.0);
    }}
    float out_alpha = (u_alpha_mode == 2) ? base.a : 1.0;
    vec3 emi = u_emissive;
    if (u_use_emissive != 0) {{
        emi *= {tex_fn}(u_emissive_tex, v_uv).rgb;
    }}
    if (u_shaded == 0) {{
        {frag_target} = vec4(albedo + emi, out_alpha);
        return;
    }}
    vec3 g_norm = normalize(v_normal);
    if (!gl_FrontFacing) {{
        g_norm = -g_norm;
    }}
    vec3 view_vec = u_cam_pos - v_pos;
    float v_len = max(length(view_vec), 1e-5);
    vec3 V = view_vec / v_len;
    vec3 N = g_norm;
    if (u_use_normal != 0) {{
        vec3 map_n = decode_normal_sample({tex_fn}(u_normal_tex, v_uv));
        N = perturb_normal(g_norm, view_vec, v_uv, map_n);
    }}
    float metallic = clamp(u_metallic, 0.0, 1.0);
    float roughness = clamp(u_roughness, 0.06, 1.0);
    if (u_use_met != 0) {{
        vec4 mg = {tex_fn}(u_met_tex, v_uv);
        metallic = clamp(mg.r, 0.0, 1.0);
        if (mg.a < 0.999) {{
            roughness = clamp(1.0 - mg.a, 0.06, 1.0);
        }}
    }}
    vec3 L1 = normalize(vec3(0.55, 0.78, 0.32));
    vec3 L2 = normalize(vec3(-0.45, 0.25, -0.82));
    vec3 f0 = mix(vec3(0.04), albedo, metallic);
    vec3 kd = albedo * (1.0 - metallic);
    float shininess = mix(96.0, 6.0, roughness * roughness);
    vec3 H1 = normalize(L1 + V);
    float spec1 = pow(max(dot(N, H1), 0.0), shininess) * (shininess + 8.0) * 0.025;
    float fres = pow(clamp(1.0 - max(dot(N, V), 0.0), 0.0, 1.0), 5.0);
    vec3 F = f0 + (1.0 - f0) * fres;
    float nl1 = max(dot(N, L1), 0.0);
    float nl2 = max(dot(N, L2), 0.0);
    float hemi = clamp(N.y * 0.5 + 0.5, 0.0, 1.0);
    vec3 amb = kd * (0.28 + 0.14 * hemi) + f0 * (0.14 + 0.10 * hemi);
    vec3 direct = kd * (nl1 * 0.85 + nl2 * 0.28) + F * spec1 * (nl1 + 0.15);
    vec3 color = amb + direct + emi;
    {frag_target} = vec4(color, out_alpha);
}}
"
        );

        let line_vs = format!(
            "{header}{precision}
uniform mat4 u_mvp;
uniform float u_depth_bias;
{v_in} vec3 a_pos;
{v_in} vec4 a_color;
{v_out} vec4 v_color;
void main() {{
    vec4 clip = u_mvp * vec4(a_pos, 1.0);
    clip.z += u_depth_bias * clip.w;
    gl_Position = clip;
    v_color = a_color;
}}
"
        );
        let line_fs = format!(
            "{header}{precision}
{f_in} vec4 v_color;
{f_out}
void main() {{
    {frag_target} = v_color;
}}
"
        );

        unsafe {
            let mesh_program = compile_program(
                gl,
                &mesh_vs,
                &mesh_fs,
                &[(0, "a_pos"), (1, "a_normal"), (2, "a_uv")],
            )?;
            let line_program =
                compile_program(gl, &line_vs, &line_fs, &[(0, "a_pos"), (1, "a_color")])?;

            let mesh_vbo = gl.create_buffer()?;
            let mesh_ebo = gl.create_buffer()?;
            let mesh_vao = if gl.supports_vertex_array() {
                let vao = gl.create_vertex_array()?;
                gl.bind_vertex_array(Some(vao));
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(mesh_vbo));
                let stride = (8 * size_of::<f32>()) as i32;
                gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
                gl.enable_vertex_attrib_array(0);
                gl.vertex_attrib_pointer_f32(
                    1,
                    3,
                    glow::FLOAT,
                    false,
                    stride,
                    (3 * size_of::<f32>()) as i32,
                );
                gl.enable_vertex_attrib_array(1);
                gl.vertex_attrib_pointer_f32(
                    2,
                    2,
                    glow::FLOAT,
                    false,
                    stride,
                    (6 * size_of::<f32>()) as i32,
                );
                gl.enable_vertex_attrib_array(2);
                gl.bind_vertex_array(None);
                Some(vao)
            } else {
                None
            };

            let line_vbo = gl.create_buffer()?;
            let line_vao = if gl.supports_vertex_array() {
                let vao = gl.create_vertex_array()?;
                gl.bind_vertex_array(Some(vao));
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(line_vbo));
                let stride = (7 * size_of::<f32>()) as i32;
                gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
                gl.enable_vertex_attrib_array(0);
                gl.vertex_attrib_pointer_f32(
                    1,
                    4,
                    glow::FLOAT,
                    false,
                    stride,
                    (3 * size_of::<f32>()) as i32,
                );
                gl.enable_vertex_attrib_array(1);
                gl.bind_vertex_array(None);
                Some(vao)
            } else {
                None
            };

            let white_tex = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(white_tex));
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                1,
                1,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&[255u8, 255, 255, 255])),
            );
            gl.bind_texture(glow::TEXTURE_2D, None);

            Ok(Self {
                u_mesh_mvp: gl.get_uniform_location(mesh_program, "u_mvp"),
                u_mesh_cam_pos: gl.get_uniform_location(mesh_program, "u_cam_pos"),
                u_mesh_base_color: gl.get_uniform_location(mesh_program, "u_base_color"),
                u_mesh_emissive: gl.get_uniform_location(mesh_program, "u_emissive"),
                u_mesh_metallic: gl.get_uniform_location(mesh_program, "u_metallic"),
                u_mesh_roughness: gl.get_uniform_location(mesh_program, "u_roughness"),
                u_mesh_alpha_mode: gl.get_uniform_location(mesh_program, "u_alpha_mode"),
                u_mesh_alpha_cutoff: gl.get_uniform_location(mesh_program, "u_alpha_cutoff"),
                u_mesh_use_tex: gl.get_uniform_location(mesh_program, "u_use_tex"),
                u_mesh_use_normal: gl.get_uniform_location(mesh_program, "u_use_normal"),
                u_mesh_use_emissive: gl.get_uniform_location(mesh_program, "u_use_emissive"),
                u_mesh_use_met: gl.get_uniform_location(mesh_program, "u_use_met"),
                u_mesh_shaded: gl.get_uniform_location(mesh_program, "u_shaded"),
                u_mesh_wire_tint: gl.get_uniform_location(mesh_program, "u_wire_tint"),
                u_mesh_depth_bias: gl.get_uniform_location(mesh_program, "u_depth_bias"),
                u_mesh_tex: gl.get_uniform_location(mesh_program, "u_tex"),
                u_mesh_normal_tex: gl.get_uniform_location(mesh_program, "u_normal_tex"),
                u_mesh_emissive_tex: gl.get_uniform_location(mesh_program, "u_emissive_tex"),
                u_mesh_met_tex: gl.get_uniform_location(mesh_program, "u_met_tex"),
                u_line_mvp: gl.get_uniform_location(line_program, "u_mvp"),
                u_line_depth_bias: gl.get_uniform_location(line_program, "u_depth_bias"),
                mesh_program,
                line_program,
                mesh_vao,
                mesh_vbo,
                mesh_ebo,
                line_vao,
                line_vbo,
                white_tex,
                compiled_ok: true,
                last_error: None,
            })
        }
    }

    fn destroy(self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.mesh_program);
            gl.delete_program(self.line_program);
            if let Some(v) = self.mesh_vao {
                gl.delete_vertex_array(v);
            }
            if let Some(v) = self.line_vao {
                gl.delete_vertex_array(v);
            }
            gl.delete_buffer(self.mesh_vbo);
            gl.delete_buffer(self.mesh_ebo);
            gl.delete_buffer(self.line_vbo);
            gl.delete_texture(self.white_tex);
        }
    }

    fn paint(&self, painter: &egui_glow::Painter, packet: &GlRenderPacket) {
        if !self.compiled_ok {
            return;
        }
        let gl = painter.gl();
        unsafe {
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.depth_mask(true);
            gl.clear_depth_f32(1.0);
            gl.clear(glow::DEPTH_BUFFER_BIT);

            // Grid floor lines first (behind geometry in Z)
            if !packet.grid_line_verts.is_empty() {
                self.draw_lines(gl, &packet.mvp, 0.0, &packet.grid_line_verts, 1.0);
            }

            if packet.backface_cull {
                gl.enable(glow::CULL_FACE);
                gl.cull_face(glow::BACK);
                gl.front_face(glow::CCW);
            } else {
                gl.disable(glow::CULL_FACE);
            }

            gl.use_program(Some(self.mesh_program));
            gl.uniform_matrix_4_f32_slice(self.u_mesh_mvp.as_ref(), false, &packet.mvp);
            gl.uniform_3_f32_slice(self.u_mesh_cam_pos.as_ref(), &packet.cam_world_pos);
            gl.uniform_1_i32(self.u_mesh_tex.as_ref(), 0);
            gl.uniform_1_i32(self.u_mesh_normal_tex.as_ref(), 1);
            gl.uniform_1_i32(self.u_mesh_emissive_tex.as_ref(), 2);
            gl.uniform_1_i32(self.u_mesh_met_tex.as_ref(), 3);

            if let Some(vao) = self.mesh_vao {
                gl.bind_vertex_array(Some(vao));
            }
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.mesh_vbo));
            if self.mesh_vao.is_none() {
                let stride = (8 * size_of::<f32>()) as i32;
                gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
                gl.enable_vertex_attrib_array(0);
                gl.vertex_attrib_pointer_f32(
                    1,
                    3,
                    glow::FLOAT,
                    false,
                    stride,
                    (3 * size_of::<f32>()) as i32,
                );
                gl.enable_vertex_attrib_array(1);
                gl.vertex_attrib_pointer_f32(
                    2,
                    2,
                    glow::FLOAT,
                    false,
                    stride,
                    (6 * size_of::<f32>()) as i32,
                );
                gl.enable_vertex_attrib_array(2);
            }
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.mesh_ebo));

            // Pass 1: opaque and alpha-cutout submeshes + wireframe overlays.
            gl.disable(glow::BLEND);
            gl.depth_mask(true);
            let mut has_transparent = false;
            for part in &packet.parts {
                if part.vertices.is_empty() || part.tri_indices.is_empty() {
                    continue;
                }
                gl.buffer_data_u8_slice(
                    glow::ARRAY_BUFFER,
                    as_u8_slice(&part.vertices),
                    glow::STREAM_DRAW,
                );

                if packet.shaded {
                    gl.buffer_data_u8_slice(
                        glow::ELEMENT_ARRAY_BUFFER,
                        as_u8_slice(&part.tri_indices),
                        glow::STREAM_DRAW,
                    );
                    gl.uniform_1_i32(self.u_mesh_shaded.as_ref(), 1);
                    gl.uniform_4_f32(self.u_mesh_wire_tint.as_ref(), 0.0, 0.0, 0.0, 0.0);
                    gl.uniform_1_f32(self.u_mesh_depth_bias.as_ref(), 0.0);
                    for sub in &part.submeshes {
                        if sub.index_count < 3 {
                            continue;
                        }
                        if sub.alpha_mode == 2 {
                            has_transparent = true;
                            continue;
                        }
                        self.draw_submesh_pbr(gl, painter, packet, sub);
                    }
                }

                if (packet.wireframe || !packet.shaded) && !part.edge_indices.is_empty() {
                    if !packet.shaded {
                        self.apply_submesh_cull(gl, packet.backface_cull, false);
                        gl.buffer_data_u8_slice(
                            glow::ELEMENT_ARRAY_BUFFER,
                            as_u8_slice(&part.tri_indices),
                            glow::STREAM_DRAW,
                        );
                        gl.uniform_1_i32(self.u_mesh_shaded.as_ref(), 0);
                        gl.uniform_4_f32(self.u_mesh_wire_tint.as_ref(), 0.0, 0.0, 0.0, 0.0);
                        gl.uniform_1_f32(self.u_mesh_depth_bias.as_ref(), 0.0);
                        for sub in &part.submeshes {
                            if sub.index_count >= 3 && sub.alpha_mode != 2 {
                                self.draw_submesh_pbr(gl, painter, packet, sub);
                            }
                        }
                    }
                    gl.disable(glow::CULL_FACE);
                    gl.uniform_4_f32(self.u_mesh_wire_tint.as_ref(), 0.59, 0.82, 1.0, 0.65);
                    gl.uniform_1_f32(self.u_mesh_depth_bias.as_ref(), -0.0008);
                    gl.buffer_data_u8_slice(
                        glow::ELEMENT_ARRAY_BUFFER,
                        as_u8_slice(&part.edge_indices),
                        glow::STREAM_DRAW,
                    );
                    gl.draw_elements(
                        glow::LINES,
                        part.edge_indices.len() as i32,
                        glow::UNSIGNED_INT,
                        0,
                    );
                }
            }

            // Pass 2: alpha-blended submeshes drawn after opaque geometry.
            if has_transparent {
                gl.enable(glow::BLEND);
                gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                gl.depth_mask(false);
                for part in &packet.parts {
                    if part.vertices.is_empty() || part.tri_indices.is_empty() {
                        continue;
                    }
                    if !part.submeshes.iter().any(|s| s.alpha_mode == 2 && s.index_count >= 3) {
                        continue;
                    }
                    gl.buffer_data_u8_slice(
                        glow::ARRAY_BUFFER,
                        as_u8_slice(&part.vertices),
                        glow::STREAM_DRAW,
                    );
                    gl.buffer_data_u8_slice(
                        glow::ELEMENT_ARRAY_BUFFER,
                        as_u8_slice(&part.tri_indices),
                        glow::STREAM_DRAW,
                    );
                    gl.uniform_1_i32(self.u_mesh_shaded.as_ref(), i32::from(packet.shaded));
                    gl.uniform_4_f32(self.u_mesh_wire_tint.as_ref(), 0.0, 0.0, 0.0, 0.0);
                    gl.uniform_1_f32(self.u_mesh_depth_bias.as_ref(), 0.0);
                    for sub in &part.submeshes {
                        if sub.alpha_mode == 2 && sub.index_count >= 3 {
                            self.draw_submesh_pbr(gl, painter, packet, sub);
                        }
                    }
                }
                gl.depth_mask(true);
                gl.disable(glow::BLEND);
            }
            gl.active_texture(glow::TEXTURE0);

            // Skeleton bones drawn slightly biased on top for visibility
            if packet.show_bones && !packet.bone_line_verts.is_empty() {
                gl.disable(glow::DEPTH_TEST);
                self.draw_lines(gl, &packet.mvp, -0.001, &packet.bone_line_verts, 2.0);
            }

            gl.disable(glow::CULL_FACE);
            gl.disable(glow::DEPTH_TEST);
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.use_program(None);
        }
    }

    unsafe fn apply_submesh_cull(
        &self,
        gl: &glow::Context,
        backface_cull: bool,
        double_sided: bool,
    ) {
        unsafe {
            if backface_cull && !double_sided {
                gl.enable(glow::CULL_FACE);
                gl.cull_face(glow::BACK);
                gl.front_face(glow::CCW);
            } else {
                gl.disable(glow::CULL_FACE);
            }
        }
    }

    unsafe fn bind_slot_tex(
        &self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        unit: u32,
        tex: Option<egui::TextureId>,
        loc: Option<&glow::UniformLocation>,
    ) {
        unsafe {
            gl.active_texture(unit);
            let native = tex.and_then(|id| painter.texture(id));
            gl.bind_texture(glow::TEXTURE_2D, Some(native.unwrap_or(self.white_tex)));
            gl.uniform_1_i32(loc, i32::from(native.is_some()));
        }
    }

    unsafe fn draw_submesh_pbr(
        &self,
        gl: &glow::Context,
        painter: &egui_glow::Painter,
        packet: &GlRenderPacket,
        sub: &GlSubMeshMaterial,
    ) {
        unsafe {
            self.apply_submesh_cull(gl, packet.backface_cull, sub.double_sided);
            let tex_on = packet.textured;
            self.bind_slot_tex(
                gl,
                painter,
                glow::TEXTURE0,
                tex_on.then_some(sub.albedo_tex).flatten(),
                self.u_mesh_use_tex.as_ref(),
            );
            self.bind_slot_tex(
                gl,
                painter,
                glow::TEXTURE1,
                tex_on.then_some(sub.normal_tex).flatten(),
                self.u_mesh_use_normal.as_ref(),
            );
            self.bind_slot_tex(
                gl,
                painter,
                glow::TEXTURE2,
                tex_on.then_some(sub.emissive_tex).flatten(),
                self.u_mesh_use_emissive.as_ref(),
            );
            self.bind_slot_tex(
                gl,
                painter,
                glow::TEXTURE3,
                tex_on.then_some(sub.metallic_gloss_tex).flatten(),
                self.u_mesh_use_met.as_ref(),
            );
            gl.uniform_4_f32_slice(self.u_mesh_base_color.as_ref(), &sub.base_color);
            gl.uniform_3_f32_slice(self.u_mesh_emissive.as_ref(), &sub.emissive_color);
            gl.uniform_1_f32(self.u_mesh_metallic.as_ref(), sub.metallic);
            gl.uniform_1_f32(self.u_mesh_roughness.as_ref(), sub.roughness);
            gl.uniform_1_i32(self.u_mesh_alpha_mode.as_ref(), i32::from(sub.alpha_mode));
            gl.uniform_1_f32(self.u_mesh_alpha_cutoff.as_ref(), sub.alpha_cutoff);
            gl.draw_elements(
                glow::TRIANGLES,
                sub.index_count as i32,
                glow::UNSIGNED_INT,
                (sub.index_start * size_of::<u32>()) as i32,
            );
        }
    }

    unsafe fn draw_lines(
        &self,
        gl: &glow::Context,
        mvp: &[f32; 16],
        depth_bias: f32,
        verts: &[f32],
        width: f32,
    ) {
        unsafe {
            gl.use_program(Some(self.line_program));
            gl.uniform_matrix_4_f32_slice(self.u_line_mvp.as_ref(), false, mvp);
            gl.uniform_1_f32(self.u_line_depth_bias.as_ref(), depth_bias);
            gl.line_width(width);
            if let Some(vao) = self.line_vao {
                gl.bind_vertex_array(Some(vao));
            }
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.line_vbo));
            if self.line_vao.is_none() {
                let stride = (7 * size_of::<f32>()) as i32;
                gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
                gl.enable_vertex_attrib_array(0);
                gl.vertex_attrib_pointer_f32(
                    1,
                    4,
                    glow::FLOAT,
                    false,
                    stride,
                    (3 * size_of::<f32>()) as i32,
                );
                gl.enable_vertex_attrib_array(1);
            }
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, as_u8_slice(verts), glow::STREAM_DRAW);
            gl.draw_arrays(glow::LINES, 0, (verts.len() / 7) as i32);
        }
    }
}

trait SupportsVao {
    fn supports_vertex_array(&self) -> bool;
}

impl SupportsVao for glow::Context {
    fn supports_vertex_array(&self) -> bool {
        let v = self.version();
        v.major >= 3 || (v.is_embedded && v.major >= 3)
    }
}

unsafe fn compile_program(
    gl: &glow::Context,
    vs_src: &str,
    fs_src: &str,
    attribs: &[(u32, &str)],
) -> Result<glow::Program, String> {
    unsafe {
        let vs = gl.create_shader(glow::VERTEX_SHADER)?;
        gl.shader_source(vs, vs_src);
        gl.compile_shader(vs);
        if !gl.get_shader_compile_status(vs) {
            let log = gl.get_shader_info_log(vs);
            gl.delete_shader(vs);
            return Err(format!("vertex shader error: {log}"));
        }

        let fs = gl.create_shader(glow::FRAGMENT_SHADER)?;
        gl.shader_source(fs, fs_src);
        gl.compile_shader(fs);
        if !gl.get_shader_compile_status(fs) {
            let log = gl.get_shader_info_log(fs);
            gl.delete_shader(vs);
            gl.delete_shader(fs);
            return Err(format!("fragment shader error: {log}"));
        }

        let prog = gl.create_program()?;
        gl.attach_shader(prog, vs);
        gl.attach_shader(prog, fs);
        for &(loc, name) in attribs {
            gl.bind_attrib_location(prog, loc, name);
        }
        gl.link_program(prog);
        gl.detach_shader(prog, vs);
        gl.detach_shader(prog, fs);
        gl.delete_shader(vs);
        gl.delete_shader(fs);
        if !gl.get_program_link_status(prog) {
            let log = gl.get_program_info_log(prog);
            gl.delete_program(prog);
            return Err(format!("program link error: {log}"));
        }
        Ok(prog)
    }
}

fn as_u8_slice<T: Copy>(slice: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(slice.as_ptr().cast::<u8>(), std::mem::size_of_val(slice)) }
}

/// Dispatches hardware GPU rendering (`egui_glow::CallbackFn`) inside `rect` and paints the
/// crisp XYZ axes / renderer badge overlay on top. Returns `false` if shader init failed so caller
/// can fall back cleanly to CPU software rasterization.
pub fn paint_gpu_scene_viewport(
    ui: &egui::Ui,
    rect: Rect,
    parts: &[ViewportMeshPart],
    bone_segments: &[([f32; 3], [f32; 3])],
    bounds_min: [f32; 3],
    bounds_max: [f32; 3],
    cam: &MeshViewportState,
) -> bool {
    {
        let guard = cam.gpu.inner.lock();
        if let Some(ref res) = *guard
            && !res.compiled_ok
        {
            return false;
        }
    }

    let mvp = build_view_projection_matrix(rect.width(), rect.height(), bounds_min, bounds_max, cam);
    let cam_world_pos = camera_world_pos(bounds_min, bounds_max, cam);
    let want_edges = cam.wireframe || !cam.shaded;
    let mut draw_parts = Vec::with_capacity(parts.len());
    for part in parts {
        if !part.visible || part.mesh.positions.is_empty() || part.mesh.indices.len() < 3 {
            continue;
        }
        let m = &part.mesh;
        let has_normals = m.normals.len() == m.positions.len();
        let has_uvs = m.uvs.len() == m.positions.len();
        let mut vertices = Vec::with_capacity(m.positions.len() * 8);
        for i in 0..m.positions.len() {
            let p = m.positions[i];
            let n = if has_normals {
                m.normals[i]
            } else {
                [0.0, 1.0, 0.0]
            };
            let uv = if has_uvs {
                [m.uvs[i][0], 1.0 - m.uvs[i][1]]
            } else {
                [0.0, 0.0]
            };
            vertices.extend_from_slice(&[p[0], p[1], p[2], n[0], n[1], n[2], uv[0], uv[1]]);
        }
        let mut tri_indices = Vec::with_capacity(m.indices.len());
        let mut edge_indices = if want_edges {
            Vec::with_capacity(m.indices.len().min(60_000) * 2)
        } else {
            Vec::new()
        };
        let vcount = m.positions.len() as u32;
        let fallback_sub = [ViewportSubMesh {
            index_start: 0,
            index_count: (m.indices.len() / 3) * 3,
            material_slot: 0,
        }];
        let src_subs = if part.submeshes.is_empty() {
            &fallback_sub[..]
        } else {
            part.submeshes.as_slice()
        };
        let mut gl_subs = Vec::with_capacity(src_subs.len());
        let default_slot = ViewportMaterialSlot::from_albedo(part.albedo_image.clone(), part.base_color);
        for sub in src_subs {
            let start = sub.index_start.min(m.indices.len());
            let end = start.saturating_add(sub.index_count).min(m.indices.len());
            if start + 3 > end {
                continue;
            }
            let out_start = tri_indices.len();
            for tri in m.indices[start..end].as_chunks::<3>().0 {
                if tri[0] >= vcount || tri[1] >= vcount || tri[2] >= vcount {
                    continue;
                }
                tri_indices.extend_from_slice(tri);
                if want_edges && edge_indices.len() < 120_000 {
                    edge_indices.extend_from_slice(&[tri[0], tri[1], tri[1], tri[2], tri[2], tri[0]]);
                }
            }
            let out_count = tri_indices.len() - out_start;
            if out_count < 3 {
                continue;
            }
            let slot = part.material_for_submesh(sub).unwrap_or(&default_slot);
            let (alpha_mode, alpha_cutoff) = match slot.alpha_mode {
                MaterialAlphaMode::Opaque => (0, 0.5),
                MaterialAlphaMode::Mask { cutoff } => (1, cutoff),
                MaterialAlphaMode::Blend => (2, 0.0),
            };
            let albedo_tex = slot
                .albedo_texture
                .as_ref()
                .map(|t| t.id())
                .or_else(|| (sub.material_slot == 0).then(|| part.albedo_texture.as_ref().map(|t| t.id())).flatten());
            gl_subs.push(GlSubMeshMaterial {
                index_start: out_start,
                index_count: out_count,
                albedo_tex,
                normal_tex: slot.normal_texture.as_ref().map(|t| t.id()),
                emissive_tex: slot.emissive_texture.as_ref().map(|t| t.id()),
                metallic_gloss_tex: slot.metallic_gloss_texture.as_ref().map(|t| t.id()),
                base_color: slot.base_color,
                emissive_color: slot.emissive_color,
                metallic: slot.metallic,
                roughness: slot.roughness,
                alpha_mode,
                alpha_cutoff,
                double_sided: slot.double_sided,
            });
        }
        draw_parts.push(GlMeshDraw {
            vertices,
            tri_indices,
            edge_indices,
            submeshes: gl_subs,
        });
    }

    let grid_line_verts = build_ground_grid_verts(bounds_min, bounds_max);
    let mut bone_line_verts = if cam.show_bones {
        Vec::with_capacity(bone_segments.len() * 14)
    } else {
        Vec::new()
    };
    if cam.show_bones {
        for &(a, b) in bone_segments {
            bone_line_verts.extend_from_slice(&[a[0], a[1], a[2], 1.0, 0.78, 0.25, 1.0]);
            bone_line_verts.extend_from_slice(&[b[0], b[1], b[2], 0.41, 0.94, 1.0, 1.0]);
        }
    }

    let packet = Arc::new(GlRenderPacket {
        mvp,
        cam_world_pos,
        parts: draw_parts,
        grid_line_verts,
        bone_line_verts,
        shaded: cam.shaded,
        textured: cam.textured,
        wireframe: cam.wireframe,
        backface_cull: cam.backface_cull,
        show_bones: cam.show_bones,
    });

    let gpu_handle = cam.gpu.clone();
    let cb = egui_glow::CallbackFn::new(move |_info, painter| {
        let mut guard = gpu_handle.inner.lock();
        let res = guard.get_or_insert_with(|| GlViewportResources::create(painter.gl()));
        if res.compiled_ok {
            res.paint(painter, &packet);
        }
    });

    let painter = ui.painter().with_clip_rect(rect);
    painter.add(egui::PaintCallback {
        rect,
        callback: Arc::new(cb),
    });

    if cam.show_axes {
        paint_axes_gizmo(&painter, rect, cam);
    }

    let label = if let Some(ref err) = cam
        .gpu
        .inner
        .lock()
        .as_ref()
        .and_then(|r| r.last_error.clone())
    {
        format!("GPU fallback: {err}")
    } else {
        "GPU OpenGL PBR".to_owned()
    };
    painter.text(
        pos2(rect.right() - 8.0, rect.top() + 8.0),
        egui::Align2::RIGHT_TOP,
        label,
        egui::FontId::monospace(10.0),
        Color32::from_rgba_unmultiplied(155, 225, 180, 190),
    );
    true
}

fn build_ground_grid_verts(bounds_min: [f32; 3], bounds_max: [f32; 3]) -> Vec<f32> {
    let cx = (bounds_min[0] + bounds_max[0]) * 0.5;
    let cz = (bounds_min[2] + bounds_max[2]) * 0.5;
    let y = bounds_min[1];
    let span = ((bounds_max[0] - bounds_min[0])
        .hypot(bounds_max[2] - bounds_min[2])
        * 0.85)
        .max(0.5);
    let half_steps = 5i32;
    let step = span / half_steps as f32;
    let mut out = Vec::with_capacity(((half_steps * 2 + 1) as usize) * 28);
    for i in -half_steps..=half_steps {
        let offset = i as f32 * step;
        let is_axis = i == 0;
        let col = if is_axis {
            [0.32, 0.42, 0.56, 0.75]
        } else {
            [0.24, 0.27, 0.33, 0.6]
        };
        // Line along Z
        out.extend_from_slice(&[cx + offset, y, cz - span, col[0], col[1], col[2], col[3]]);
        out.extend_from_slice(&[cx + offset, y, cz + span, col[0], col[1], col[2], col[3]]);
        // Line along X
        out.extend_from_slice(&[cx - span, y, cz + offset, col[0], col[1], col[2], col[3]]);
        out.extend_from_slice(&[cx + span, y, cz + offset, col[0], col[1], col[2], col[3]]);
    }
    out
}

/// Paints the bottom-left 3D orientation axes indicator matching [`MeshViewportState`].
pub fn paint_axes_gizmo(painter: &egui::Painter, rect: Rect, cam: &MeshViewportState) {
    let basis = camera_basis(cam);
    let origin: Pos2 = pos2(rect.left() + 34.0, rect.bottom() - 34.0);
    let axis_len = 22.0;
    for (axis, color, label) in [
        ([1.0, 0.0, 0.0], Color32::from_rgb(240, 85, 85), "X"),
        ([0.0, 1.0, 0.0], Color32::from_rgb(95, 225, 110), "Y"),
        ([0.0, 0.0, 1.0], Color32::from_rgb(95, 160, 250), "Z"),
    ] {
        let sx = dot3(axis, basis.right);
        let sy_a = dot3(axis, basis.up);
        let tip = pos2(origin.x + sx * axis_len, origin.y - sy_a * axis_len);
        painter.line_segment([origin, tip], Stroke::new(2.0, color));
        painter.text(
            tip,
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::monospace(10.0),
            color,
        );
    }
}

struct CameraBasis {
    forward: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    cam_pos: [f32; 3],
}

fn camera_basis(cam: &MeshViewportState) -> CameraBasis {
    let (sy, cy) = cam.yaw.sin_cos();
    let (sp, cp) = cam.pitch.sin_cos();
    let forward = normalize3([-cy * cp, -sp, -sy * cp]);
    let right = normalize3(cross3(forward, [0.0, 1.0, 0.0]));
    let up = normalize3(cross3(right, forward));
    let cam_pos = [
        -forward[0] * cam.distance,
        -forward[1] * cam.distance,
        -forward[2] * cam.distance,
    ];
    CameraBasis {
        forward,
        right,
        up,
        cam_pos,
    }
}

fn camera_world_pos(
    bounds_min: [f32; 3],
    bounds_max: [f32; 3],
    cam: &MeshViewportState,
) -> [f32; 3] {
    let center = [
        (bounds_min[0] + bounds_max[0]) * 0.5,
        (bounds_min[1] + bounds_max[1]) * 0.5,
        (bounds_min[2] + bounds_max[2]) * 0.5,
    ];
    let extent = [
        bounds_max[0] - bounds_min[0],
        bounds_max[1] - bounds_min[1],
        bounds_max[2] - bounds_min[2],
    ];
    let radius = (extent[0] * extent[0] + extent[1] * extent[1] + extent[2] * extent[2])
        .sqrt()
        .max(1e-4)
        * 0.5;
    let basis = camera_basis(cam);
    [
        center[0] + radius * basis.cam_pos[0],
        center[1] + radius * basis.cam_pos[1],
        center[2] + radius * basis.cam_pos[2],
    ]
}

/// Builds a column-major 4x4 MVP matrix matching [`crate::views::preview`] CPU camera math exactly.
pub fn build_view_projection_matrix(
    width_pts: f32,
    height_pts: f32,
    bounds_min: [f32; 3],
    bounds_max: [f32; 3],
    cam: &MeshViewportState,
) -> [f32; 16] {
    let w = width_pts.max(1.0);
    let h = height_pts.max(1.0);
    let center = [
        (bounds_min[0] + bounds_max[0]) * 0.5,
        (bounds_min[1] + bounds_max[1]) * 0.5,
        (bounds_min[2] + bounds_max[2]) * 0.5,
    ];
    let extent = [
        bounds_max[0] - bounds_min[0],
        bounds_max[1] - bounds_min[1],
        bounds_max[2] - bounds_min[2],
    ];
    let radius = (extent[0] * extent[0] + extent[1] * extent[1] + extent[2] * extent[2])
        .sqrt()
        .max(1e-4)
        * 0.5;
    let inv_r = 1.0 / radius;

    let CameraBasis {
        forward,
        right,
        up,
        cam_pos,
    } = camera_basis(cam);

    // View matrix mapping world point `p` -> `(rx, ry, rz, 1)` in camera basis:
    // np = (p - center) * inv_r; r = R * (np - cam_pos)
    let view_col = {
        let tx = -dot3(center, right) * inv_r - dot3(cam_pos, right);
        let ty = -dot3(center, up) * inv_r - dot3(cam_pos, up);
        let tz = -dot3(center, forward) * inv_r - dot3(cam_pos, forward);
        [
            right[0] * inv_r,
            up[0] * inv_r,
            forward[0] * inv_r,
            0.0,
            right[1] * inv_r,
            up[1] * inv_r,
            forward[1] * inv_r,
            0.0,
            right[2] * inv_r,
            up[2] * inv_r,
            forward[2] * inv_r,
            0.0,
            tx,
            ty,
            tz,
            1.0,
        ]
    };

    let scale = w.min(h) * 0.9;
    let sx = 2.0 * scale / w;
    let sy = 2.0 * scale / h;
    let px = cam.pan.x * h / w;
    let py = -cam.pan.y;
    let z_near = 0.05_f32;
    let z_far = 80.0_f32;
    let a = (z_far + z_near) / (z_far - z_near);
    let b = -(2.0 * z_far * z_near) / (z_far - z_near);

    // Projection matrix (column-major) mapping `(rx, ry, rz, 1)` -> clip `(cx, cy, cz, rz)`:
    let proj_col = [
        sx, 0.0, 0.0, 0.0,
        0.0, sy, 0.0, 0.0,
        px, py, a, 1.0,
        0.0, 0.0, b, 0.0,
    ];

    mul_mat4_col(&proj_col, &view_col)
}

fn mul_mat4_col(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for c in 0..4 {
        for r in 0..4 {
            out[c * 4 + r] = a[r] * b[c * 4]
                + a[4 + r] * b[c * 4 + 1]
                + a[8 + r] * b[c * 4 + 2]
                + a[12 + r] * b[c * 4 + 3];
        }
    }
    out
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-6 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 1.0, 0.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_mvp_projects_bounds_center_with_cpu_parity() {
        let mut cam = MeshViewportState::reset();
        cam.pan = egui::vec2(0.1, -0.2);
        let bmin = [-2.0, 0.0, -2.0];
        let bmax = [2.0, 4.0, 2.0];
        let w = 800.0;
        let h = 600.0;
        let mvp = build_view_projection_matrix(w, h, bmin, bmax, &cam);

        // Bounds center (0, 2, 0) maps to view `(0, 0, cam.distance)`
        let cx = mvp[12] + mvp[4] * 2.0;
        let cy = mvp[13] + mvp[5] * 2.0;
        let cw = mvp[15] + mvp[7] * 2.0;
        assert!((cw - cam.distance).abs() < 1e-4, "expected w={}", cam.distance);
        let ndc_x = cx / cw;
        let ndc_y = cy / cw;
        assert!((ndc_x - cam.pan.x * h / w).abs() < 1e-4);
        assert!((ndc_y + cam.pan.y).abs() < 1e-4);
    }
}
