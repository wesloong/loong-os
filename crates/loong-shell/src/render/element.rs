//! 自定义渲染元素。

use smithay::{
    backend::renderer::{
        Renderer, Texture,
        element::{
            Element, Id, Kind, RenderElement, UnderlyingStorage,
            surface::WaylandSurfaceRenderElement,
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
        let uniforms = [
            Uniform::new("size", (dst.size.w as f32, dst.size.h as f32)),
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

/// 应用窗口表面：给默认着色器换成带圆角（和可选水墨遮罩）的版本。
pub struct RoundedSurface {
    pub inner: WaylandSurfaceRenderElement<GlesRenderer>,
    pub radius: f32,
    pub clip: Option<Rectangle<i32, Physical>>,
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
        frame.override_default_tex_program(
            self.program.clone(),
            vec![
                Uniform::new("size", (dst.size.w as f32, dst.size.h as f32)),
                Uniform::new("clip", local_clip(self.clip, dst)),
                Uniform::new("radius", self.radius),
                Uniform::new("color_mul", [1.0f32, 1.0, 1.0, 1.0]),
                origin,
                reveal,
                mode,
                ink,
            ],
        );
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
