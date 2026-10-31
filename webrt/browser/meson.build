# loong-browser：系统浏览器（界面 + 网页内容同屏，见 webrt/platform/）
# 界面文件安装到 <datadir>/loong/web/browser/ui/，与 LoongKit（<datadir>/loong/web/loongkit/）的相对位置
# 和源码树（webrt/browser/ui/ 与 webrt/loongkit/）一致。

browser_ui_dir = get_option('prefix') / get_option('datadir') / 'loong' / 'web' / 'browser' / 'ui'

executable('loong-browser',
  'loong-browser.c',
  c_args: ['-DLOONG_BROWSER_UI_DIR="@0@"'.format(browser_ui_dir)],
  dependencies: [wpe_webkit, loong_platform_dep, dependency('gio-unix-2.0')],
  install: true,
)

install_subdir('ui', install_dir: get_option('datadir') / 'loong' / 'web' / 'browser')

# 测试工具：常驻的虚拟指针、虚拟键盘与输入法（只在 wlroots 合成器上用，见 tests/sway-run.sh），不安装。
vinput_sources = []
foreach xml : ['tests/protocols/wlr-virtual-pointer-unstable-v1.xml', 'tests/protocols/virtual-keyboard-unstable-v1.xml',
            'tests/protocols/input-method-unstable-v2.xml']
  vinput_sources += custom_target(xml.split('/')[-1] + '-code', input: xml, output: '@BASENAME@-protocol.c',
    command: [wayland_scanner, 'private-code', '@INPUT@', '@OUTPUT@'])
  vinput_sources += custom_target(xml.split('/')[-1] + '-header', input: xml, output: '@BASENAME@-client-protocol.h',
    command: [wayland_scanner, 'client-header', '@INPUT@', '@OUTPUT@'])
endforeach
executable('loong-vinput', 'tests/loong-vinput.c', vinput_sources,
  dependencies: [wayland_client, xkbcommon],
  install: false,
)
