project('loong-webrt', 'c',
  version: '0.1.0',
  license: 'MIT OR Apache-2.0',
  meson_version: '>= 1.0',
  default_options: ['c_std=gnu17', 'warning_level=2', 'buildtype=release', 'b_ndebug=if-release'],
)

wpe_webkit = dependency('wpe-webkit-2.0', version: '>= 2.54')
wpe_platform = dependency('wpe-platform-2.0', version: '>= 2.54')

add_project_arguments('-Wno-unused-parameter', '-DG_LOG_DOMAIN="loong-webrt"', language: 'c')

executable('loong-webapp',
  'webapp/loong-webapp.c',
  dependencies: [wpe_webkit, wpe_platform],
  install: true,
)
