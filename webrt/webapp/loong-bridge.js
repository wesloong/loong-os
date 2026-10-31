/*
 * window.loong：系统应用的 JS bridge（由 loong-webapp 在 document-start 注入顶层框架）。
 *
 * 只有以 --bridge=system 启动、且页面位于 /usr/share/loong/web/ 下的应用才有这个对象；
 * 每次调用 loong-webapp 还会再核对一次页面地址，并按白名单校验接口、方法与参数。
 * 接口说明见 docs/api/loong-js-bridge.md。
 *
 *   const { backlight, power, wifi } = loong.system;
 *   await backlight.setBrightness(0.6);                 // 默认平滑过渡
 *   const stop = power.watch((p, changed) => render(p)); // 先给全部属性，之后每次变化再调用
 *   const nets = await wifi.getNetworks();              // [{ ssid, strength, signalDbm, security, known, connected }]
 *   try { await wifi.connect('Home', pass); } catch (e) { e.name === 'org.loong.System1.Error.Failed' }
 *   loong.appearance.set('dark');                       // 写入系统外观，所有应用随之切换
 *   const info = await loong.device.info();             // 系统版本、机型、存储空间……
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
(() => {
  'use strict';

  const handler = window.webkit?.messageHandlers?.loong;
  if (!handler || 'loong' in window) return;
  // 先绑定好 postMessage：之后页面脚本改动 webkit.messageHandlers 也不影响 bridge。
  const post = handler.postMessage.bind(handler);

  /** bridge 调用失败：name 是 D-Bus 错误名（例如 org.loong.System1.Error.Failed），message 是给人看的中文说明。 */
  class LoongError extends Error {
    constructor(name, message) {
      super(message);
      Object.defineProperty(this, 'name', { value: name, enumerable: false, configurable: true, writable: true });
    }
  }

  async function request(message) {
    const reply = await post(message);
    if (reply && reply.ok) return reply.value;
    const e = (reply && reply.error) || {};
    throw new LoongError(e.name || 'org.loong.Bridge.Error.Failed', e.message || '未知错误');
  }

  // D-Bus 属性名（PascalCase）→ JS 属性名（camelCase）：TimeToEmpty → timeToEmpty。
  const camel = (key) => key.charAt(0).toLowerCase() + key.slice(1);
  const camelize = (object) => Object.fromEntries(Object.entries(object || {}).map(([k, v]) => [camel(k), v]));

  /** 接口名 → 接收 bridge 事件的函数。 */
  const receivers = new Map();

  /**
   * 一个 loong-sysd 接口（Backlight / Power / Wifi）。
   * 事件：'change'（detail 为变化了的属性）；Power 另有 'lowbattery'，Wifi 另有 'networkschanged'。
   */
  class Service extends EventTarget {
    #name;
    #props = null;
    #watchers = new Set();
    #loading = null;

    constructor(name) {
      super();
      this.#name = name;
      receivers.set(name, (event) => this.#receive(event));
    }

    /** 读取全部属性（camelCase）。 */
    async getAll() {
      this.#loading ??= request({ op: 'getAll', iface: this.#name })
        .then((props) => (this.#props = camelize(props)))
        .finally(() => (this.#loading = null));
      return { ...(await this.#loading) };
    }

    /** 最近一次读到的属性（还没读过时为 null）。 */
    get props() {
      return this.#props && { ...this.#props };
    }

    /**
     * 订阅属性：先读一次全部属性并回调 callback(props, null)，之后每次变化回调 callback(props, changedKeys)。
     * 返回取消订阅的函数。
     */
    watch(callback) {
      this.#watchers.add(callback);
      this.getAll().then(
        (props) => this.#watchers.has(callback) && callback(props, null),
        (e) => console.warn(`loong.system: 读取 ${this.#name} 失败：${e.name}: ${e.message}`),
      );
      return () => this.#watchers.delete(callback);
    }

    call(method, ...args) {
      return request({ op: 'call', iface: this.#name, method, args });
    }

    #notify(changed) {
      const props = this.props;
      for (const cb of this.#watchers) {
        try {
          cb(props, changed);
        } catch (e) {
          console.error(e);
        }
      }
    }

    async #receive(event) {
      switch (event.signal) {
        case 'PropertiesChanged': {
          const changed = camelize(event.args);
          if (this.#props) Object.assign(this.#props, changed);
          if (event.invalidated?.length || (this.#props === null && this.#watchers.size)) await this.getAll().catch(() => {});
          this.dispatchEvent(new CustomEvent('change', { detail: changed }));
          if (this.#props) this.#notify(Object.keys(changed));
          break;
        }
        case 'Reload': // loong-sysd 重新启动：重新读取全部属性
          if (this.#props || this.#watchers.size) {
            const props = await this.getAll().catch(() => null);
            if (props) {
              this.dispatchEvent(new CustomEvent('change', { detail: props }));
              this.#notify(Object.keys(props));
            }
          }
          break;
        default:
          this.dispatchEvent(new CustomEvent(event.signal.toLowerCase(), { detail: event.args || {} }));
      }
    }
  }

  class Backlight extends Service {
    /** 设置亮度（0–1）。animate=true（默认）时平滑过渡，拖动滑块时可以连续调用。 */
    setBrightness(value, { animate = true } = {}) {
      return this.call('SetBrightness', Number(value), Boolean(animate));
    }
  }

  class Power extends Service {
    /** 立即重新读取电池状态（有变化会触发 'change'）。 */
    refresh() {
      return this.call('Refresh');
    }
  }

  class Wifi extends Service {
    /** 附近的网络，按信号从强到弱。 */
    async getNetworks() {
      const list = await this.call('GetNetworks');
      return (list?.[0] || []).map(([ssid, strength, signalDbm, security, known, connected]) => ({
        ssid,
        strength,
        signalDbm,
        security,
        known,
        connected,
      }));
    }
    setEnabled(enabled) {
      return this.call('SetEnabled', Boolean(enabled));
    }
    scan() {
      return this.call('Scan');
    }
    /** 连接网络，连上或失败后才返回。开放网络与已保存的网络 passphrase 传空字符串。 */
    connect(ssid, passphrase = '') {
      return this.call('Connect', String(ssid), String(passphrase));
    }
    disconnect() {
      return this.call('Disconnect');
    }
    forget(ssid) {
      return this.call('Forget', String(ssid));
    }
  }

  /** loong.system：loong-sysd 是否在运行（'servicechange' 事件，detail.running）。 */
  class System extends EventTarget {
    backlight = new Backlight('Backlight');
    power = new Power('Power');
    wifi = new Wifi('Wifi');
    running = null;
  }
  const system = new System();
  receivers.set('System', (event) => {
    const running = Boolean(event.args?.running);
    const restarted = running && system.running === false;
    system.running = running;
    system.dispatchEvent(new CustomEvent('servicechange', { detail: { running } }));
    if (restarted) for (const name of ['Backlight', 'Power', 'Wifi']) receivers.get(name)({ signal: 'Reload' });
  });

  /** loong.appearance：系统外观 'light'（宣纸）/ 'dark'（夜墨），事件 'change'（detail.theme）。 */
  class Appearance extends EventTarget {
    theme = null;
    async get() {
      this.theme = (await request({ op: 'appearance.get' })).theme;
      return this.theme;
    }
    async set(theme) {
      await request({ op: 'appearance.set', theme: String(theme) });
      this.theme = theme;
    }
  }
  const appearance = new Appearance();
  receivers.set('Appearance', (event) => {
    appearance.theme = event.args.theme;
    appearance.dispatchEvent(new CustomEvent('change', { detail: { theme: event.args.theme } }));
  });

  const device = Object.freeze({
    /** 本机信息：{ os, device, hostname, kernel, cpu, memory, storage, webkit }，见文档。 */
    info: () => request({ op: 'device.info' }),
  });

  const loong = {
    version: 1,
    system,
    appearance,
    device,
    LoongError,
  };
  // loong-webapp 通过这个函数投递事件（JSON 字符串）。
  Object.defineProperty(loong, Symbol.for('loong.bridge.dispatch'), {
    value(json) {
      let event;
      try {
        event = JSON.parse(json);
      } catch {
        return;
      }
      receivers.get(event.iface)?.(event);
    },
  });
  for (const s of [system.backlight, system.power, system.wifi]) Object.freeze(s);
  Object.defineProperty(window, 'loong', { value: Object.freeze(loong), enumerable: false, configurable: false, writable: false });
})();
