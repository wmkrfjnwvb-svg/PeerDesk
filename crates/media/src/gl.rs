//! 用显卡显示 NV12 画面（OpenGL 系统胶水）：两张纹理（亮度 R8、交错色度 RG8），片元着色器按 BT.601
//! 有限范围转成 RGB，和编码端（采集按 BT.601 转 YUV）、CPU 上的 [`peerdesk_core::frame::nv12_to_rgb`] 一致。
//!
//! 为什么（2026-11-25）：控制端解码后在 CPU 上转 RGB、再转成界面的格式，1872×1170 每帧 30–50 ms；
//! 被控端改按原分辨率编码后像素多一倍多。业内（WebRTC、Chromium）解码出的 YUV 直接交给显卡转颜色。
//! 上传的数据也只有 RGBA 的 3/8。
//!
//! 只在有 OpenGL 上下文时能跑（界面里），覆盖率统计不含这个文件，由界面截图测试和实机验证。

#![allow(unsafe_code)]

use glow::HasContext as _;
use peerdesk_core::frame::Nv12;

/// 画一个铺满视口的矩形，顶点由 `gl_VertexID` 生成（不需要顶点缓冲）。
const VERTEX: &str = r"
out vec2 v_uv;
void main() {
    vec2 p = vec2(float((gl_VertexID & 1) * 2 - 1), float((gl_VertexID >> 1) * 2 - 1));
    v_uv = vec2((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
    gl_Position = vec4(p, 0.0, 1.0);
}
";

/// BT.601 有限范围：Y 16–235、UV 16–240。系数和 `nv12_to_rgb` 的整数系数一样（×256 取整前的值）。
const FRAGMENT: &str = r"
#ifdef GL_ES
precision mediump float;
#endif
uniform sampler2D u_y;
uniform sampler2D u_uv;
in vec2 v_uv;
out vec4 out_color;
void main() {
    float y = 1.1640625 * (texture(u_y, v_uv).r - 0.0627451);
    vec2 c = texture(u_uv, v_uv).rg - vec2(0.5019608);
    out_color = vec4(
        y + 1.59765625 * c.y,
        y - 0.390625 * c.x - 0.8125 * c.y,
        y + 2.015625 * c.x,
        1.0);
}
";

/// 一个窗口的 NV12 显示：着色器程序和两张纹理。在界面的绘制回调里（GL 线程上）创建、上传、绘制、销毁。
#[derive(Debug)]
pub struct Nv12Renderer {
    program: glow::Program,
    vao: glow::VertexArray,
    y: glow::Texture,
    uv: glow::Texture,
    size: (i32, i32),
}

impl Nv12Renderer {
    /// 创建。`version` 是着色器开头的版本声明（例如 `#version 140`），`new_interface` 表示支持
    /// `in`/`out` 和 `gl_VertexID`（GLSL 1.40、ES 3.00 起）；不支持时返回原因，调用方改在 CPU 上转颜色。
    ///
    /// # Errors
    /// 着色器版本太老、编译或链接失败、建不了纹理。
    pub fn new(gl: &glow::Context, version: &str, new_interface: bool) -> Result<Self, String> {
        if !new_interface {
            return Err(format!(
                "OpenGL 版本太老（{}），不能用显卡转颜色",
                version.trim()
            ));
        }
        // SAFETY: 所有调用都在调用方保证当前有效的 GL 上下文上进行；创建的对象由本结构持有，
        // 由 `destroy` 在同一个上下文上删除。
        unsafe {
            let program = gl.create_program()?;
            let mut shaders = Vec::new();
            for (kind, body) in [
                (glow::VERTEX_SHADER, VERTEX),
                (glow::FRAGMENT_SHADER, FRAGMENT),
            ] {
                let shader = gl.create_shader(kind)?;
                gl.shader_source(shader, &format!("{version}{body}"));
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    let log = gl.get_shader_info_log(shader);
                    gl.delete_shader(shader);
                    gl.delete_program(program);
                    return Err(format!("着色器编译失败：{log}"));
                }
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.link_program(program);
            let linked = gl.get_program_link_status(program);
            for shader in shaders {
                gl.detach_shader(program, shader);
                gl.delete_shader(shader);
            }
            if !linked {
                let log = gl.get_program_info_log(program);
                gl.delete_program(program);
                return Err(format!("着色器链接失败：{log}"));
            }
            let vao = gl.create_vertex_array()?;
            let y = texture(gl)?;
            let uv = texture(gl)?;
            Ok(Self {
                program,
                vao,
                y,
                uv,
                size: (0, 0),
            })
        }
    }

    /// 上传一帧（尺寸变了重新分配纹理）。
    pub fn upload(&mut self, gl: &glow::Context, frame: &Nv12) {
        let (w, h) = (i32::from(frame.width()), i32::from(frame.height()));
        let (luma, chroma) = frame.planes();
        let fresh = self.size != (w, h);
        // SAFETY: 纹理是本结构在同一个上下文上建的；数据长度由 Nv12 保证是 宽×高 和 宽×高/2。
        unsafe {
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            for (tex, internal, format, tw, th, data) in [
                (self.y, glow::R8, glow::RED, w, h, luma),
                (self.uv, glow::RG8, glow::RG, w / 2, h / 2, chroma),
            ] {
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                if fresh {
                    gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        i32::try_from(internal).unwrap_or(0),
                        tw,
                        th,
                        0,
                        format,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(data)),
                    );
                } else {
                    gl.tex_sub_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        0,
                        0,
                        tw,
                        th,
                        format,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(data)),
                    );
                }
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
        }
        self.size = (w, h);
    }

    /// 画到当前视口（界面已经把视口设成画面所在的矩形）。还没有上传过画面时什么都不画。
    pub fn paint(&self, gl: &glow::Context) {
        if self.size == (0, 0) {
            return;
        }
        // SAFETY: 程序、纹理、顶点数组都是本结构在同一个上下文上建的；画完把纹理单元恢复成 0。
        unsafe {
            gl.use_program(Some(self.program));
            for (unit, tex, name) in [(0u32, self.y, "u_y"), (1u32, self.uv, "u_uv")] {
                gl.active_texture(glow::TEXTURE0 + unit);
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                let location = gl.get_uniform_location(self.program, name);
                gl.uniform_1_i32(location.as_ref(), i32::try_from(unit).unwrap_or(0));
            }
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.bind_vertex_array(None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.use_program(None);
        }
    }

    /// 在同一个上下文上删除程序和纹理。
    pub fn destroy(self, gl: &glow::Context) {
        // SAFETY: 这些对象是本结构在同一个上下文上建的，删除后不再使用（self 被消耗）。
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
            gl.delete_texture(self.y);
            gl.delete_texture(self.uv);
        }
    }
}

/// 建一张纹理：线性过滤（窗口比画面小时缩小显示）、边缘夹紧。
///
/// # Safety
/// 调用方保证 GL 上下文有效。
unsafe fn texture(gl: &glow::Context) -> Result<glow::Texture, String> {
    // SAFETY: 由调用方保证上下文有效；参数都是 GL 定义的常量。
    unsafe {
        let tex = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        for (param, value) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, param, i32::try_from(value).unwrap_or(0));
        }
        gl.bind_texture(glow::TEXTURE_2D, None);
        Ok(tex)
    }
}
