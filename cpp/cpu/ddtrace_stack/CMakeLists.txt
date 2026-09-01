cmake_minimum_required(VERSION 3.19)

# Vendored CPU profiler: dd-trace-py's `stack` sampler (echion-based wall-clock
# sampler). See VENDOR.md for the upstream release and the list of local
# modifications.
#
# No part of dd_wrapper / libdatadog is vendored; see shim/ and
# src/pyroscope_renderer.cpp.
project(pyroscope_cpu_ddtrace)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

add_compile_options(-fPIC -fvisibility=hidden -pthread -Wall -Wextra)

# Deliberately NOT defining _POSIX_C_SOURCE here (unlike cpp/CMakeLists.txt).
# Upstream dd-trace-py does not define it, and on macOS it selects the strict
# POSIX namespace, which hides BSD extensions the vendored code uses --
# echion/danger.cc calls getpagesize().

# Required by the vendored sources, mirroring dd-trace-py's own
# stack/CMakeLists.txt. Without PL_LINUX / PL_DARWIN, echion's danger.h
# compiles out its platform memory-copy primitives (copy_type, proc_ref_t,
# safe_memcpy_return_t) and everything downstream fails to resolve.
#
# UNWIND_NATIVE_DISABLE: we profile Python frames only. Native unwinding would
# pull in libunwind and change what is being measured.
add_compile_definitions(UNWIND_NATIVE_DISABLE)

if (APPLE)
    # rust/build.rs does not build this project on macOS today (thread
    # auto-registration is Linux-only; see the TODO(macos) in
    # src/echion/threads.cc). The branch is kept so the tree still configures
    # if someone builds it by hand while working on that.
    add_compile_definitions(_DARWIN_C_SOURCE PL_DARWIN)
else ()
    add_compile_definitions(PL_LINUX)
endif ()

find_package(Python3 COMPONENTS Interpreter Development)
if (NOT Python3_INCLUDE_DIRS)
    message(FATAL_ERROR "Python3_INCLUDE_DIRS not found")
endif ()
message(STATUS "pyroscope_cpu_ddtrace: Python3_VERSION ${Python3_VERSION}")

file(GLOB ECHION_SOURCES ${CMAKE_CURRENT_SOURCE_DIR}/src/echion/*.cc)

set(SOURCE_FILES
        ${ECHION_SOURCES}
        src/sampler.cpp
        src/thread_span_links.cpp
        src/origin_task_links.cpp
        src/pyroscope_renderer.cpp
        src/pyroscope_entry.cpp)

set(TARGET_NAME pyroscope_cpu_ddtrace)
add_library(${TARGET_NAME} STATIC ${SOURCE_FILES})

# No bundling step (unlike cpp/CMakeLists.txt, which has to merge Abseil):
# this project has no transitive static dependencies, so the archive cmake
# produces is already complete.
target_include_directories(
        ${TARGET_NAME}
        PRIVATE ${Python3_INCLUDE_DIRS}
        ${CMAKE_CURRENT_SOURCE_DIR}
        ${CMAKE_CURRENT_SOURCE_DIR}/include
        ${CMAKE_CURRENT_SOURCE_DIR}/include/util
        ${CMAKE_CURRENT_SOURCE_DIR}/echion
        ${CMAKE_CURRENT_SOURCE_DIR}/shim
        ${CMAKE_CURRENT_SOURCE_DIR}/..          # PyroscopeCpu.h
        # profiling_helpers/ is NOT vendored twice: src/echion/frame.cc includes
        # <profiling_helpers/frame_accessors.h>, and cpp/profiling_helpers/ (used
        # by the memalloc profiler) is byte-identical to the upstream copy. One
        # definition on disk, so the two archives cannot drift into an ODR
        # violation on those `namespace DataDog` inline functions.
        ${CMAKE_CURRENT_SOURCE_DIR}/../..
        ${CMAKE_CURRENT_SOURCE_DIR}/../../../rust/include  # pyroscope_ffi.h
)

# Python modules should use -undefined dynamic_lookup on macOS and not link to
# libpython on Linux, matching cpp/CMakeLists.txt.
if (TARGET Python3::Python AND NOT APPLE)
    target_link_libraries(${TARGET_NAME} PRIVATE Python3::Python)
endif ()

install(TARGETS ${TARGET_NAME}
        ARCHIVE DESTINATION .
        LIBRARY DESTINATION .)
