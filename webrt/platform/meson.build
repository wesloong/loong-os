# loong WPEPlatform 后端（静态库）：浏览器在进程内使用，之后 loong-webapp 也会用它。

# wayland_client、wayland_scanner、loong_app_protocol 在上一级 meson.build 中定义。
wayland_protocols = dependency('wayland-protocols', version: '>= 1.32')
xkbcommon = dependency('xkbcommon')
libdrm = dependency('libdrm', required: false)

wl_protocol_dir = wayland_protocols.get_variable('pkgdatadir')
protocol_xmls = [
  wl_protocol_dir / 'stable/xdg-shell/xdg-shell.xml',
  wl_protocol_dir / 'stable/viewporter/viewporter.xml',
  wl_protocol_dir / 'stable/linux-dmabuf/linux-dmabuf-v1.xml',
  wl_protocol_dir / 'staging/fractional-scale/fractional-scale-v1.xml',
  wl_protocol_dir / 'staging/single-pixel-buffer/single-pixel-buffer-v1.xml',
  wl_protocol_dir / 'unstable/text-input/text-input-unstable-v3.xml',
]
protocol_sources = []
protocol_headers = []
foreach xml : protocol_xmls
  protocol_sources += custom_target('@0@-code'.format(xml.split('/')[-1]),
    input: xml,
    output: '@BASENAME@-protocol.c',
    command: [wayland_scanner, 'private-code', '@INPUT@', '@OUTPUT@'],
  )
  protocol_headers += custom_target('@0@-header'.format(xml.split('/')[-1]),
    input: xml,
    output: '@BASENAME@-client-protocol.h',
    command: [wayland_scanner, 'client-header', '@INPUT@', '@OUTPUT@'],
  )
endforeach

loong_platform_args = ['-D_GNU_SOURCE'] # memfd_create
if libdrm.found()
  loong_platform_args += '-DHAVE_LIBDRM'
endif

loong_platform = static_library('loong-platform',
  'loong-display.c',
  'loong-im-context.c',
  'loong-screen.c',
  'loong-seat.c',
  'loong-toplevel.c',
  'loong-view.c',
  protocol_sources,
  protocol_headers,
  loong_app_protocol,
  c_args: loong_platform_args,
  dependencies: [wpe_platform, wayland_client, xkbcommon, libdrm],
)

loong_platform_dep = declare_dependency(
  link_with: loong_platform,
  include_directories: include_directories('.'),
  dependencies: [wpe_platform],
)
