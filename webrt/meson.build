project('loong-webrt', 'c',
  version: '0.1.0',
  license: 'MIT OR Apache-2.0',
  meson_version: '>= 1.0',
  default_options: ['c_std=gnu17', 'warning_level=2', 'buildtype=release', 'b_ndebug=if-release'],
)

gnome = import('gnome')

wpe_webkit = dependency('wpe-webkit-2.0', version: '>= 2.54')
wpe_platform = dependency('wpe-platform-2.0', version: '>= 2.54')
gio = dependency('gio-2.0', version: '>= 2.74')

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
  resources,
  c_args: ['-DLOONG_WEB_ROOT="@0@"'.format(web_root)],
  link_with: bridge_core,
  dependencies: [wpe_webkit, wpe_platform, gio],
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
