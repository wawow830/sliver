cmake_minimum_required(VERSION 3.21)
project(Sliver LANGUAGES C CXX)

find_package(Qt6 6.5 REQUIRED COMPONENTS Gui Qml)
find_package(PkgConfig REQUIRED)
pkg_check_modules(WAYLAND REQUIRED IMPORTED_TARGET wayland-client)
qt_standard_project_setup(REQUIRES 6.5)
set(QT_QML_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/qml)

set(XML ${CMAKE_CURRENT_SOURCE_DIR}/virtual-keyboard-unstable-v1.xml)
add_custom_command(
    OUTPUT virtual-keyboard.h virtual-keyboard.c
    COMMAND wayland-scanner client-header ${XML} virtual-keyboard.h
    COMMAND wayland-scanner private-code ${XML} virtual-keyboard.c
    DEPENDS ${XML})

set_source_files_properties(Theme.qml Desktop.qml Spectrum.qml PROPERTIES QT_QML_SINGLETON_TYPE TRUE)

qt_add_qml_module(sliver
    URI Sliver
    PLUGIN_TARGET sliver
    SOURCES keyboard.h keyboard.cpp virtual-keyboard.h virtual-keyboard.c
    QML_FILES Theme.qml Desktop.qml Spectrum.qml Pill.qml Key.qml Level.qml Brightness.qml Volume.qml Mic.qml
              Workspaces.qml Cover.qml Seek.qml Media.qml FKeys.qml Bar.qml)
target_include_directories(sliver PRIVATE ${CMAKE_CURRENT_BINARY_DIR})
target_link_libraries(sliver PRIVATE Qt6::Gui PkgConfig::WAYLAND)

include(GNUInstallDirs)
install(DIRECTORY ${QT_QML_OUTPUT_DIRECTORY}/Sliver DESTINATION ${CMAKE_INSTALL_LIBDIR}/qt6/qml)
