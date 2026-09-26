cmake_minimum_required(VERSION 3.16)
project(micyou-qt-frontend VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)

find_package(Qt6 REQUIRED COMPONENTS Widgets)

qt_add_executable(micyou-qt-frontend
  src/main.cpp
  src/session.h src/session.cpp
  src/mainwindow.h src/mainwindow.cpp
)
target_link_libraries(micyou-qt-frontend PRIVATE Qt6::Widgets)

if(WIN32)
  set_target_properties(micyou-qt-frontend PROPERTIES WIN32_EXECUTABLE TRUE)
elseif(APPLE)
  set_target_properties(micyou-qt-frontend PROPERTIES
    MACOSX_BUNDLE TRUE
    MACOSX_BUNDLE_GUI_IDENTIFIER "dev.libmicyou.frontend.qt")
endif()
