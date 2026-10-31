/*
 * 设置（系统应用）。系统接口来自 loong-webapp 注入的 window.loong（docs/api/loong-js-bridge.md）；
 * 普通浏览器里没有 window.loong，或地址带 ?mock 时，改用 mock.js 的演示数据。
 *
 * 安全：Wi‑Fi 名称（SSID）由附近的任何人决定，一律用 textContent 写入页面，从不拼接 HTML。
 */
(() => {
  'use strict';

  const params = new URLSearchParams(location.search);
  const MOCK = params.has('mock') || !window.loong;
  const loong = MOCK ? window.LoongMock.create(params) : window.loong;
  const { backlight, power, wifi } = loong.system;
  const root = document.documentElement;
  const $ = (id) => document.getElementById(id);
  const SVGNS = 'http://www.w3.org/2000/svg';

  const state = {
    wifi: null,
    nets: [],
    netsLoaded: false,
    connecting: null, // 正在连接的 SSID
    toggling: false,
    power: null,
    backlight: null,
    theme: null,
    info: null,
    serviceError: null,
  };

  // ───────────────────────── DOM 小工具 ─────────────────────────

  /** h('div', { class: 'x', onclick }, '文字', child)：文字一律作为文本节点插入。 */
  function h(tag, props, ...children) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(props || {})) {
      if (v == null || v === false) continue;
      if (k === 'class') el.className = v;
      else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
      else el.setAttribute(k, v === true ? '' : v);
    }
    for (const c of children.flat()) if (c != null && c !== false) el.append(c instanceof Node ? c : String(c));
    return el;
  }

  function symbol(id, cls = '') {
    const svg = document.createElementNS(SVGNS, 'svg');
    svg.setAttribute('class', `loong-symbol ${cls}`.trim());
    svg.setAttribute('aria-hidden', 'true');
    const use = document.createElementNS(SVGNS, 'use');
    use.setAttribute('href', `#${id}`);
    svg.append(use);
    return svg;
  }

  function enso() {
    return symbol('i-enso', 'enso spin');
  }

  // ── 墨画 Wi‑Fi：一点三弧，与状态栏的 ink::wifi 同形（每道弧尖入、中段饱满、出锋，成新月形） ──

  const WIFI_ARCS = (() => {
    const H = 20;
    const cx = 12;
    const cy = H * 0.84;
    const line = H * 0.11;
    const half = 0.8;
    const arcs = [];
    for (let k = 0; k < 3; k++) {
      const r = H * (0.3 + 0.235 * k);
      const w = line * (1.15 + 0.06 * k);
      const outer = [];
      const inner = [];
      const N = 20;
      for (let i = 0; i <= N; i++) {
        const u = i / N;
        const a = -Math.PI / 2 - half + 2 * half * u;
        const t = (Math.pow(Math.sin(Math.PI * u), 0.6) * w) / 2 + 0.12;
        outer.push([cx + (r + t) * Math.cos(a), cy + (r + t) * Math.sin(a)]);
        inner.push([cx + (r - t) * Math.cos(a), cy + (r - t) * Math.sin(a)]);
      }
      const pts = outer.concat(inner.reverse());
      arcs.push(`M${pts.map(([x, y]) => `${x.toFixed(2)} ${y.toFixed(2)}`).join('L')}Z`);
    }
    return { arcs, dot: [cx, cy - line * 0.2, line * 0.85] };
  })();

  function wifiLevel(strength) {
    return strength >= 60 ? 3 : strength >= 30 ? 2 : strength > 0 ? 1 : 0;
  }

  function inkWifi(level) {
    const svg = document.createElementNS(SVGNS, 'svg');
    svg.setAttribute('viewBox', '0 0 24 20');
    svg.setAttribute('class', 'ink-wifi');
    svg.setAttribute('aria-hidden', 'true');
    const [x, y, r] = WIFI_ARCS.dot;
    const dot = document.createElementNS(SVGNS, 'circle');
    dot.setAttribute('cx', x);
    dot.setAttribute('cy', y);
    dot.setAttribute('r', r);
    dot.setAttribute('class', level > 0 ? 'on' : 'off');
    svg.append(dot);
    WIFI_ARCS.arcs.forEach((d, k) => {
      const p = document.createElementNS(SVGNS, 'path');
      p.setAttribute('d', d);
      p.setAttribute('class', k < level ? 'on' : 'off');
      svg.append(p);
    });
    return svg;
  }

  // ───────────────────────── 文案 ─────────────────────────

  function fmtDuration(seconds) {
    const m = Math.max(1, Math.round(seconds / 60));
    const hh = Math.floor(m / 60);
    const mm = m % 60;
    if (!hh) return `${mm} 分钟`;
    return mm ? `${hh} 小时 ${mm} 分钟` : `${hh} 小时`;
  }

  function fmtBytes(bytes) {
    const GB = 1024 ** 3;
    if (bytes >= 100 * GB) return `${Math.round(bytes / GB)} GB`;
    if (bytes >= GB) return `${(bytes / GB).toFixed(1)} GB`;
    return `${Math.max(1, Math.round(bytes / 1024 ** 2))} MB`;
  }

  const SECURITY = {
    open: '开放（不加密）',
    psk: 'WPA2/WPA3 个人',
    '8021x': '企业（802.1X）',
    wep: 'WEP（不支持）',
  };

  function strengthLabel(strength) {
    return strength >= 60 ? '强' : strength >= 30 ? '中' : '弱';
  }

  /** 把 bridge 的错误转成一句给人看的话（判断用错误名，不用消息）。 */
  function friendlyError(e, context = '') {
    const kind = String(e?.name || '').split('.').pop();
    switch (kind) {
      case 'Failed':
        return context === 'password' ? '密码不正确，或网络拒绝了连接。' : e.message || '操作失败。';
      case 'NotFound':
        return '这个网络已不在附近。';
      case 'Unavailable':
        return '无线网络服务（iwd）没有运行。';
      case 'AccessDenied':
        return '没有权限执行这个操作。';
      case 'ServiceUnknown':
      case 'NameHasNoOwner':
        return '系统服务（loong-sysd）没有运行。';
      case 'NoReply':
        return '系统服务没有及时回应，请稍后再试。';
      default:
        return e?.message || '未知错误。';
    }
  }

  // ───────────────────────── 浮层：sheet 与弹窗 ─────────────────────────

  let openLayers = 0;

  function showLayer(el) {
    if (openLayers++ === 0) {
      $('scrim').classList.remove('is-closing');
      $('scrim').hidden = false;
    }
    el.classList.remove('is-closing');
    el.hidden = false;
  }

  function hideLayer(el) {
    if (el.hidden || el.classList.contains('is-closing')) return;
    const done = () => {
      el.hidden = true;
      el.classList.remove('is-closing');
    };
    el.classList.add('is-closing');
    el.addEventListener('animationend', done, { once: true });
    setTimeout(done, 400);
    if (--openLayers === 0) {
      const scrim = $('scrim');
      scrim.classList.add('is-closing');
      const hideScrim = () => openLayers === 0 && (scrim.hidden = true);
      scrim.addEventListener('animationend', hideScrim, { once: true });
      setTimeout(hideScrim, 400);
    }
  }

  /** 弹窗。buttons: [{ label, value, kind: 'default' | 'danger' }]，返回被点按钮的 value。 */
  function alertBox({ title, message = '', buttons = [{ label: '好', value: true, kind: 'default' }] }) {
    const box = $('alert');
    $('alert-title').textContent = title;
    $('alert-message').textContent = message;
    $('alert-message').hidden = !message;
    const bar = $('alert-buttons');
    bar.replaceChildren();
    return new Promise((resolve) => {
      for (const b of buttons) {
        bar.append(
          h('button', {
            type: 'button',
            class: b.kind ? `is-${b.kind}` : null,
            'data-ink': true,
            onclick: () => {
              hideLayer(box);
              resolve(b.value);
            },
          }, b.label),
        );
      }
      showLayer(box);
      bar.querySelector('.is-default, button:last-child')?.focus();
    });
  }

  // ───────────────────────── 路由 ─────────────────────────

  const ROUTES = {
    wifi: { pane: 'pane-wifi', title: '无线局域网', side: 'wifi' },
    'wifi/network': { pane: 'pane-network', side: 'wifi', parent: 'wifi', back: '无线局域网' },
    display: { pane: 'pane-display', title: '显示与亮度', side: 'display' },
    battery: { pane: 'pane-battery', title: '电池', side: 'battery' },
    general: { pane: 'pane-general', title: '通用', side: 'general' },
    'general/about': { pane: 'pane-about', title: '关于本机', side: 'general', parent: 'general', back: '通用' },
    'general/legal': { pane: 'pane-legal', title: '法律与许可', side: 'general', parent: 'general', back: '通用' },
  };
  let current = { key: null, arg: null };

  const netHref = (ssid) => `#wifi/network/${encodeURIComponent(ssid)}`;

  function parseHash() {
    const raw = location.hash.slice(1);
    if (raw.startsWith('wifi/network/')) {
      try {
        return { key: 'wifi/network', arg: decodeURIComponent(raw.slice('wifi/network/'.length)) };
      } catch {
        return { key: 'wifi', arg: null };
      }
    }
    return { key: ROUTES[raw] ? raw : 'wifi', arg: null };
  }

  function route() {
    const { key, arg } = parseHash();
    const r = ROUTES[key];
    const prev = current;
    const pane = $(r.pane);
    if (prev.key !== key || prev.arg !== arg) {
      for (const p of document.querySelectorAll('.pane')) p.hidden = p !== pane;
      pane.classList.remove('loong-bloom', 'push-in', 'pop-in');
      void pane.offsetWidth; // 重新触发动画
      const sameSection = prev.key && ROUTES[prev.key].side === r.side;
      const depth = r.parent ? 1 : 0;
      const prevDepth = prev.key && ROUTES[prev.key].parent ? 1 : 0;
      // 同一栏目里进入子页面：从右推入；返回：从左回来；换栏目：晕染开启。
      pane.classList.add(sameSection && depth > prevDepth ? 'push-in' : sameSection && depth < prevDepth ? 'pop-in' : 'loong-bloom');
      $('detail').scrollTop = 0;
    }
    current = { key, arg };

    $('nav-title').textContent = r.title ?? arg ?? '';
    const back = $('nav-back');
    back.hidden = !r.parent;
    if (r.parent) {
      back.setAttribute('href', `#${r.parent}`);
      $('nav-back-label').textContent = r.back;
    }
    for (const a of document.querySelectorAll('#side-main a.loong-row')) {
      if (a.getAttribute('href') === `#${r.side}`) a.setAttribute('aria-current', 'page');
      else a.removeAttribute('aria-current');
    }

    if (key === 'wifi') maybeScan(true);
    if (key === 'wifi/network') renderNetwork();
    if (key === 'battery') power.refresh().catch(() => {});
    if (key === 'general/about') loadInfo();
  }

  // ───────────────────────── 侧栏与搜索 ─────────────────────────

  function renderSidebar() {
    const w = state.wifi;
    let wifiText = '';
    if (state.serviceError && !w) wifiText = '';
    else if (w && !w.available) wifiText = '不可用';
    else if (w && !w.enabled) wifiText = '关闭';
    else if (state.connecting || w?.state === 'connecting') wifiText = '正在连接…';
    else if (w?.state === 'connected' && w.ssid) wifiText = w.ssid;
    else if (w) wifiText = '未连接';
    $('side-wifi').textContent = wifiText;

    const p = state.power;
    $('side-battery').textContent = p?.hasBattery ? `${Math.round(p.percentage)}%` : '';

    const info = state.info;
    if (info) {
      $('side-device').textContent = info.device?.name || info.board || info.hostname || '本机';
      $('side-os').textContent = info.os?.prettyName || info.os?.name || 'Loong OS';
    }
  }

  const SEARCH_INDEX = [
    { title: '无线局域网', href: '#wifi', kw: 'wifi wi-fi wlan 无线 网络 密码 连接 上网 热点' },
    { title: '显示与亮度', href: '#display', kw: '显示 亮度 屏幕 背光 外观 深色 浅色 夜墨 宣纸 暗色 主题 dark light' },
    { title: '电池', href: '#battery', kw: '电池 电量 充电 电源 续航 battery' },
    { title: '通用', href: '#general', kw: '通用 general' },
    { title: '关于本机', href: '#general/about', kw: '关于 本机 版本 型号 系统 内核 存储 空间 容量 内存 处理器 webkit about' },
    { title: '法律与许可', href: '#general/legal', kw: '法律 许可 许可证 开源 license' },
  ];

  function onSearch() {
    const q = $('search').value.trim().toLowerCase();
    $('side-main').hidden = Boolean(q);
    $('side-search').hidden = !q;
    if (!q) return;
    const hits = SEARCH_INDEX.filter((e) => `${e.title} ${e.kw}`.toLowerCase().includes(q));
    $('search-results').replaceChildren(
      ...(hits.length
        ? hits.map((e) => h('a', { class: 'loong-row', href: e.href }, h('span', { class: 'loong-row-title' }, e.title), h('span', { class: 'loong-chevron' })))
        : [h('div', { class: 'loong-row net-empty' }, '没有结果')]),
    );
  }

  // ───────────────────────── 无线局域网 ─────────────────────────

  let lastScan = 0;

  /** 停留在无线局域网页面时，每 20 秒扫描一次。 */
  function maybeScan(now = false) {
    const w = state.wifi;
    if (current.key !== 'wifi' || !w?.available || !w.enabled || w.scanning) return;
    if (!now && Date.now() - lastScan < 20000) return;
    lastScan = Date.now();
    wifi.scan().catch((e) => console.warn('扫描失败', e.name, e.message));
  }
  setInterval(maybeScan, 5000);

  async function refreshNets() {
    try {
      state.nets = await wifi.getNetworks();
      state.netsLoaded = true;
    } catch (e) {
      state.nets = [];
    }
    renderWifi();
    if (current.key === 'wifi/network') renderNetwork();
  }

  /** 同名网络可能有多个（开放与加密各一个）：已连接 > 已保存 > 信号最强。 */
  function pickNet(ssid) {
    const same = state.nets.filter((n) => n.ssid === ssid);
    return same.find((n) => n.connected) || same.find((n) => n.known) || same[0] || null;
  }

  function netRow(net, { current: isCurrent = false } = {}) {
    const locked = net.security !== 'open';
    const subtitle = isCurrent ? '已连接' : net.known ? '已保存' : null;
    const row = h(
      'div',
      {
        class: 'loong-row net-row',
        role: 'button',
        tabindex: '0',
        'data-ink': true,
        'aria-label': `${net.ssid}，信号${strengthLabel(net.strength)}${locked ? '，需要密码' : ''}${subtitle ? `，${subtitle}` : ''}`,
      },
      isCurrent ? h('span', { class: 'loong-check', 'aria-hidden': 'true' }) : null,
      h('span', { class: 'loong-row-title' }, net.ssid, subtitle && h('span', { class: 'loong-row-subtitle' }, subtitle)),
      h(
        'span',
        { class: 'loong-row-trail' },
        locked ? symbol('i-lock', 'row-icon') : null,
        inkWifi(wifiLevel(net.strength)),
        isCurrent || net.known ? h('a', { class: 'info-button', href: netHref(net.ssid), 'aria-label': `${net.ssid} 的详细信息` }, symbol('i-info')) : null,
      ),
    );
    const activate = (e) => {
      if (e.target.closest('.info-button')) return;
      if (isCurrent) location.hash = netHref(net.ssid);
      else join(net);
    };
    row.addEventListener('click', activate);
    row.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        activate(e);
      }
    });
    return row;
  }

  let wifiSignature = '';

  function renderWifi() {
    const w = state.wifi;
    const toggle = $('wifi-enabled');
    const note = $('wifi-note');
    if (!w) {
      toggle.disabled = true;
      note.hidden = !state.serviceError;
      note.textContent = state.serviceError ? `无法读取无线网络状态：${friendlyError(state.serviceError)}` : '';
      return;
    }
    toggle.checked = w.enabled;
    toggle.disabled = !w.available || state.toggling;
    note.hidden = w.available;
    note.textContent = w.available ? '' : '没有找到无线网卡，或无线网络服务（iwd）没有运行。';

    const on = w.available && w.enabled;
    $('wifi-nets-section').hidden = !on;
    // SVG 元素没有 hidden 属性（IDL），要直接改特性。
    $('wifi-scanning').toggleAttribute('hidden', !w.scanning);
    $('wifi-scanning').classList.toggle('spin', Boolean(w.scanning));

    const connecting = state.connecting || (w.state === 'connecting' ? '' : null);
    const currentNet = state.nets.find((n) => n.connected) || (w.state === 'connected' && w.ssid ? { ssid: w.ssid, strength: w.strength, security: 'psk', known: true, connected: true } : null);
    const seen = new Set();
    const others = state.nets.filter((n) => {
      const key = `${n.ssid}\n${n.security}`;
      if (n.connected || n.ssid === state.connecting || seen.has(key)) return false;
      seen.add(key);
      return true;
    });

    // 内容没变就不动 DOM（避免打断正在晕开的点墨）。
    const signature = JSON.stringify([on, connecting, currentNet, others, w.scanning, state.netsLoaded]);
    if (signature === wifiSignature) return;
    wifiSignature = signature;

    const main = $('wifi-main');
    while (main.children.length > 1) main.lastChild.remove();
    if (on && connecting !== null) {
      main.append(
        h('div', { class: 'loong-row net-row', 'aria-live': 'polite' }, enso(), h('span', { class: 'loong-row-title' }, connecting || '正在连接', h('span', { class: 'loong-row-subtitle' }, '正在连接…'))),
      );
    } else if (on && currentNet) {
      main.append(netRow(currentNet, { current: true }));
    }

    const list = $('wifi-nets');
    if (!on) {
      list.replaceChildren();
      return;
    }
    list.replaceChildren(
      ...(others.length
        ? others.map((n) => netRow(n))
        : [h('div', { class: 'loong-row net-empty' }, w.scanning || !state.netsLoaded ? '正在搜索附近的网络…' : '附近没有其他可用的网络')]),
    );
  }

  async function connect(ssid, passphrase) {
    state.connecting = ssid;
    renderWifi();
    renderSidebar();
    try {
      await wifi.connect(ssid, passphrase);
    } finally {
      state.connecting = null;
      await refreshNets();
      renderSidebar();
    }
  }

  async function join(net) {
    if (state.connecting) return;
    if (net.connected) {
      location.hash = netHref(net.ssid);
      return;
    }
    if (net.security === 'wep' || (net.security === '8021x' && !net.known)) {
      alertBox({
        title: `无法加入“${net.ssid}”`,
        message: net.security === 'wep' ? 'WEP 加密已不安全，系统不支持。' : '暂不支持需要账户登录的企业网络（802.1X）。',
      });
      return;
    }
    if (net.security === 'psk' && !net.known) {
      passwordSheet(net);
      return;
    }
    try {
      await connect(net.ssid, '');
    } catch (e) {
      alertBox({ title: `无法加入“${net.ssid}”`, message: friendlyError(e) });
    }
  }

  // —— 密码 sheet ——

  let sheetNet = null;

  function passwordSheet(net) {
    sheetNet = net;
    const sheet = $('pw-sheet');
    $('pw-title').textContent = `输入“${net.ssid}”的密码`;
    const input = $('pw-input');
    input.value = '';
    input.type = 'password';
    input.disabled = false;
    $('pw-eye').replaceChildren(symbol('i-eye'));
    $('pw-join').disabled = true;
    setSheetStatus(PW_HINT);
    sheet.classList.remove('is-busy');
    showLayer(sheet);
    setTimeout(() => input.focus(), 60);
  }

  const PW_HINT = '密码为 8–63 个字符，区分大小写。';

  function setSheetStatus(text, { busy = false, error = false } = {}) {
    const s = $('pw-status');
    s.classList.toggle('is-error', error);
    s.replaceChildren(...(busy ? [enso()] : []), text);
  }

  function validPassphrase(p) {
    return (p.length >= 8 && p.length <= 63) || /^[0-9a-fA-F]{64}$/.test(p);
  }

  $('pw-input').addEventListener('input', () => {
    $('pw-join').disabled = !validPassphrase($('pw-input').value) || $('pw-sheet').classList.contains('is-busy');
  });
  $('pw-eye').addEventListener('click', () => {
    const input = $('pw-input');
    const show = input.type === 'password';
    input.type = show ? 'text' : 'password';
    $('pw-eye').replaceChildren(symbol(show ? 'i-eye-off' : 'i-eye'));
    $('pw-eye').setAttribute('aria-label', show ? '隐藏密码' : '显示密码');
    input.focus();
  });
  $('pw-cancel').addEventListener('click', () => hideLayer($('pw-sheet')));
  $('pw-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const net = sheetNet;
    const input = $('pw-input');
    const pass = input.value;
    if (!net || !validPassphrase(pass)) return;
    const sheet = $('pw-sheet');
    sheet.classList.add('is-busy');
    input.disabled = true;
    $('pw-join').disabled = true;
    setSheetStatus('正在加入…', { busy: true });
    try {
      await connect(net.ssid, pass);
      if (sheetNet === net) hideLayer(sheet);
      const check = document.querySelector('#wifi-main .loong-check');
      if (check && window.LoongKit) LoongKit.stamp(check); // 落印
    } catch (err) {
      if (sheetNet !== net) return;
      sheet.classList.remove('is-busy');
      input.disabled = false;
      input.select();
      $('pw-join').disabled = !validPassphrase(input.value);
      setSheetStatus(friendlyError(err, 'password'), { error: true });
    }
  });
  addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    if (!$('alert').hidden) return;
    if (!$('pw-sheet').hidden) hideLayer($('pw-sheet'));
  });
  $('scrim').addEventListener('click', () => {
    if ($('alert').hidden && !$('pw-sheet').hidden) hideLayer($('pw-sheet'));
  });

  // —— 某个网络的详情 ——

  function infoRow(title, value) {
    return h('div', { class: 'loong-row' }, h('span', { class: 'loong-row-title' }, title), h('span', { class: 'loong-row-value' }, value));
  }

  function actionRow(label, onclick, kind = '') {
    return h('button', { type: 'button', class: `loong-row action-row ${kind}`.trim(), onclick }, label);
  }

  function renderNetwork() {
    const ssid = current.arg;
    if (ssid == null) return;
    const net = pickNet(ssid);
    const connectingHere = state.connecting === ssid;
    const status = connectingHere ? '正在连接…' : net?.connected ? '已连接' : net?.known ? '已保存，未连接' : net ? '未连接' : '不在附近';
    $('net-hero').replaceChildren(
      inkWifi(net ? wifiLevel(net.strength) : 0),
      h('h2', { class: 'loong-title2' }, ssid),
      h('p', null, status),
    );
    $('net-info').replaceChildren(
      ...(net
        ? [
            infoRow('信号强度', `${strengthLabel(net.strength)} · ${String(net.signalDbm).replace('-', '−')} dBm`),
            infoRow('安全性', SECURITY[net.security] || net.security),
            infoRow('自动加入', net.known ? '是' : '否'),
          ]
        : [infoRow('状态', '附近没有这个网络')]),
    );
    const actions = [];
    if (net?.connected) actions.push(actionRow('断开连接', () => wifi.disconnect().catch((e) => alertBox({ title: '无法断开连接', message: friendlyError(e) }))));
    else if (net && !connectingHere) actions.push(actionRow('加入此网络', () => join(net)));
    if (net?.known)
      actions.push(
        actionRow(
          '忽略此网络',
          async () => {
            const ok = await alertBox({
              title: `忽略“${ssid}”？`,
              message: '本机将不再自动加入这个网络，并删除保存的密码。',
              buttons: [
                { label: '取消', value: false },
                { label: '忽略', value: true, kind: 'danger' },
              ],
            });
            if (!ok) return;
            try {
              await wifi.forget(ssid);
              location.hash = '#wifi';
            } catch (e) {
              alertBox({ title: `无法忽略“${ssid}”`, message: friendlyError(e) });
            }
          },
          'danger',
        ),
      );
    $('net-actions').replaceChildren(...actions);
    $('net-actions').parentElement.hidden = actions.length === 0;
    $('net-note').hidden = !net?.known;
    $('net-note').textContent = net?.known ? '已保存的网络在附近时会被自动加入。' : '';
  }

  $('wifi-enabled').addEventListener('change', async (e) => {
    const enabled = e.target.checked;
    state.toggling = true;
    renderWifi();
    try {
      await wifi.setEnabled(enabled);
      if (enabled) lastScan = 0;
    } catch (err) {
      e.target.checked = !enabled;
      alertBox({ title: enabled ? '无法打开无线局域网' : '无法关闭无线局域网', message: friendlyError(err) });
    } finally {
      state.toggling = false;
      renderWifi();
    }
  });

  // ───────────────────────── 显示与亮度 ─────────────────────────

  const looks = document.querySelectorAll('input[name="look"]');

  function renderTheme() {
    for (const i of looks) {
      i.checked = i.value === state.theme;
      i.parentElement.querySelector('.look-mark').classList.toggle('loong-check', i.checked);
    }
  }

  function applyTheme(theme, animate) {
    if (theme !== 'light' && theme !== 'dark') return;
    state.theme = theme;
    if (animate && window.LoongKit) LoongKit.setTheme(theme);
    else root.dataset.theme = theme;
    renderTheme();
  }

  for (const i of looks) {
    i.addEventListener('change', async () => {
      const prev = state.theme;
      applyTheme(i.value, true);
      if (window.LoongKit) LoongKit.stamp(i.parentElement.querySelector('.look-mark')); // 落印
      try {
        await loong.appearance.set(i.value);
      } catch (e) {
        applyTheme(prev, true);
        alertBox({ title: '无法更改外观', message: friendlyError(e) });
      }
    });
  }
  loong.appearance.addEventListener('change', (e) => {
    if (e.detail.theme !== state.theme) applyTheme(e.detail.theme, true);
  });

  // —— 亮度：拖动时连续发送（同一时刻最多一个请求在途，每帧最多一次），loong-sysd 用弹簧平滑过渡；
  //    别处改了亮度（控制中心、热键）时，滑块用 SMOOTH 弹簧跟过去。 ——

  const slider = $('brightness');
  const bright = { dragging: false, inflight: false, pending: null, quietUntil: 0 };
  const knob = window.LoongKit
    ? LoongKit.springValue(slider.value / 1000, (v) => {
        slider.value = Math.round(v * 1000);
        LoongKit.syncSlider(slider);
      })
    : { value: 0.5, to(v) { this.set(v); }, set(v) { this.value = v; slider.value = Math.round(v * 1000); } };

  const nextFrame = () => new Promise((r) => requestAnimationFrame(() => r()));

  async function pumpBrightness() {
    if (bright.inflight) return;
    bright.inflight = true;
    while (bright.pending !== null) {
      const v = bright.pending;
      bright.pending = null;
      bright.quietUntil = performance.now() + 500;
      try {
        await backlight.setBrightness(v, { animate: true });
      } catch (e) {
        console.warn('设置亮度失败', e.name, e.message);
        if (/AccessDenied|NotSupported/.test(e.name)) {
          bright.pending = null;
          alertBox({ title: '无法调节亮度', message: friendlyError(e) });
          break;
        }
      }
      await nextFrame();
    }
    bright.inflight = false;
    // 安静期过后再和 loong-sysd 的值对一次（期间收到的回声可能是旧值）。
    setTimeout(renderBacklight, 520);
  }

  slider.addEventListener('input', () => {
    bright.dragging = true;
    const v = slider.value / 1000;
    knob.set(v);
    bright.pending = v;
    pumpBrightness();
  });
  const endDrag = () => {
    bright.dragging = false;
  };
  slider.addEventListener('change', endDrag);
  slider.addEventListener('pointerup', endDrag);
  slider.addEventListener('pointercancel', endDrag);

  function renderBacklight(initial = false) {
    const p = state.backlight;
    const available = Boolean(p?.available);
    slider.disabled = !available;
    $('bright-slider').parentElement.classList.toggle('is-disabled', !available);
    $('bright-note').hidden = available || !p;
    if (!p) return;
    if (!available) {
      knob.set(0);
      return;
    }
    if (bright.dragging || bright.inflight || performance.now() < bright.quietUntil) return;
    if (Math.abs(knob.value - p.brightness) < 0.002) return;
    if (initial === true) knob.set(p.brightness);
    else knob.to(p.brightness);
  }

  // ───────────────────────── 电池 ─────────────────────────

  function renderBattery() {
    const p = state.power;
    const pct = $('battery-pct');
    const st = $('battery-state');
    const art = $('battery-art');
    const info = $('battery-info');
    if (!p) {
      pct.textContent = '—';
      st.textContent = state.serviceError ? `无法读取电池状态：${friendlyError(state.serviceError)}` : '';
      info.replaceChildren();
      return;
    }
    const charging = p.state === 'charging';
    const low = p.hasBattery && !charging && !p.acOnline && (p.warningLevel !== 'none' || p.percentage <= p.lowThreshold);
    art.classList.toggle('is-none', !p.hasBattery);
    art.classList.toggle('is-low', low);
    // 墨洇到电量处：电芯内框 x=13..163。
    const level = p.hasBattery ? Math.max(0, Math.min(100, p.percentage)) : 0;
    $('battery-fill').setAttribute('width', level > 0 ? (13 + 1.5 * level).toFixed(1) : '0');
    $('battery-bolt').toggleAttribute('hidden', !charging);

    let stateText;
    let icon = null;
    if (!p.hasBattery) {
      pct.classList.add('is-text');
      pct.textContent = '外接电源';
      stateText = '本机没有电池，由外接电源供电';
      icon = 'i-plug';
    } else {
      pct.classList.remove('is-text');
      pct.replaceChildren(String(Math.round(p.percentage)), h('small', null, '%'));
      switch (p.state) {
        case 'charging':
          stateText = p.timeToFull ? `正在充电 · 约 ${fmtDuration(p.timeToFull)}后充满` : '正在充电';
          icon = 'i-zap';
          break;
        case 'discharging':
          stateText = p.timeToEmpty ? `使用电池 · 约可使用 ${fmtDuration(p.timeToEmpty)}` : '使用电池';
          break;
        case 'full':
          stateText = '已充满';
          icon = 'i-plug';
          break;
        case 'not-charging':
          stateText = '已接通电源，暂停充电';
          icon = 'i-plug';
          break;
        default:
          stateText = p.acOnline ? '已接通电源' : '电池状态未知';
      }
    }
    st.classList.toggle('is-low', low);
    st.replaceChildren(...(icon ? [symbol(icon, icon === 'i-zap' ? 'loong-symbol--fill' : '')] : []), stateText);

    const rows = [infoRow('电源适配器', p.acOnline ? '已接通' : '未接通')];
    if (p.hasBattery) {
      rows.push(infoRow('电量', `${p.percentage.toFixed(1)}%`));
      if (p.state === 'discharging' && p.timeToEmpty) rows.push(infoRow('预计可使用', fmtDuration(p.timeToEmpty)));
      if (p.state === 'charging' && p.timeToFull) rows.push(infoRow('预计充满需要', fmtDuration(p.timeToFull)));
      rows.push(infoRow('低电量提醒', `低于 ${Math.round(p.lowThreshold)}% 时`));
    }
    info.replaceChildren(...rows);
    $('battery-note').textContent = p.hasBattery ? '接上或拔下电源后，状态会立即更新。' : '';
    $('battery-note').hidden = !p.hasBattery;

    // 电量历史：loong-sysd 还没有提供，只有演示数据有。
    const history = power.history;
    $('battery-chart-section').hidden = !(p.hasBattery && Array.isArray(history) && history.length);
    if (Array.isArray(history)) {
      $('battery-bars').replaceChildren(
        ...history.map((s) => {
          const bar = h('i', { class: s.charging ? 'charge' : null });
          bar.style.setProperty('--h', Math.max(2, Math.min(100, s.percentage)));
          return bar;
        }),
      );
    }
  }

  // ───────────────────────── 关于本机 ─────────────────────────

  let infoLoading = null;

  function loadInfo() {
    infoLoading ??= loong.device
      .info()
      .then((info) => {
        state.info = info;
        renderAbout();
        renderSidebar();
      })
      .catch((e) => console.warn('读取本机信息失败', e.name, e.message))
      .finally(() => (infoLoading = null));
    return infoLoading;
  }

  function renderAbout() {
    const info = state.info;
    if (!info) return;
    const os = info.os || {};
    $('about-os').textContent = os.name || 'Loong OS';
    const version = os.versionId || os.version || os.imageVersion;
    $('about-version').textContent = version ? `版本 ${version}` : '';

    const rows = [];
    if (info.hostname) rows.push(infoRow('名称', info.hostname));
    const model = info.device?.name || info.board;
    if (model) rows.push(infoRow('型号', model));
    const s = info.device?.screen;
    if (s?.width && s?.height) {
      const parts = [`${s.width} × ${s.height}`];
      if (s.diagonal_inch) parts.push(`${s.diagonal_inch} 英寸`);
      if (s.refresh_hz) parts.push(`${s.refresh_hz} Hz`);
      rows.push(infoRow('屏幕', parts.join(' · ')));
    }
    const cpu = info.cpu || {};
    const cpuName = cpu.soc || cpu.model;
    if (cpuName || cpu.count) rows.push(infoRow('处理器', [cpuName, cpu.count ? `${cpu.count} 核` : null].filter(Boolean).join(' · ')));
    if (info.memory?.total) rows.push(infoRow('内存', fmtBytes(info.memory.total)));
    if (os.prettyName) rows.push(infoRow('系统', os.prettyName));
    if (info.kernel) rows.push(infoRow('内核', `${info.kernel.name} ${info.kernel.release}`));
    if (info.webkit) rows.push(infoRow('Web 内核', `WPE WebKit ${info.webkit}`));
    $('about-info').replaceChildren(...rows);

    // 存储：设备上系统分区（只读 erofs）与数据分区分开；开发环境可能是同一个文件系统。
    const st = info.storage || {};
    const data = st.data;
    if (!data) return;
    const sys = st.separate && st.system ? st.system.total - st.system.available : 0;
    const total = data.total + (st.separate && st.system ? st.system.total : 0);
    const dataUsed = Math.max(0, data.total - data.available);
    const used = sys + dataUsed;
    $('storage-used').textContent = `已使用 ${fmtBytes(used)}`;
    $('storage-total').textContent = `共 ${fmtBytes(total)}`;
    const pct = (v) => (total ? (100 * v) / total : 0).toFixed(2);
    const segments = [
      st.separate ? { label: '系统', value: sys, color: 'var(--loong-color-ink-zhong)' } : null,
      { label: st.separate ? '应用与数据' : '已使用', value: dataUsed, color: 'var(--loong-color-shiqing)' },
    ].filter(Boolean);
    $('storage-meter').replaceChildren(
      ...segments.map((seg) => {
        const i = h('i');
        i.style.setProperty('--w', pct(seg.value));
        i.style.setProperty('--c', seg.color);
        return i;
      }),
    );
    $('storage-meter').setAttribute('aria-label', segments.map((s) => `${s.label} ${fmtBytes(s.value)}`).join('，') + `，可用 ${fmtBytes(data.available)}`);
    $('storage-legend').replaceChildren(
      ...segments.map((seg) => {
        const span = h('span', null, `${seg.label} ${fmtBytes(seg.value)}`);
        span.style.setProperty('--c', seg.color);
        return span;
      }),
      (() => {
        const span = h('span', null, `可用 ${fmtBytes(data.available)}`);
        span.style.setProperty('--c', 'var(--loong-color-fill)');
        return span;
      })(),
    );
  }

  // ───────────────────────── 启动 ─────────────────────────

  function onServiceError(e) {
    console.warn('loong-sysd 不可用', e.name, e.message);
    state.serviceError = e;
    renderWifi();
    renderBattery();
    renderSidebar();
  }

  function init() {
    $('mock-note').hidden = !MOCK;
    $('search').addEventListener('input', onSearch);
    addEventListener('hashchange', route);
    route();

    wifi.watch((p, changed) => {
      state.wifi = p;
      state.serviceError = null;
      renderWifi();
      renderSidebar();
      if (changed?.includes('enabled') || changed?.includes('state')) refreshNets();
      maybeScan();
    }, onServiceError);
    wifi.addEventListener('networkschanged', refreshNets);
    refreshNets();

    power.watch((p) => {
      state.power = p;
      renderBattery();
      renderSidebar();
    }, onServiceError);

    backlight.watch((p, changed) => {
      state.backlight = p;
      renderBacklight(changed === null);
    }, onServiceError);

    loong.appearance.get().then(
      (theme) => applyTheme(theme, false),
      (e) => console.warn('读取外观失败', e.name, e.message),
    );
    loadInfo();
  }

  init();
})();
