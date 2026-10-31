#!/usr/bin/env node
// LoongKit 页面截图：用宿主机的 Playwright + Chromium 渲染 webrt/demo 与 webrt/apps 下的页面
// （设置用 ?mock 演示数据，见 webrt/apps/settings/mock.js），
// 输出亮色/暗色、1x（1280×800 逻辑点，QEMU）与 2x（2560×1600，小米平板 5）截图，
// 供视觉评审与文档使用。
//
//   node tools/web-shot.mjs                      # 默认画面 → docs/images/web/
//   node tools/web-shot.mjs --out /tmp/shots     # 输出到别处
//   node tools/web-shot.mjs --only settings      # 只拍名字含 settings 的画面
//   node tools/web-shot.mjs --scales 1,2         # 每个画面都拍 1x 与 2x（覆盖默认）
//   node tools/web-shot.mjs --themes dark        # 只拍暗色
//
// 输出格式：1x 与按压特写为 PNG（逐像素可比）；2x 整屏为 JPEG（质量 92）。宣纸纹理是
// 噪声，PNG 压不动（每张 2–3 MB），JPEG 在这个质量下肉眼无损，约 0.4 MB。
//
// 字体：Chromium 走 fontconfig。脚本会生成临时 fonts.conf，额外收录开发 chroot 中的
// 字体（Noto Sans CJK SC、霞鹜文楷），与设备上的字体一致；LoongKit 的 CSS 只用 local()。
// 若 chroot 中还没有霞鹜文楷，可用 LOONG_WENKAI_CSS=/path/to/lxgwwenkaiscreen.css
// 指向一份 Web 字体样式表（仅截图时注入，不进入产品 CSS）。

import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

// 用 CommonJS 解析，以便找到全局安装（NODE_PATH）的 playwright。
const { chromium } = createRequire(import.meta.url)('playwright');

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');

function arg(name, def) {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : def;
}

const outDir = resolve(arg('out', join(repo, 'docs/images/web')));
const only = arg('only', '');
const scalesArg = arg('scales', '');
const themes = arg('themes', 'light,dark').split(',');

// 每个画面：名字、页面、默认缩放、可选的准备动作（在截图前执行，可返回 clip）。
const SETTINGS = 'webrt/apps/settings/index.html?mock=1';
const shots = [
  { name: 'demo', page: 'webrt/demo/index.html', scales: [1, 2] },
  { name: 'settings-wifi', page: `${SETTINGS}#wifi`, scales: [2] },
  { name: 'settings-display', page: `${SETTINGS}#display`, scales: [1, 2] },
  { name: 'settings-battery', page: `${SETTINGS}#battery`, scales: [2] },
  { name: 'settings-about', page: `${SETTINGS}#general/about`, scales: [2] },
  {
    // 加入加密网络：点网络行 → 密码 sheet，输入后截图。
    name: 'settings-wifi-password',
    page: `${SETTINGS}#wifi`,
    scales: [2],
    async before(page) {
      await page.locator('#wifi-nets .net-row', { hasText: '茶寮-5G' }).click();
      await page.locator('#pw-input').fill('loong1234');
      await page.waitForTimeout(900);
    },
  },
  // 点墨：按住按钮 / 列表行，墨点晕开途中截图（只截附近）。
  pressShot('inkdrop-button', 'webrt/demo/index.html', '[data-shot="press"]', 0.38, 0.55, 60, 40),
  pressShot('inkdrop-row', `${SETTINGS}#display`, '.sidebar a[href="#battery"]', 0.42, 0.5, 20, 60),
];

function pressShot(name, pageUrl, selector, fx, fy, padX, padY) {
  return {
    name,
    page: pageUrl,
    scales: [2],
    crop: true,
    async before(page) {
      const box = await page.locator(selector).first().boundingBox();
      await page.mouse.move(box.x + box.width * fx, box.y + box.height * fy);
      await page.mouse.down();
      await page.waitForTimeout(180);
      return { clip: { x: box.x - padX, y: box.y - padY, width: box.width + 2 * padX, height: box.height + 2 * padY } };
    },
  };
}

// —— 字体：生成 fonts.conf，加入 chroot 字体目录 ——
function mainRepoRoot() {
  try {
    const common = execFileSync('git', ['-C', repo, 'rev-parse', '--path-format=absolute', '--git-common-dir'], {
      encoding: 'utf8',
    }).trim();
    return dirname(common);
  } catch {
    return repo;
  }
}

const fontDirs = [
  ...(process.env.LOONG_FONT_DIRS || '').split(':').filter(Boolean),
  join(repo, 'build/chroot/usr/share/fonts'),
  join(mainRepoRoot(), 'build/chroot/usr/share/fonts'),
].filter((d, i, a) => d && existsSync(d) && a.indexOf(d) === i);

const tmp = mkdtempSync(join(tmpdir(), 'loong-web-shot-'));
const fontsConf = join(tmp, 'fonts.conf');
writeFileSync(
  fontsConf,
  `<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "fonts.dtd">
<fontconfig>
  <include ignore_missing="yes">/etc/fonts/fonts.conf</include>
${fontDirs.map((d) => `  <dir>${d}</dir>`).join('\n')}
  <cachedir>${join(tmp, 'fc-cache')}</cachedir>
</fontconfig>
`,
);

const wenkaiCss = process.env.LOONG_WENKAI_CSS;

mkdirSync(outDir, { recursive: true });
const browser = await chromium.launch({ env: { ...process.env, FONTCONFIG_FILE: fontsConf } });

let count = 0;
for (const shot of shots) {
  if (only && !shot.name.includes(only)) continue;
  const scales = shot.crop || !scalesArg ? shot.scales : scalesArg.split(',').map(Number);
  for (const theme of themes) {
    for (const scale of scales) {
      const ctx = await browser.newContext({
        viewport: { width: 1280, height: 800 },
        deviceScaleFactor: scale,
        colorScheme: theme,
      });
      const page = await ctx.newPage();
      page.on('pageerror', (e) => console.error(`[${shot.name}] page error:`, e.message));
      page.on('console', (m) => m.type() === 'error' && console.error(`[${shot.name}] console:`, m.text()));
      const [pathQuery, hash] = shot.page.split('#');
      const [path, query] = pathQuery.split('?');
      await page.goto(pathToFileURL(join(repo, path)).href + (query ? `?${query}` : '') + (hash ? `#${hash}` : ''));
      if (wenkaiCss) await page.addStyleTag({ url: pathToFileURL(resolve(wenkaiCss)).href });
      await page.evaluate(() => document.fonts.ready);
      // 等入场动画（晕染开启、弹簧小球）停稳。
      await page.waitForTimeout(1300);
      await page.evaluate(() => document.fonts.ready);
      const opts = (shot.before && (await shot.before(page))) || {};
      const suffix = scale === 1 ? '' : `@${scale}x`;
      const jpeg = scale > 1 && !shot.crop;
      const file = join(outDir, `${shot.name}-${theme}${suffix}.${jpeg ? 'jpg' : 'png'}`);
      await page.screenshot({ path: file, ...(jpeg ? { type: 'jpeg', quality: 92 } : {}), ...opts });
      console.log(file);
      count++;
      await ctx.close();
    }
  }
}
await browser.close();
console.log(`${count} 张截图；字体目录：${fontDirs.join(', ') || '（仅系统字体）'}`);
