cmake_minimum_required(VERSION 3.19)

project(gcp_cpu_profiler LANGUAGES CXX)

if(NOT CMAKE_SYSTEM_NAME STREQUAL "Linux")
  message(FATAL_ERROR "gcp_cpu_profiler is supported only on Linux")
endif()

set(CMAKE_CXX_STANDARD 11)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

find_package(Python3 REQUIRED COMPONENTS Interpreter)
set(GCP_PYTHON_VERSION "${Python3_VERSION}")
find_package(
  Python3 "${GCP_PYTHON_VERSION}" EXACT REQUIRED
  COMPONENTS Development.Module
)
find_package(Threads REQUIRED)

add_library(
  gcp_cpu_profiler STATIC
  bridge.cc
  clock.cc
  log.cc
  populate_frames.cc
  profiler.cc
  stacktraces.cc
)

set_target_properties(
  gcp_cpu_profiler
  PROPERTIES
    POSITION_INDEPENDENT_CODE ON
    CXX_VISIBILITY_PRESET hidden
    VISIBILITY_INLINES_HIDDEN ON
)

target_include_directories(
  gcp_cpu_profiler
  PRIVATE
    "${CMAKE_CURRENT_SOURCE_DIR}"
    "${CMAKE_CURRENT_SOURCE_DIR}/../rust/include"
)

target_link_libraries(
  gcp_cpu_profiler
  PUBLIC
    Python3::Module
    Threads::Threads
)

install(TARGETS gcp_cpu_profiler ARCHIVE DESTINATION .)
