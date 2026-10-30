/*
 * LoongKit「墨韵」交互：点墨、滑块与分段控件、外观切换、落印。无依赖，约 3 KB。
 *
 *   <link rel="stylesheet" href="loongkit.css">
 *   <script src="loongkit.js" defer></script>
 *
 * 点墨：在 .loong-button、a/button.loong-row、分段控件以及任何带 data-ink 的元素上按下时，
 * 指下落一滴墨，半径随 SNAPPY 弹簧晕开；抬起后随 SMOOTH 弹簧淡入纸中。只动 transform 与
 * opacity（合成线程），墨点外形是一张静态 mask，栅格化一次。“减弱动态效果”时不落墨。
 */
(() => {
  'use strict';

  const root = document.documentElement;
  const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)');
  const INK_TARGETS = '[data-ink], .loong-button, a.loong-row, button.loong-row, .loong-segmented label';
  /** 墨点半径（px）：约等于指下元素的高度——一滴墨，而不是整块泼墨。 */
  const MIN_DROP_RADIUS = 28;
  const MAX_DROP_RADIUS = 96;

  /** 读取 springs.css 中的弹簧缓动；缺失或浏览器不支持 linear() 时退化为 ease-out。 */
  function spring(name) {
    const css = getComputedStyle(root);
    const duration = parseFloat(css.getPropertyValue(`--loong-spring-${name}-duration`)) || 400;
    let easing = css.getPropertyValue(`--loong-spring-${name}-easing`).trim() || 'ease-out';
    if (!CSS.supports('transition-timing-function', easing)) easing = 'ease-out';
    return { duration, easing };
  }

  // —— 点墨 ——

  function inkDrop(host, clientX, clientY) {
    if (reduceMotion.matches) return null;
    const r = host.getBoundingClientRect();
    const x = clientX - r.left;
    const y = clientY - r.top;
    // 小元素（开关、分段）晕满为止；大元素只晕开一滴（约为元素高度），被元素边界裁切。
    const far = Math.hypot(Math.max(x, r.width - x), Math.max(y, r.height - y));
    const drop0 = Math.max(MIN_DROP_RADIUS, Math.min(0.95 * Math.max(r.height, 44), MAX_DROP_RADIUS));
    const radius = Math.min(far * 1.1, drop0);
    const drop = document.createElement('span');
    drop.className = 'loong-inkdrop';
    drop.setAttribute('aria-hidden', 'true');
    drop.style.cssText = `left:${x - radius}px;top:${y - radius}px;width:${2 * radius}px;height:${2 * radius}px`;
    // 每滴墨转一个随机角度、略扁一点，毛边各不相同。
    const rot = Math.round(Math.random() * 360);
    const sx = (0.92 + Math.random() * 0.16).toFixed(3);
    drop.dataset.base = `rotate(${rot}deg) scale(${sx}, 1)`;
    host.prepend(drop);
    const s = spring('snappy');
    drop.animate(
      [
        { transform: `${drop.dataset.base} scale(0.08)`, opacity: 0.6 },
        { transform: `${drop.dataset.base} scale(1)`, opacity: 1 },
      ],
      { duration: s.duration, easing: s.easing, fill: 'forwards' },
    );
    return drop;
  }

  /** 收墨：墨继续往纸里洇一点，同时淡去。 */
  function releaseDrop(drop) {
    if (!drop || drop.dataset.released) return;
    drop.dataset.released = '1';
    const s = spring('smooth');
    const current = getComputedStyle(drop).transform;
    const a = drop.animate(
      [
        { transform: current === 'none' ? `${drop.dataset.base} scale(1)` : current, opacity: 1 },
        { transform: `${drop.dataset.base} scale(1.12)`, opacity: 0 },
      ],
      { duration: s.duration, easing: s.easing, fill: 'forwards' },
    );
    a.onfinish = a.oncancel = () => drop.remove();
  }

  document.addEventListener(
    'pointerdown',
    (e) => {
      if (e.button !== 0) return;
      const host = e.target.closest(INK_TARGETS);
      if (!host || host.matches(':disabled, [aria-disabled="true"]')) return;
      const drop = inkDrop(host, e.clientX, e.clientY);
      if (!drop) return;
      const up = () => {
        releaseDrop(drop);
        window.removeEventListener('pointerup', up);
        window.removeEventListener('pointercancel', up);
      };
      window.addEventListener('pointerup', up);
      window.addEventListener('pointercancel', up);
    },
    { passive: true },
  );

  // —— 滑块：--loong-value = 0..1 ——

  function syncSlider(input) {
    const min = Number(input.min || 0);
    const max = Number(input.max || 100);
    const v = (Number(input.value) - min) / (max - min || 1);
    input.parentElement.style.setProperty('--loong-value', Math.min(1, Math.max(0, v)).toFixed(4));
  }

  // —— 分段控件：--loong-index / --loong-count ——

  function syncSegmented(group) {
    const inputs = [...group.querySelectorAll('input[type="radio"]')];
    group.style.setProperty('--loong-count', inputs.length);
    group.style.setProperty('--loong-index', Math.max(0, inputs.findIndex((i) => i.checked)));
    if (!group.querySelector(':scope > i')) group.append(document.createElement('i'));
  }

  document.addEventListener('input', (e) => {
    if (e.target.matches('.loong-slider input[type="range"]')) syncSlider(e.target);
  });
  document.addEventListener('change', (e) => {
    const group = e.target.closest('.loong-segmented');
    if (group) syncSegmented(group);
  });

  // —— 外观：'light' | 'dark' | 'auto' ——

  function setTheme(mode) {
    const apply = () => {
      if (mode === 'light' || mode === 'dark') root.dataset.theme = mode;
      else delete root.dataset.theme;
    };
    // 支持视图过渡时，用 SMOOTH 弹簧交叉晕染；否则直接切换。
    if (document.startViewTransition && !reduceMotion.matches) document.startViewTransition(apply);
    else apply();
  }

  function currentTheme() {
    return root.dataset.theme || (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');
  }

  // —— 落印：确认类操作，印章带着 BOUNCY 弹簧按到纸上 ——

  function stamp(el) {
    el.classList.remove('is-stamped');
    void el.offsetWidth; // 重新触发动画
    el.classList.add('is-stamped');
  }

  // —— 毛笔滤镜：给符号（.loong-symbol）用的 SVG 滤镜，注入一次 ——

  const FILTERS = `<svg xmlns="http://www.w3.org/2000/svg" aria-hidden="true" style="position:absolute;width:0;height:0;overflow:hidden">
<filter id="loong-brush" x="-20%" y="-20%" width="140%" height="140%">
  <feTurbulence type="fractalNoise" baseFrequency="0.9" numOctaves="2" seed="3" result="n"/>
  <feDisplacementMap in="SourceGraphic" in2="n" scale="1.1" xChannelSelector="R" yChannelSelector="G" result="d"/>
  <feGaussianBlur in="d" stdDeviation="0.6" result="bleed"/>
  <feComponentTransfer in="bleed" result="halo"><feFuncA type="linear" slope="0.45"/></feComponentTransfer>
  <feMerge><feMergeNode in="halo"/><feMergeNode in="d"/></feMerge>
</filter>
</svg>`;

  function init() {
    if (!document.getElementById('loong-brush')) document.body.insertAdjacentHTML('afterbegin', FILTERS);
    root.classList.add('loong-ready'); // 滤镜已就位，CSS 才去引用 #loong-brush
    document.querySelectorAll('.loong-slider input[type="range"]').forEach(syncSlider);
    document.querySelectorAll('.loong-segmented').forEach(syncSegmented);
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();

  window.LoongKit = { inkDrop, releaseDrop, setTheme, currentTheme, stamp, spring, syncSlider, syncSegmented };
})();
