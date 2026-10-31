/*
 * 设置的演示数据：在普通浏览器里（没有 window.loong）或地址带 ?mock 时使用，
 * 接口形状与 loong-webapp 注入的 window.loong 完全相同（docs/api/loong-js-bridge.md），
 * 便于在桌面浏览器里开发、截图（tools/web-shot.mjs）。
 *
 *   index.html?mock=1          充电中、已连 Wi‑Fi
 *   index.html?mock=battery    使用电池
 *   index.html?mock=low        电量低
 *   index.html?mock=desktop    没有电池、没有背光、没有 Wi‑Fi 网卡（类似台式机或虚拟机）
 *   index.html?mock=wifi-off   Wi‑Fi 已关闭
 *
 * 演示网络的密码都是 loong1234。
 */
(() => {
  'use strict';

  const PASSWORD = 'loong1234';
  const delay = (ms) => new Promise((r) => setTimeout(r, ms));

  class LoongError extends Error {
    constructor(name, message) {
      super(message);
      Object.defineProperty(this, 'name', { value: name, enumerable: false, configurable: true, writable: true });
    }
  }
  const err = (kind, message) => new LoongError(`org.loong.System1.Error.${kind}`, message);

  class Service extends EventTarget {
    constructor(props) {
      super();
      this._props = props;
      this._watchers = new Set();
    }
    async getAll() {
      await delay(10);
      return { ...this._props };
    }
    get props() {
      return { ...this._props };
    }
    watch(cb) {
      this._watchers.add(cb);
      this.getAll().then((p) => this._watchers.has(cb) && cb(p, null));
      return () => this._watchers.delete(cb);
    }
    _change(changed) {
      Object.assign(this._props, changed);
      this.dispatchEvent(new CustomEvent('change', { detail: changed }));
      for (const cb of this._watchers) cb(this.props, Object.keys(changed));
    }
  }

  function create(params) {
    const scenario = params.get('mock') || '1';
    const desktop = scenario === 'desktop';

    // —— 背光 ——
    const backlight = new Service({ available: !desktop, brightness: desktop ? 0 : 0.64, maxRaw: desktop ? 0 : 2047, device: desktop ? '' : 'panel0-backlight' });
    backlight.setBrightness = async (value, { animate = true } = {}) => {
      if (!Number.isFinite(value)) throw new LoongError('org.loong.Bridge.Error.InvalidArgs', '亮度应为有限的数字');
      if (!backlight._props.available) throw err('NotSupported', '本机没有背光设备');
      await delay(4);
      backlight._change({ brightness: Math.min(1, Math.max(0, value)) });
    };

    // —— 电池 ——
    const power = new Service(
      desktop
        ? { hasBattery: false, percentage: 0, state: 'unknown', timeToEmpty: 0, timeToFull: 0, acOnline: true, warningLevel: 'none', lowThreshold: 15, criticalThreshold: 5 }
        : scenario === 'battery'
          ? { hasBattery: true, percentage: 64, state: 'discharging', timeToEmpty: 6 * 3600 + 25 * 60, timeToFull: 0, acOnline: false, warningLevel: 'none', lowThreshold: 15, criticalThreshold: 5 }
          : scenario === 'low'
            ? { hasBattery: true, percentage: 9, state: 'discharging', timeToEmpty: 48 * 60, timeToFull: 0, acOnline: false, warningLevel: 'low', lowThreshold: 15, criticalThreshold: 5 }
            : { hasBattery: true, percentage: 78, state: 'charging', timeToEmpty: 0, timeToFull: 42 * 60, acOnline: true, warningLevel: 'none', lowThreshold: 15, criticalThreshold: 5 },
    );
    power.refresh = async () => delay(5);
    // 只有演示数据有历史曲线（loong-sysd 还没有记录电量历史的接口）。
    if (!desktop) {
      const h = [92, 88, 85, 83, 82, 81, 78, 71, 64, 58, 52, 47, 55, 68, 79, 74, 69, 63, 57, 50, 44, 58, 70, 78];
      const charging = new Set([12, 13, 14, 21, 22, 23]);
      power.history = h.map((v, i) => ({ percentage: v, charging: charging.has(i) }));
    }

    // —— Wi‑Fi ——
    const nets = desktop
      ? []
      : [
          { ssid: '竹林小筑', strength: 82, signalDbm: -41, security: 'psk', known: true, connected: true },
          { ssid: '茶寮-5G', strength: 71, signalDbm: -47, security: 'psk', known: false, connected: false },
          { ssid: '听雨轩', strength: 52, signalDbm: -59, security: 'psk', known: true, connected: false },
          { ssid: 'Loong-Guest', strength: 44, signalDbm: -64, security: 'open', known: false, connected: false },
          { ssid: '山居书屋', strength: 23, signalDbm: -76, security: 'psk', known: false, connected: false },
          { ssid: '书院（教职工）', strength: 18, signalDbm: -79, security: '8021x', known: false, connected: false },
        ];
    const wifiOff = scenario === 'wifi-off';
    if (wifiOff) nets.forEach((n) => (n.connected = false));
    const wifi = new Service({
      available: !desktop,
      enabled: !desktop && !wifiOff,
      state: desktop ? 'unavailable' : wifiOff ? 'off' : 'connected',
      scanning: false,
      ssid: desktop || wifiOff ? '' : '竹林小筑',
      strength: desktop || wifiOff ? 0 : 82,
    });
    const networksChanged = () => wifi.dispatchEvent(new CustomEvent('networkschanged', { detail: {} }));
    const requireOn = () => {
      if (!wifi._props.available) throw err('Unavailable', 'iwd 没有运行');
      if (!wifi._props.enabled) throw err('Failed', 'Wi‑Fi 已关闭');
    };
    wifi.getNetworks = async () => {
      await delay(10);
      return wifi._props.enabled ? nets.map((n) => ({ ...n })).sort((a, b) => b.strength - a.strength) : [];
    };
    wifi.setEnabled = async (enabled) => {
      if (!wifi._props.available) throw err('Unavailable', 'iwd 没有运行');
      await delay(150);
      if (!enabled) {
        nets.forEach((n) => (n.connected = false));
        wifi._change({ enabled: false, state: 'off', ssid: '', strength: 0 });
        networksChanged();
        return;
      }
      wifi._change({ enabled: true, state: 'disconnected' });
      networksChanged();
      // 打开后自动加入信号最强的已保存网络。
      setTimeout(() => {
        const best = nets.filter((n) => n.known).sort((a, b) => b.strength - a.strength)[0];
        if (!best || !wifi._props.enabled) return;
        best.connected = true;
        wifi._change({ state: 'connected', ssid: best.ssid, strength: best.strength });
        networksChanged();
      }, 1200);
    };
    wifi.scan = async () => {
      requireOn();
      if (wifi._props.scanning) return;
      wifi._change({ scanning: true });
      setTimeout(() => {
        for (const n of nets) n.strength = Math.max(5, Math.min(99, n.strength + Math.round(Math.random() * 6 - 3)));
        wifi._change({ scanning: false });
        networksChanged();
      }, 1800);
    };
    wifi.connect = async (ssid, passphrase = '') => {
      requireOn();
      const net = nets.find((n) => n.ssid === ssid);
      if (!net) throw err('NotFound', `附近没有名为 “${ssid}” 的网络`);
      if (net.security === 'wep') throw err('NotSupported', '不支持 WEP 网络');
      if (net.security === '8021x' && !net.known) throw err('NotSupported', '暂不支持连接企业网络（802.1X）');
      if (net.security === 'psk' && !net.known && !passphrase) throw err('InvalidArgs', `连接 “${ssid}” 需要密码`);
      const prev = nets.find((n) => n.connected);
      if (prev) prev.connected = false;
      wifi._change({ state: 'connecting', ssid: '', strength: 0 });
      await delay(1400);
      if (net.security === 'psk' && passphrase && passphrase !== PASSWORD) {
        wifi._change({ state: 'disconnected' });
        networksChanged();
        throw err('Failed', '连接失败：Operation failed');
      }
      net.connected = true;
      net.known = true;
      wifi._change({ state: 'connected', ssid: net.ssid, strength: net.strength });
      networksChanged();
    };
    wifi.disconnect = async () => {
      requireOn();
      await delay(200);
      nets.forEach((n) => (n.connected = false));
      wifi._change({ state: 'disconnected', ssid: '', strength: 0 });
      networksChanged();
    };
    wifi.forget = async (ssid) => {
      requireOn();
      const known = nets.filter((n) => n.ssid === ssid && n.known);
      if (!known.length) throw err('NotFound', `没有保存名为 “${ssid}” 的网络`);
      await delay(150);
      for (const n of known) {
        n.known = false;
        if (n.connected) {
          n.connected = false;
          wifi._change({ state: 'disconnected', ssid: '', strength: 0 });
        }
      }
      networksChanged();
    };

    const system = new EventTarget();
    Object.assign(system, { backlight, power, wifi, running: true });

    // —— 外观：只在本页生效（真机上写入系统外观文件，所有应用随之切换） ——
    const appearance = new EventTarget();
    appearance.theme = null;
    appearance.get = async () => {
      appearance.theme ??= matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
      return appearance.theme;
    };
    appearance.set = async (theme) => {
      if (theme !== 'light' && theme !== 'dark') throw new LoongError('org.loong.Bridge.Error.InvalidArgs', '外观只能是 light 或 dark');
      appearance.theme = theme;
      appearance.dispatchEvent(new CustomEvent('change', { detail: { theme } }));
    };

    const GB = 1024 ** 3;
    const device = {
      info: async () => {
        await delay(20);
        return desktop
          ? {
              os: { name: 'Loong OS', prettyName: 'Loong OS 0.1.0', versionId: '0.1.0', imageVersion: '0.1.0', id: 'loong' },
              device: { name: 'QEMU 虚拟平板', size_class: 'regular', screen: { width: 1280, height: 800, scale: 1.0, refresh_hz: 60 } },
              hostname: 'loong',
              kernel: { name: 'Linux', release: '6.18.2-0-virt', machine: 'x86_64' },
              cpu: { count: 4, model: 'QEMU Virtual CPU version 2.5+' },
              memory: { total: 2 * GB },
              storage: { data: { total: 0.5 * GB, free: 0.41 * GB, available: 0.4 * GB }, system: { total: 0.21 * GB, free: 0, available: 0 }, separate: true },
              webkit: '2.54.2',
            }
          : {
              os: { name: 'Loong OS', prettyName: 'Loong OS 0.1.0', versionId: '0.1.0', imageVersion: '0.1.0', id: 'loong' },
              device: { name: '小米平板 5', size_class: 'regular', screen: { width: 2560, height: 1600, diagonal_inch: 11.0, refresh_hz: 120 } },
              hostname: 'loong',
              kernel: { name: 'Linux', release: '6.18.2-sm8150', machine: 'aarch64' },
              cpu: { count: 8, soc: 'Snapdragon SM8150' },
              board: 'Xiaomi Pad 5',
              memory: { total: 5.6 * GB },
              storage: { data: { total: 107.4 * GB, free: 82.1 * GB, available: 82.1 * GB }, system: { total: 0.86 * GB, free: 0, available: 0 }, separate: true },
              webkit: '2.54.2',
            };
      },
    };

    return { version: 1, mock: true, system, appearance, device, LoongError };
  }

  window.LoongMock = { create };
})();
