project('loong-webrt', 'c',
  version: '0.1.0',
  license: 'MIT OR Apache-2.0',
  meson_version: '>= 1.0',
  default_options: ['c_std=gnu17', 'warning_level=2', 'buildtype=release', 'b_ndebug=if-release'],
)

gnome = import('gnome')

wpe_webkit = dependency('wpe-webkit-2.0', version: '>= 2.54')
wpe_platform = dependency('wpe-platform-2.0', version: '>= 2.54')
# WPE 自带 Wayland 平台的公开接口（wl_display、窗口的 wl_surface），loong-webapp 用它接上私有协议。
wpe_platform_wayland = dependency('wpe-platform-wayland-2.0', version: '>= 2.54')
gio = dependency('gio-2.0', version: '>= 2.74')
wayland_client = dependency('wayland-client', version: '>= 1.22')
wayland_scanner = find_program(dependency('wayland-scanner', native: true).get_variable('wayland_scanner'))

# Loong 私有协议 loong_app_v1（状态栏样式、主题色）。XML 在仓库顶层 protocols/，loong-shell 用同一份
# 生成服务端代码；这里生成客户端代码，loong-webapp 与 loong 平台后端（浏览器）共用。
loong_app_xml = meson.project_source_root() / '..' / 'protocols' / 'loong-app-v1.xml'
loong_app_protocol = [
  custom_target('loong-app-v1-code',
    input: loong_app_xml,
    output: 'loong-app-v1-protocol.c',
    command: [wayland_scanner, 'private-code', '@INPUT@', '@OUTPUT@'],
  ),
  custom_target('loong-app-v1-header',
    input: loong_app_xml,
    output: 'loong-app-v1-client-protocol.h',
    command: [wayland_scanner, 'client-header', '@INPUT@', '@OUTPUT@'],
  ),
]

add_project_arguments('-Wno-unused-parameter', '-DG_LOG_DOMAIN="loong-webrt"', language: 'c')

web_root = get_option('web_root')
if not web_root.startswith('/')
  error('web_root 必须是绝对路径')
endif

# JS bridge 的纯逻辑（白名单、参数校验、JSON、URL 校验、配置解析），单元测试也链接它。
bridge_core = static_library('loong-bridge-core',
  'webapp/loong-bridge-core.c',
  dependencies: [wpe_webkit, gio],
)

resources = gnome.compile_resources('loong-webapp-resources',
  'webapp/loong-webapp.gresource.xml',
  source_dir: 'webapp',
  c_name: 'loong_webapp',
)

executable('loong-webapp',
  'webapp/loong-webapp.c',
  'webapp/loong-bridge.c',
  'webapp/loong-appearance.c',
  'webapp/loong-app-surface.c',
  loong_app_protocol,
  resources,
  c_args: ['-DLOONG_WEB_ROOT="@0@"'.format(web_root)],
  link_with: bridge_core,
  dependencies: [wpe_webkit, wpe_platform, wpe_platform_wayland, wayland_client, gio],
  install: true,
)

test('bridge-core',
  executable('test-bridge-core',
    'tests/test-bridge-core.c',
    link_with: bridge_core,
    dependencies: [wpe_webkit, gio],
    include_directories: include_directories('webapp'),
  ),
)

# 开发用的假 iwd（不安装）：没有无线网卡时也能跑通 设置 → bridge → loong-sysd → iwd，
# 见 webrt/tests/settings-headless.sh（FAKE_IWD=1）。
executable('fake-iwd', 'tests/fake-iwd.c', dependencies: gio)

# loong WPEPlatform 后端（每个 view 一个 wl_subsurface）与系统浏览器
subdir('platform')
subdir('browser')
