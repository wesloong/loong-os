# Loong OS：按目标调整优化级别（由 APKBUILD 通过 -DCMAKE_PROJECT_INCLUDE=… 加载）。
#
# 整体用 -Os 编译（体积优先），但 LOONG_O2_TARGETS 里列出的目标（JavaScriptCore、WebCoreStyle 等热点，
# 可以是 WebKit 的子目标）改用 -O2。目标级 COMPILE_OPTIONS 在命令行上排在 CMAKE_<LANG>_FLAGS 之后，所以 -O2 生效；
# 子目标（如 JavaScriptCoreJIT）继承父目标的选项。
#
# CMAKE_PROJECT_INCLUDE 在每次 project() 之后加载，此时目标还没有定义；因此只在顶层目录登记一个延迟调用，
# 等顶层 CMakeLists.txt 处理完（所有子目录、所有目标都已定义）再设置。不改 WebKit 源码，升级时无需维护补丁。

if (NOT CMAKE_CURRENT_SOURCE_DIR STREQUAL CMAKE_SOURCE_DIR)
    return()
endif ()

get_property(_loong_opt_registered GLOBAL PROPERTY LOONG_OPT_TARGETS_REGISTERED)
if (_loong_opt_registered)
    return()
endif ()
set_property(GLOBAL PROPERTY LOONG_OPT_TARGETS_REGISTERED TRUE)

function(loong_apply_opt_targets)
    foreach (_target IN LISTS LOONG_O2_TARGETS)
        if (TARGET ${_target})
            set_property(TARGET ${_target} APPEND PROPERTY COMPILE_OPTIONS "$<$<COMPILE_LANGUAGE:C,CXX>:-O2>")
            message(STATUS "Loong: ${_target} 用 -O2 编译")
        else ()
            # 目标改名或被功能开关去掉时及早发现，而不是悄悄退回 -Os
            message(FATAL_ERROR "Loong: LOONG_O2_TARGETS 里的目标 ${_target} 不存在")
        endif ()
    endforeach ()
endfunction()

cmake_language(DEFER CALL loong_apply_opt_targets)
