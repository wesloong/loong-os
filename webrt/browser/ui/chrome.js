/*
 * 浏览器界面的逻辑。与 loong-browser（C）通过 script message handler “loong” 通信：
 *
 *   界面 → 本进程：post({ cmd: 'navigate' | 'back' | 'forward' | 'reload' | 'stop' | 'newTab' |
 *                        'closeTab' | 'switchTab' | 'focusContent' | 'install' | 'layout' |
 *                        'dialogResult' | 'menuResult' | 'ready', … })
 *   本进程 → 界面：loongChrome.update(状态) / dialog(…) / menu(…) / closeModal() / toast(…)
 *
 * 界面平时只占顶部（layout.height）；有浮层时 expanded=true，本进程把界面拉到整个窗口。
 */
(() => {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const post = (message) => window.webkit?.messageHandlers?.loong?.postMessage(message);
  const root = document.documentElement;
  const chrome = $('chrome');
  const input = $('address-input');
  const label = $('address-label');
  const address = $('address');

  let state = null;
  let editing = false;
  let wasLoading = false;
  let doneTimer = 0;
  let focusAddressOnUpdate = false;
  let firstRender = true;

  /* ───────── 布局：告诉本进程界面需要多高、是否铺满窗口 ───────── */

  const expandedBy = new Set();
  let posted = { height: -1, expanded: null };

  function postLayout() {
    const height = Math.ceil(chrome.getBoundingClientRect().height);
    const expanded = expandedBy.size > 0;
    if (height === posted.height && expanded === posted.expanded) return;
    posted = { height, expanded };
    post({ cmd: 'layout', height, expanded });
  }

  function expand(reason, on) {
    if (on) expandedBy.add(reason);
    else expandedBy.delete(reason);
    postLayout();
  }

  new ResizeObserver(postLayout).observe(chrome);

  /* ───────── 地址显示 ───────── */

  function displayUrl(url) {
    try {
      const u = new URL(url);
      if (u.protocol === 'http:' || u.protocol === 'https:') return u.host.replace(/^www\./, '');
      if (u.protocol === 'file:') return decodeURIComponent(u.pathname.split('/').filter(Boolean).pop() || u.pathname);
      return url;
    } catch (e) {
      return url;
    }
  }

  function setSymbol(svg, id) {
    const use = svg.querySelector('use');
    if (use.getAttribute('href') !== '#' + id) use.setAttribute('href', '#' + id);
  }

  function activeTab() {
    return state && state.tabs.find((t) => t.id === state.active);
  }

  /* ───────── 状态更新 ───────── */

  function update(s) {
    state = s;
    const tab = activeTab();
    if (!tab) return;

    root.style.setProperty('--status-bar', `${s.statusBar ?? 24}px`);
    $('back').disabled = !s.canGoBack;
    $('forward').disabled = !s.canGoForward;
    const reload = $('reload');
    setSymbol(reload.querySelector('svg'), tab.loading ? 'i-x' : 'i-reload');
    reload.setAttribute('aria-label', tab.loading ? '停止' : '重新加载');
    $('share').disabled = !s.canInstall;

    label.textContent = tab.start ? '' : displayUrl(tab.url);
    label.title = tab.title || '';
    setSymbol($('address-icon'), tab.start ? 'i-search' : s.secure ? 'i-lock' : 'i-globe');
    if (!editing) input.value = tab.url;

    // 主题色：淡淡洇在工具栏的纸上。
    chrome.style.setProperty('--tint', s.themeColor ? `color-mix(in srgb, ${s.themeColor} 13%, transparent)` : 'transparent');

    updateProgress(tab);
    renderTabs(s);
    firstRender = false;

    if (focusAddressOnUpdate && tab.start) {
      focusAddressOnUpdate = false;
      input.focus();
    }
  }

  /* 加载进度：一笔浓墨随进度写出去；加载完成时写满，然后墨迹淡去。 */
  function updateProgress(tab) {
    clearTimeout(doneTimer);
    if (tab.loading) {
      root.classList.remove('is-done');
      root.classList.add('is-loading');
      address.style.setProperty('--p', Math.max(0.08, tab.progress).toFixed(3));
    } else if (wasLoading) {
      address.style.setProperty('--p', '1');
      root.classList.add('is-done');
      doneTimer = setTimeout(() => {
        root.classList.remove('is-loading', 'is-done');
        doneTimer = setTimeout(() => address.style.setProperty('--p', '0'), 800);
      }, 900);
    }
    wasLoading = tab.loading;
  }

  /* ───────── 标签栏 ───────── */

  const strip = $('tabstrip');

  function renderTabs(s) {
    const many = s.tabs.length > 1;
    root.classList.toggle('has-tabs', many);
    if (!many) {
      strip.replaceChildren();
      return;
    }
    const existing = new Map([...strip.children].map((el) => [Number(el.dataset.id), el]));
    const order = [];
    for (const t of s.tabs) {
      let el = existing.get(t.id);
      if (!el) {
        el = document.createElement('div');
        el.className = 'tab';
        el.dataset.id = t.id;
        el.dataset.ink = '';
        el.setAttribute('role', 'tab');
        el.innerHTML =
          '<svg class="loong-symbol tab-icon"><use href="#i-globe"/></svg><span class="tab-title"></span>' +
          '<button class="tab-close" aria-label="关闭标签页"><svg class="loong-symbol"><use href="#i-x"/></svg></button>';
        el.addEventListener('click', () => post({ cmd: 'switchTab', id: t.id }));
        el.querySelector('.tab-close').addEventListener('click', (e) => {
          e.stopPropagation();
          post({ cmd: 'closeTab', id: t.id });
        });
        if (!firstRender) {
          el.classList.add('is-new');
          el.addEventListener('animationend', () => el.classList.remove('is-new'), { once: true });
        }
      }
      el.classList.toggle('is-active', t.id === s.active);
      el.classList.toggle('is-loading', t.loading);
      el.setAttribute('aria-selected', t.id === s.active ? 'true' : 'false');
      const title = t.title || displayUrl(t.url) || '无标题';
      el.querySelector('.tab-title').textContent = title;
      el.title = title;
      updateTabIcon(el, t.favicon);
      order.push(el);
    }
    strip.replaceChildren(...order);
    const active = strip.querySelector('.is-active');
    active?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }

  function updateTabIcon(el, favicon) {
    const current = el.querySelector('.tab-icon');
    if ((current.dataset.src || '') === (favicon || '')) return;
    let icon;
    if (favicon) {
      icon = document.createElement('img');
      icon.className = 'tab-icon';
      icon.alt = '';
      icon.src = favicon;
      icon.onerror = () => {
        const fallback = globeIcon();
        fallback.dataset.src = favicon;
        icon.replaceWith(fallback);
      };
    } else {
      icon = globeIcon();
    }
    icon.dataset.src = favicon || '';
    current.replaceWith(icon);
  }

  function globeIcon() {
    const ns = 'http://www.w3.org/2000/svg';
    const svg = document.createElementNS(ns, 'svg');
    svg.setAttribute('class', 'loong-symbol tab-icon');
    const use = document.createElementNS(ns, 'use');
    use.setAttribute('href', '#i-globe');
    svg.append(use);
    return svg;
  }

  /* ───────── 地址栏编辑 ───────── */

  function startEditing() {
    if (editing) return;
    editing = true;
    root.classList.add('is-editing');
    const tab = activeTab();
    input.value = tab && !tab.start ? tab.url : '';
    setTimeout(() => input.select(), 0);
  }

  function endEditing() {
    if (!editing) return;
    editing = false;
    root.classList.remove('is-editing');
    const tab = activeTab();
    input.value = tab ? tab.url : '';
    if (document.activeElement === input) input.blur();
  }

  input.addEventListener('focus', startEditing);
  input.addEventListener('blur', () => setTimeout(() => document.activeElement !== input && endEditing(), 0));
  window.addEventListener('blur', endEditing);
  address.addEventListener('submit', (e) => {
    e.preventDefault();
    const text = input.value.trim();
    if (!text) return;
    endEditing();
    post({ cmd: 'navigate', text });
  });
  $('cancel').addEventListener('pointerdown', (e) => e.preventDefault()); // 不让输入框先失焦
  $('cancel').addEventListener('click', () => {
    endEditing();
    post({ cmd: 'focusContent' });
  });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      endEditing();
      post({ cmd: 'focusContent' });
    }
  });

  /* ───────── 工具按钮 ───────── */

  $('back').addEventListener('click', () => post({ cmd: 'back' }));
  $('forward').addEventListener('click', () => post({ cmd: 'forward' }));
  $('reload').addEventListener('click', () => post({ cmd: activeTab()?.loading ? 'stop' : 'reload' }));
  $('new-tab').addEventListener('click', () => {
    focusAddressOnUpdate = true;
    post({ cmd: 'newTab' });
  });

  document.addEventListener('keydown', (e) => {
    if (!e.ctrlKey) return;
    const key = e.key.toLowerCase();
    if (key === 'l') input.focus();
    else if (key === 't') $('new-tab').click();
    else if (key === 'w' && state) post({ cmd: 'closeTab', id: state.active });
    else return;
    e.preventDefault();
  });

  /* ───────── 浮层：遮罩 ───────── */

  const scrim = $('scrim');
  let scrimAction = null;

  function showScrim(dim, onTap) {
    scrim.hidden = false;
    scrim.classList.toggle('scrim--dim', dim);
    scrimAction = onTap;
  }

  function hideScrim() {
    scrim.hidden = true;
    scrimAction = null;
  }

  scrim.addEventListener('pointerdown', (e) => {
    e.preventDefault();
    scrimAction?.();
  });

  /* ───────── 分享菜单 ───────── */

  const shareMenu = $('share-menu');

  function openShare() {
    const r = $('share').getBoundingClientRect();
    shareMenu.style.top = `${Math.round(chrome.getBoundingClientRect().bottom + 6)}px`;
    shareMenu.style.right = `${Math.round(window.innerWidth - r.right)}px`;
    shareMenu.hidden = false;
    showScrim(false, closeShare);
    expand('share', true);
  }

  function closeShare() {
    shareMenu.hidden = true;
    hideScrim();
    expand('share', false);
  }

  $('share').addEventListener('click', () => (shareMenu.hidden ? openShare() : closeShare()));
  $('install').addEventListener('click', () => {
    closeShare();
    post({ cmd: 'install' });
  });
  $('copy-link').addEventListener('click', () => {
    closeShare();
    const url = activeTab()?.url;
    if (!url) return;
    const area = document.createElement('textarea');
    area.value = url;
    document.body.append(area);
    area.select();
    const ok = document.execCommand('copy');
    area.remove();
    toast({ kind: ok ? 'copied' : 'error', text: ok ? '已拷贝链接' : '无法拷贝链接' });
  });

  /* ───────── 网页对话框（alert / confirm / prompt / 离开确认） ───────── */

  const dialog = $('dialog');
  let dialogId = 0;

  function showDialog(d) {
    closeModal();
    dialogId = d.id;
    const leave = d.type === 'beforeunload';
    $('dialog-title').textContent = leave ? '要离开此网页吗？' : `“${d.origin}”上的网页`;
    $('dialog-message').textContent = leave ? '你所做的更改可能不会保存。' : d.message || '';
    const prompt = $('dialog-input');
    prompt.hidden = d.type !== 'prompt';
    prompt.value = d.defaultText || '';
    $('dialog-cancel').hidden = d.type === 'alert';
    $('dialog-cancel').textContent = leave ? '留下' : '取消';
    $('dialog-ok').textContent = leave ? '离开' : '好';
    dialog.hidden = false;
    showScrim(true, null);
    expand('dialog', true);
    setTimeout(() => (d.type === 'prompt' ? prompt.select() : $('dialog-ok').focus()), 30);
  }

  function finishDialog(ok) {
    if (dialog.hidden) return;
    post({ cmd: 'dialogResult', id: dialogId, ok, text: $('dialog-input').value });
    hideDialog();
  }

  function hideDialog() {
    dialog.hidden = true;
    hideScrim();
    expand('dialog', false);
  }

  $('dialog-ok').addEventListener('click', () => finishDialog(true));
  $('dialog-cancel').addEventListener('click', () => finishDialog(false));
  dialog.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') finishDialog(true);
    else if (e.key === 'Escape' && !$('dialog-cancel').hidden) finishDialog(false);
  });

  /* ───────── <select> 菜单 ───────── */

  const selectMenu = $('select-menu');
  let menuId = 0;

  function showMenu(m) {
    closeModal();
    menuId = m.id;
    selectMenu.replaceChildren();
    m.items.forEach((item, index) => {
      const row = document.createElement('button');
      row.className = 'loong-row';
      if (item.group) {
        row.classList.add('is-group');
        row.disabled = true;
      } else {
        if (item.child) row.classList.add('is-child');
        row.disabled = !item.enabled;
        row.setAttribute('role', 'option');
        row.setAttribute('aria-selected', item.selected ? 'true' : 'false');
        row.addEventListener('click', () => finishMenu(index));
      }
      const title = document.createElement('span');
      title.className = 'loong-row-title';
      title.textContent = item.label;
      row.append(title);
      if (item.selected) {
        const check = document.createElement('span');
        check.className = 'loong-check';
        row.append(check);
      }
      selectMenu.append(row);
    });
    selectMenu.style.minWidth = `${Math.max(200, m.width)}px`;
    selectMenu.style.maxHeight = `${Math.round((m.windowHeight || window.innerHeight) * 0.5)}px`;
    selectMenu.style.left = `${Math.max(8, m.x)}px`;
    selectMenu.style.top = `${m.y + m.height + 4}px`;
    selectMenu.hidden = false;
    showScrim(false, () => finishMenu(-1));
    expand('menu', true);
    // 放不下就翻到下拉框上方。用窗口尺寸（本进程给出）判断：此刻界面可能还没被拉到整个窗口。
    const r = selectMenu.getBoundingClientRect();
    const winW = m.windowWidth || window.innerWidth;
    const winH = m.windowHeight || window.innerHeight;
    if (r.bottom > winH - 8 && m.y - r.height - 4 > 8) selectMenu.style.top = `${m.y - r.height - 4}px`;
    if (r.right > winW - 8) selectMenu.style.left = `${Math.max(8, winW - r.width - 8)}px`;
    selectMenu.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' });
  }

  function finishMenu(index) {
    if (selectMenu.hidden) return;
    post({ cmd: 'menuResult', id: menuId, index });
    hideMenu();
  }

  function hideMenu() {
    selectMenu.hidden = true;
    hideScrim();
    expand('menu', false);
  }

  function closeModal() {
    if (!dialog.hidden) hideDialog();
    if (!selectMenu.hidden) hideMenu();
  }

  /* ───────── 提示：落一方印 ───────── */

  const toastEl = $('toast');
  let toastTimer = 0;

  function toast(t) {
    clearTimeout(toastTimer);
    const installed = t.kind === 'installed';
    $('toast-seal').textContent = installed ? '成' : t.kind === 'error' ? '误' : '记';
    $('toast-text').textContent = installed ? `已添加到主屏幕：${t.text}` : t.text;
    toastEl.classList.remove('is-leaving');
    toastEl.style.top = `${Math.round(chrome.getBoundingClientRect().bottom + 12)}px`;
    toastEl.hidden = false;
    expand('toast', true);
    window.LoongKit?.stamp($('toast-seal'));
    toastTimer = setTimeout(() => {
      toastEl.classList.add('is-leaving');
      toastTimer = setTimeout(() => {
        toastEl.hidden = true;
        expand('toast', false);
      }, 600);
    }, 2400);
  }

  window.loongChrome = { update, dialog: showDialog, menu: showMenu, closeModal, toast };

  postLayout();
  post({ cmd: 'ready' });
})();
