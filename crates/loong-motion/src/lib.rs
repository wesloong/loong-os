//! # loong-motion
//!
//! Loong OS 的物理动画引擎。系统里所有动画（合成器里的窗口/图标/面板，以及 Web 侧的
//! LoongKit）都基于这里的模型，保证手感一致：
//!
//! - [`Spring`] / [`SpringAnimation`]：阻尼谐振子的**解析解**。参数采用 `response`（周期感）
//!   与 `damping_fraction`（阻尼比）。动画可随时改目标（[`SpringAnimation::retarget`]），
//!   位置和速度保持连续——手势打断动画时不会“跳”。
//! - [`Decay`] / [`DecayAnimation`]：惯性滚动（指数衰减），[`Decay::project`] 预测最终落点。
//! - [`rubber_band`]：越界时的橡皮筋阻尼。
//! - [`VelocityTracker`]：根据触点轨迹估算松手速度。
//! - [`snap`]：结合速度预测，决定手势结束后应吸附到哪个位置 / 哪一页。
//! - [`css`]：把弹簧曲线转换为 CSS `linear()` 缓动，供 Web 应用使用。
//!
//! 时间统一用 `f64` 秒表示（通常来自显示器 vblank 时间戳），因此动画结果与刷新率
//! （60Hz / 120Hz）无关。本 crate 不依赖任何第三方库。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod css;
mod decay;
mod rubber;
pub mod snap;
mod spring;
mod velocity;

pub use decay::{Decay, DecayAnimation};
pub use rubber::{RUBBER_BAND_COEFFICIENT, rubber_band, rubber_band_clamp, rubber_band_inverse};
pub use spring::{Spring, SpringAnimation};
pub use velocity::VelocityTracker;

/// 可做动画的值：标量 `f64`，或定长数组 `[f64; N]`（例如位置、缩放、颜色）。
///
/// 每个分量独立按同一物理模型运动。
pub trait Animatable: Copy {
    /// 分量个数。
    const DIM: usize;
    /// 第 `i` 个分量。
    fn get(&self, i: usize) -> f64;
    /// 由每个分量构造值。
    fn from_fn(f: impl FnMut(usize) -> f64) -> Self;
}

impl Animatable for f64 {
    const DIM: usize = 1;
    #[inline]
    fn get(&self, _i: usize) -> f64 {
        *self
    }
    #[inline]
    fn from_fn(mut f: impl FnMut(usize) -> f64) -> Self {
        f(0)
    }
}

impl<const N: usize> Animatable for [f64; N] {
    const DIM: usize = N;
    #[inline]
    fn get(&self, i: usize) -> f64 {
        self[i]
    }
    #[inline]
    fn from_fn(f: impl FnMut(usize) -> f64) -> Self {
        std::array::from_fn(f)
    }
}

/// 两个值逐分量相减。
#[inline]
pub(crate) fn sub<V: Animatable>(a: V, b: V) -> V {
    V::from_fn(|i| a.get(i) - b.get(i))
}

/// 值的零元素。
#[inline]
pub(crate) fn zero<V: Animatable>() -> V {
    V::from_fn(|_| 0.0)
}
