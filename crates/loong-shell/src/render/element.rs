//! 自定义渲染元素。

use smithay::{
    backend::renderer::{
        Renderer, Texture,
        element::{
            Element, Id, Kind, RenderElement, UnderlyingStorage,
            surface::WaylandSurfaceRenderElement,
        },
        gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Physical, Rectangle, Scale, Transform},
};

/// 一个纹理图层：任意位置/大小、透明度、连续圆角、颜色乘子。
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
        let uniforms = [
            Uniform::new("size", (dst.size.w as f32, dst.size.h as f32)),
            Uniform::new("clip", local_clip(self.clip, dst)),
            Uniform::new("radius", self.radius),
            Uniform::new("color_mul", self.color),
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

/// 应用窗口表面：给默认着色器换成带圆角的版本。
pub struct RoundedSurface {
    pub inner: WaylandSurfaceRenderElement<GlesRenderer>,
    pub radius: f32,
    pub clip: Option<Rectangle<i32, Physical>>,
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
        frame.override_default_tex_program(
            self.program.clone(),
            vec![
                Uniform::new("size", (dst.size.w as f32, dst.size.h as f32)),
                Uniform::new("clip", local_clip(self.clip, dst)),
                Uniform::new("radius", self.radius),
                Uniform::new("color_mul", [1.0f32, 1.0, 1.0, 1.0]),
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

smithay::backend::renderer::element::render_elements! {
    pub ShellElement<=GlesRenderer>;
    Layer=LayerElement,
    Surface=RoundedSurface,
}

/// 编译图层着色器。
pub fn compile_program(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    use smithay::backend::renderer::gles::{UniformName, UniformType};
    renderer.compile_custom_texture_shader(
        include_str!("shader.frag"),
        &[
            UniformName::new("size", UniformType::_2f),
            UniformName::new("clip", UniformType::_4f),
            UniformName::new("radius", UniformType::_1f),
            UniformName::new("color_mul", UniformType::_4f),
        ],
    )
}

#[allow(dead_code)]
fn _assert_renderer<R: Renderer>() {}
