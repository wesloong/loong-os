//! 自定义渲染元素。

use smithay::{
    backend::renderer::{
        Renderer, Texture,
        element::{
            Element, Id, Kind, RenderElement, UnderlyingStorage,
            surface::{WaylandSurfaceRenderElement, WaylandSurfaceTexture},
            utils::RescaleRenderElement,
        },
        gles::{
            GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, GlesTexture,
            Uniform, UniformName, UniformType,
        },
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Physical, Rectangle, Scale, Size, Transform},
};

/// 图层着色器的水墨遮罩参数（见 `shader.frag`；全部为屏幕物理像素）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RevealUniforms {
    /// 晕染中心 x、y，半径，前沿宽度。
    pub reveal: [f32; 4],
    /// 模式（0 无 / 1 晕染 / 2 墨散），种子，墨散进度，缩放（物理像素/点）。
    pub mode: [f32; 4],
    /// 前沿积墨颜色（预乘）。
    pub ink: [f32; 4],
}

impl RevealUniforms {
    /// 不做遮罩。
    pub const NONE: RevealUniforms = RevealUniforms {
        reveal: [0.0; 4],
        mode: [0.0, 0.0, 0.0, 1.0],
        ink: [0.0; 4],
    };

    pub fn is_active(&self) -> bool {
        self.mode[0] > 0.5
    }

    fn uniforms(&self, dst: Rectangle<i32, Physical>) -> [Uniform<'static>; 4] {
        [
            Uniform::new("origin", (dst.loc.x as f32, dst.loc.y as f32)),
            Uniform::new("reveal", self.reveal),
            Uniform::new("reveal_mode", self.mode),
            Uniform::new("reveal_ink", self.ink),
        ]
    }
}

/// 2×3 仿射矩阵 `[a, b, c, d, e, f]`：x' = a·x + b·y + c，y' = d·x + e·y + f。
type Affine = [f64; 6];

fn affine_mul(m: Affine, n: Affine) -> Affine {
    [
        m[0] * n[0] + m[1] * n[3],
        m[0] * n[1] + m[1] * n[4],
        m[0] * n[2] + m[1] * n[5] + m[2],
        m[3] * n[0] + m[4] * n[3],
        m[3] * n[1] + m[4] * n[4],
        m[3] * n[2] + m[4] * n[5] + m[5],
    ]
}

fn affine_invert(m: Affine) -> Affine {
    let det = m[0] * m[4] - m[1] * m[3];
    if det.abs() < 1e-12 {
        return [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    }
    let (a, b, d, e) = (m[4] / det, -m[1] / det, -m[3] / det, m[0] / det);
    [a, b, -(a * m[2] + b * m[5]), d, e, -(d * m[2] + e * m[5])]
}

/// 着色器里的纹理坐标（`v_coords`，UV）→ 目标矩形内的像素坐标，按行给出（`local_x`、`local_y`）。
///
/// Smithay 的 GLES 渲染器把目标矩形内的像素坐标经过“源矩形裁剪（viewporter）、缓冲变换、
/// 上下颠倒”换算成 UV（`build_texture_mat`），这里按同样的步骤构造再求逆。圆角遮罩与晕染
/// 都在像素坐标中计算，所以不受这些换算影响。
fn uv_to_local(
    src: Rectangle<f64, Buffer>,
    dst: Size<i32, Physical>,
    texture: Size<i32, Buffer>,
    transform: Transform,
    y_inverted: bool,
) -> ([f32; 3], [f32; 3]) {
    let dss = transform.transform_size(src.size);
    let scale = |sx: f64, sy: f64| [sx, 0.0, 0.0, 0.0, sy, 0.0];
    let translate = |tx: f64, ty: f64| [1.0, 0.0, tx, 0.0, 1.0, ty];
    let (rotate, (tx, ty)) = match transform {
        Transform::Normal => ([1.0, 0.0, 0.0, 0.0, 1.0, 0.0], (0.0, 0.0)),
        Transform::_90 => ([0.0, 1.0, 0.0, -1.0, 0.0, 0.0], (0.0, dss.w)),
        Transform::_180 => ([-1.0, 0.0, 0.0, 0.0, -1.0, 0.0], (dss.w, dss.h)),
        Transform::_270 => ([0.0, -1.0, 0.0, 1.0, 0.0, 0.0], (dss.h, 0.0)),
        Transform::Flipped => ([-1.0, 0.0, 0.0, 0.0, 1.0, 0.0], (dss.w, 0.0)),
        Transform::Flipped90 => ([0.0, 1.0, 0.0, 1.0, 0.0, 0.0], (0.0, 0.0)),
        Transform::Flipped180 => ([1.0, 0.0, 0.0, 0.0, -1.0, 0.0], (0.0, dss.h)),
        Transform::Flipped270 => ([0.0, -1.0, 0.0, -1.0, 0.0, 0.0], (dss.h, dss.w)),
    };
    let mut m = scale(
        dss.w / f64::from(dst.w.max(1)),
        dss.h / f64::from(dst.h.max(1)),
    );
    m = affine_mul(rotate, m);
    m = affine_mul(translate(tx, ty), m);
    m = affine_mul(translate(src.loc.x, src.loc.y), m);
    m = affine_mul(
        scale(
            1.0 / f64::from(texture.w.max(1)),
            1.0 / f64::from(texture.h.max(1)),
        ),
        m,
    );
    if y_inverted {
        m = affine_mul([1.0, 0.0, 0.0, 0.0, -1.0, 1.0], m);
    }
    let inv = affine_invert(m);
    (
        [inv[0] as f32, inv[1] as f32, inv[2] as f32],
        [inv[3] as f32, inv[4] as f32, inv[5] as f32],
    )
}

/// 纹理铺满目标矩形（没有裁剪、没有变换）时的 UV → 像素坐标。
fn full_uv_to_local(dst: Size<i32, Physical>) -> ([f32; 3], [f32; 3]) {
    ([dst.w as f32, 0.0, 0.0], [0.0, dst.h as f32, 0.0])
}

/// 一个纹理图层：任意位置/大小、透明度、连续圆角、颜色乘子、可选的水墨遮罩。
pub struct LayerElement {
    pub id: Id,
    pub commit: CommitCounter,
    pub texture: GlesTexture,
    pub geometry: Rectangle<i32, Physical>,
    pub alpha: f32,
    pub radius: f32,
    /// 裁剪矩形（屏幕物理坐标）；`None` 表示整个图层。
    pub clip: Option<Rectangle<i32, Physical>>,
    pub color: [f32; 4],
    pub reveal: RevealUniforms,
    /// 整个图层完全不透明（纹理不透明、没有圆角/裁剪/遮罩、alpha = 1）：Smithay 可以跳过
    /// 被它完全挡住的元素和清屏。
    pub opaque: bool,
    pub program: GlesTexProgram,
}

/// 把屏幕坐标的裁剪矩形换算成相对 `dst` 的 [x, y, w, h]。
fn local_clip(clip: Option<Rectangle<i32, Physical>>, dst: Rectangle<i32, Physical>) -> [f32; 4] {
    match clip {
        Some(c) => [
            (c.loc.x - dst.loc.x) as f32,
            (c.loc.y - dst.loc.y) as f32,
            c.size.w as f32,
            c.size.h as f32,
        ],
        None => [0.0, 0.0, dst.size.w as f32, dst.size.h as f32],
    }
}

impl Element for LayerElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.texture.size()).to_f64()
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        if self.opaque && self.alpha >= 1.0 && !self.reveal.is_active() {
            OpaqueRegions::from_slice(&[Rectangle::from_size(self.geometry.size)])
        } else {
            OpaqueRegions::default()
        }
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for LayerElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), GlesError> {
        let [origin, reveal, mode, ink] = self.reveal.uniforms(dst);
        let (local_x, local_y) = full_uv_to_local(dst.size);
        let uniforms = [
            Uniform::new("size", (dst.size.w as f32, dst.size.h as f32)),
            Uniform::new("local_x", local_x),
            Uniform::new("local_y", local_y),
            Uniform::new("clip", local_clip(self.clip, dst)),
            Uniform::new("radius", self.radius),
            Uniform::new("color_mul", self.color),
            origin,
            reveal,
            mode,
            ink,
        ];
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            &[],
            Transform::Normal,
            self.alpha,
            Some(&self.program),
            &uniforms,
        )
    }
}

/// 表面的内容。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SurfaceContent {
    /// 普通缓冲（shm / dmabuf）：纹理尺寸、是否上下颠倒。
    Texture {
        size: Size<i32, Buffer>,
        y_inverted: bool,
    },
    /// 单像素缓冲（wp_single_pixel_buffer_v1，通常配合 wp_viewporter 拉伸）：预乘颜色。
    Solid([f32; 4]),
}

impl SurfaceContent {
    pub fn of(surface: &WaylandSurfaceRenderElement<GlesRenderer>) -> Self {
        match surface.texture() {
            WaylandSurfaceTexture::Texture(texture) => SurfaceContent::Texture {
                size: texture.size(),
                y_inverted: texture.is_y_inverted(),
            },
            WaylandSurfaceTexture::SolidColor(color) => SurfaceContent::Solid(color.components()),
        }
    }
}

/// 应用窗口的一个表面（根表面、子表面或弹出菜单）：给默认着色器换成带圆角（和可选水墨
/// 遮罩）的版本。
///
/// 圆角与裁剪按**整个窗口**计算：`clip` 是窗口的可见矩形（屏幕物理坐标），同一窗口的所有
/// 表面共用。着色器把每个像素换算到 `clip` 中判断，因此贴着窗口边缘的子表面带上窗口的圆角，
/// 窗口内部的子表面完全不受影响，超出窗口的部分被裁掉。
pub struct RoundedSurface {
    /// 表面（已随窗口整体缩放到卡片大小）。
    pub inner: RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    pub content: SurfaceContent,
    /// 1×1 白色纹理：纯色表面用它乘以颜色绘制。
    pub white: GlesTexture,
    pub radius: f32,
    pub clip: Rectangle<i32, Physical>,
    pub reveal: RevealUniforms,
    /// 遮罩在变化（或刚刚结束）：整个表面都要重画，客户端没有提交新画面也一样。
    pub force_damage: bool,
    pub program: GlesTexProgram,
}

impl Element for RoundedSurface {
    fn id(&self) -> &Id {
        self.inner.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }
    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        if self.force_damage {
            return DamageSet::from_slice(&[Rectangle::from_size(self.geometry(scale).size)]);
        }
        self.inner.damage_since(scale, commit)
    }
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // 圆角处是透明的，不能声明为不透明。
        OpaqueRegions::default()
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for RoundedSurface {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), GlesError> {
        let [origin, reveal, mode, ink] = self.reveal.uniforms(dst);
        let ((local_x, local_y), color) = match self.content {
            SurfaceContent::Texture { size, y_inverted } => (
                uv_to_local(src, dst.size, size, self.inner.transform(), y_inverted),
                [1.0; 4],
            ),
            SurfaceContent::Solid(color) => (full_uv_to_local(dst.size), color),
        };
        let uniforms = vec![
            Uniform::new("size", (dst.size.w as f32, dst.size.h as f32)),
            Uniform::new("local_x", local_x),
            Uniform::new("local_y", local_y),
            Uniform::new("clip", local_clip(Some(self.clip), dst)),
            Uniform::new("radius", self.radius),
            Uniform::new("color_mul", color),
            origin,
            reveal,
            mode,
            ink,
        ];
        if let SurfaceContent::Solid(_) = self.content {
            // 单像素缓冲：Smithay 默认用 draw_solid 画（不经过纹理着色器，没有圆角），
            // 这里改用白色纹理 × 颜色，走同一个着色器。
            return frame.render_texture_from_to(
                &self.white,
                Rectangle::from_size(self.white.size()).to_f64(),
                dst,
                damage,
                &[],
                Transform::Normal,
                self.inner.alpha(),
                Some(&self.program),
                &uniforms,
            );
        }
        frame.override_default_tex_program(self.program.clone(), uniforms);
        let result = RenderElement::<GlesRenderer>::draw(
            &self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        );
        frame.clear_tex_program_override();
        result
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        // 我们改变了绘制方式，不能直接扫描输出客户端缓冲。
        None
    }
}

/// 水墨印记（点墨 / 笔迹段）：在一个小矩形里用像素着色器 `ink_fx.frag` 现算墨色，
/// 不需要纹理。参数全部为相对元素左上角的物理像素。
pub struct InkElement {
    pub id: Id,
    pub commit: CommitCounter,
    pub geometry: Rectangle<i32, Physical>,
    /// 起点 x、y，终点 x、y。
    pub seg: [f32; 4],
    /// 起点半径、终点半径、起点处的笔画累计长度、缩放（物理像素/点）。
    pub shape: [f32; 4],
    /// 种类（0 墨滴 / 1 笔迹）、浓度、干湿、种子。
    pub params: [f32; 4],
    /// 墨色（预乘）。
    pub ink: [f32; 4],
    /// 笔段起点、终点是否画圆头。
    pub caps: [f32; 2],
    pub program: GlesPixelProgram,
}

impl Element for InkElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(Size::<i32, Buffer>::from((
            self.geometry.size.w,
            self.geometry.size.h,
        )))
        .to_f64()
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for InkElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), GlesError> {
        // `size` 统一设为目标矩形的物理像素尺寸，着色器里 v_coords × size 即局部像素坐标。
        let size = Size::<i32, Buffer>::from((dst.size.w, dst.size.h));
        frame.render_pixel_shader_to(
            &self.program,
            src,
            dst,
            size,
            Some(damage),
            1.0,
            &[
                Uniform::new("seg", self.seg),
                Uniform::new("shape", self.shape),
                Uniform::new("params", self.params),
                Uniform::new("ink", self.ink),
                Uniform::new("caps", self.caps),
            ],
        )
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

smithay::backend::renderer::element::render_elements! {
    pub ShellElement<=GlesRenderer>;
    Layer=LayerElement,
    Surface=RoundedSurface,
    Ink=InkElement,
}

/// 编译图层着色器。
pub fn compile_program(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        include_str!("shader.frag"),
        &[
            UniformName::new("size", UniformType::_2f),
            UniformName::new("local_x", UniformType::_3f),
            UniformName::new("local_y", UniformType::_3f),
            UniformName::new("clip", UniformType::_4f),
            UniformName::new("radius", UniformType::_1f),
            UniformName::new("color_mul", UniformType::_4f),
            UniformName::new("origin", UniformType::_2f),
            UniformName::new("reveal", UniformType::_4f),
            UniformName::new("reveal_mode", UniformType::_4f),
            UniformName::new("reveal_ink", UniformType::_4f),
        ],
    )
}

/// 编译水墨印记着色器。
pub fn compile_ink_program(renderer: &mut GlesRenderer) -> Result<GlesPixelProgram, GlesError> {
    renderer.compile_custom_pixel_shader(
        include_str!("ink_fx.frag"),
        &[
            UniformName::new("seg", UniformType::_4f),
            UniformName::new("shape", UniformType::_4f),
            UniformName::new("params", UniformType::_4f),
            UniformName::new("ink", UniformType::_4f),
            UniformName::new("caps", UniformType::_2f),
        ],
    )
}

#[allow(dead_code)]
fn _assert_renderer<R: Renderer>() {}

#[cfg(test)]
mod tests {
    use super::*;

    /// `shader.frag` 中 `corner_mask` 的 CPU 版（同一公式）：`p` 相对裁剪矩形左上角。
    fn corner_mask(p: (f32, f32), clip: [f32; 4], radius: f32) -> f32 {
        let half = (clip[2] * 0.5, clip[3] * 0.5);
        let a = (radius * 1.28).min(half.0.min(half.1));
        if a < 0.5 {
            let inside = |v: f32, max: f32| f32::from(u8::from(v >= 0.0 && v <= max));
            return inside(p.0, clip[2]) * inside(p.1, clip[3]);
        }
        let q = (
            (p.0 - half.0).abs() - (half.0 - a),
            (p.1 - half.1).abs() - (half.1 - a),
        );
        let d = if q.0 > 0.0 && q.1 > 0.0 {
            let n = ((q.0 / a).powi(2), (q.1 / a).powi(2));
            ((n.0 * n.0 + n.1 * n.1).sqrt().sqrt() - 1.0) * a
        } else {
            q.0.max(q.1) - a
        };
        (0.5 - d).clamp(0.0, 1.0)
    }

    /// 着色器对目标矩形 `dst` 中局部坐标 `local` 处像素算出的圆角遮罩（与 `main()` 的换算一致）。
    fn mask_at(
        dst: Rectangle<i32, Physical>,
        clip: Rectangle<i32, Physical>,
        radius: f32,
        local: (f32, f32),
    ) -> f32 {
        let c = local_clip(Some(clip), dst);
        corner_mask((local.0 - c[0], local.1 - c[1]), c, radius)
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn apply((x, y): ([f32; 3], [f32; 3]), u: f32, v: f32) -> (f32, f32) {
        (x[0] * u + x[1] * v + x[2], y[0] * u + y[1] * v + y[2])
    }

    fn close((a, b): (f32, f32), (c, d): (f32, f32)) -> bool {
        (a - c).abs() < 1e-3 && (b - d).abs() < 1e-3
    }

    #[test]
    fn texture_coords_map_back_to_target_pixels() {
        let dst = Size::<i32, Physical>::from((400, 200));
        let tex = Size::<i32, Buffer>::from((200, 100));
        let full = Rectangle::from_size(tex).to_f64();
        // 铺满：与图层元素的映射相同。
        let m = uv_to_local(full, dst, tex, Transform::Normal, false);
        assert_eq!(m, full_uv_to_local(dst));
        // viewporter 裁剪：源矩形 (10, 20, 100, 50) 的四角对应目标矩形的四角。
        let crop = Rectangle::new((10.0, 20.0).into(), (100.0, 50.0).into());
        let m = uv_to_local(crop, dst, tex, Transform::Normal, false);
        assert!(close(apply(m, 0.05, 0.2), (0.0, 0.0)));
        assert!(close(apply(m, 0.55, 0.7), (400.0, 200.0)));
        // 上下颠倒的缓冲（dmabuf 的 Y_INVERT）。
        let m = uv_to_local(crop, dst, tex, Transform::Normal, true);
        assert!(close(apply(m, 0.05, 0.8), (0.0, 0.0)));
        // 旋转 90° 的缓冲（100×50，显示为 50×100）：缓冲左下角是目标的左上角。
        let tex = Size::<i32, Buffer>::from((100, 50));
        let dst = Size::<i32, Physical>::from((50, 100));
        let m = uv_to_local(
            Rectangle::from_size(tex).to_f64(),
            dst,
            tex,
            Transform::_90,
            false,
        );
        assert!(close(apply(m, 0.0, 1.0), (0.0, 0.0)));
        assert!(close(apply(m, 0.0, 0.0), (50.0, 0.0)));
        assert!(close(apply(m, 1.0, 0.0), (50.0, 100.0)));
    }

    #[test]
    fn subsurface_corners_follow_the_window() {
        let window = rect(100, 50, 1000, 600);
        let r = 40.0;
        // 贴着窗口顶边的工具栏子表面：左上角被窗口的圆角裁掉，中间完整。
        let toolbar = rect(100, 50, 1000, 64);
        assert_eq!(mask_at(toolbar, window, r, (0.5, 0.5)), 0.0);
        assert_eq!(mask_at(toolbar, window, r, (999.5, 0.5)), 0.0);
        assert_eq!(mask_at(toolbar, window, r, (500.0, 32.0)), 1.0);
        // 窗口内部的子表面：自己的四个角都不受影响。
        let inner = rect(400, 250, 300, 200);
        for corner in [(0.5, 0.5), (299.5, 0.5), (0.5, 199.5), (299.5, 199.5)] {
            assert_eq!(mask_at(inner, window, r, corner), 1.0, "{corner:?}");
        }
        // 超出窗口的部分被裁掉。
        let overflow = rect(1050, 300, 100, 100);
        assert_eq!(mask_at(overflow, window, r, (75.0, 50.0)), 0.0);
        assert_eq!(mask_at(overflow, window, r, (25.0, 50.0)), 1.0);
        // 没有圆角时只裁剪。
        assert_eq!(mask_at(toolbar, window, 0.0, (0.5, 0.5)), 1.0);
        assert_eq!(mask_at(overflow, window, 0.0, (75.0, 50.0)), 0.0);
    }
}
