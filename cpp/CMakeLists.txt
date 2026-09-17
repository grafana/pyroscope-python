cmake_minimum_required(VERSION 3.19)
include(CheckIPOSupported)
include(FetchContent)

project(_memalloc)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Add compile options
add_compile_options(-fPIC -fvisibility=hidden -pthread -Wall -Wextra)

# Pyroscope patch: deliberately NO _POSIX_C_SOURCE / _DARWIN_C_SOURCE, unlike
# dd-trace-py's memalloc CMakeLists that this file was derived from. Do not
# restore them on a vendor sync -- they are a matched pair of a mistake and its
# workaround:
#
#   * _POSIX_C_SOURCE=200809L entered dd-trace-py in #11460 as the companion to
#     -std=c11, so a strict-ISO build could see pthread_mutexattr_settype and
#     PTHREAD_MUTEX_ERRORCHECK in the then-new mutex-based memalloc guard. That
#     guard is gone (it is an RAII type now) and CMake emits -std=gnu++20, which
#     is not strict ISO, so nothing needs the macro. On Linux it is a no-op
#     anyway: g++ predefines _GNU_SOURCE for every C++ TU.
#   * On macOS it is actively harmful. Darwin's sys/cdefs.h pins __DARWIN_C_LEVEL
#     to _POSIX_C_SOURCE unless _DARWIN_C_SOURCE is also set, which hides the BSD
#     types and macros that Abseil (u_int, MAP_ANONYMOUS) and echion
#     (getpagesize) rely on. _DARWIN_C_SOURCE existed only to cancel that.
#
# Dropping both restores the platform's default header exposure, which is what
# Abseil, echion, and dd-trace-py's own stack extension all expect. If some SDK
# ever does need _DARWIN_C_SOURCE, it is harmless on its own -- but never bring
# back _POSIX_C_SOURCE with it.

# Abseil: HeapMapType is backed by absl::flat_hash_map (Swiss Tables). absl is built as
# static libraries (no BUILD_SHARED_LIBS), position-independent so it can be merged into
# the final shared Python extension. Tag matches dd-trace-py's pinned version.
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(ABSL_PROPAGATE_CXX_STD ON CACHE BOOL "" FORCE)
set(BUILD_TESTING OFF CACHE BOOL "" FORCE)
set(ABSL_BUILD_TESTING OFF CACHE BOOL "" FORCE)
set(ABSL_USE_GOOGLETEST_HEAD OFF CACHE BOOL "" FORCE)
set(ABSL_ENABLE_INSTALL ON CACHE BOOL "" FORCE)
set(FETCHCONTENT_UPDATES_DISCONNECTED ON CACHE BOOL "" FORCE)
FetchContent_Declare(
        absl
        GIT_REPOSITORY https://github.com/abseil/abseil-cpp.git
        GIT_TAG 20250127.1
        GIT_SHALLOW TRUE
        GIT_PROGRESS TRUE)
FetchContent_MakeAvailable(absl)

# Find Python (be flexible about what's available in build environments)
find_package(Python3 COMPONENTS Interpreter Development)

# Make sure we have necessary Python variables
if (NOT Python3_INCLUDE_DIRS)
    message(FATAL_ERROR "Python3_INCLUDE_DIRS not found")
endif ()

message(STATUS "Python3_EXECUTABLE ${Python3_EXECUTABLE}")
message(STATUS "Python3_VERSION ${Python3_VERSION}")
message(STATUS "Python3_INCLUDE_DIRS ${Python3_INCLUDE_DIRS}")

# Python3::Python target might not exist in all build environments so we'll link using include dirs and let the linker
# find Python dynamically
if (NOT TARGET Python3::Python)
    message(STATUS "Python3::Python target not found, using include dirs only")
endif ()

# The memory profiler and the vendored echion stack sampler are two separate
# extensions upstream, with deliberately different flags. They are compiled here
# as two OBJECT libraries so each gets only what it should, then archived into a
# single static library for the Rust build to link.
set(MEMALLOC_SOURCES
        memalloc/_memalloc.cpp
        memalloc/_memalloc_tb.cpp
        memalloc/_memalloc_heap.cpp
        memalloc/_memalloc_reentrant.cpp
)

set(STACK_SOURCES
        # Pyroscope patch: the dd_wrapper half. Upstream builds these into a
        # separate shared libdd_wrapper that stack_v2 links against; we archive
        # them straight into the stack object library, since the CPU sampler is
        # their only consumer (see cpp/dd_wrapper/include/profiler_state.hpp).
        dd_wrapper/src/native_call_tracker.cpp
        dd_wrapper/src/profiler_state.cpp
        stack/src/echion/danger.cc
        stack/src/echion/frame.cc
        stack/src/echion/greenlets.cc
        stack/src/echion/interp.cc
        stack/src/echion/long.cc
        stack/src/echion/mirrors.cc
        stack/src/echion/stack_chunk.cc
        stack/src/echion/stacks.cc
        stack/src/echion/strings.cc
        stack/src/echion/tasks.cc
        stack/src/echion/threads.cc
        stack/src/echion/vm.cc
        stack/src/sampler.cpp
        stack/src/stack.cpp
        stack/src/stack_renderer.cpp
        stack/src/thread_span_links.cpp
)

set(FULL_EXTENSION_NAME "datadog_mem_profiler")

# Include directories both halves need. Python's headers are marked SYSTEM so
# CPython's own internals stop warning at us (3.14's pycore_stackref.h trips
# -Wunused-parameter); upstream marks them SYSTEM for the same reason.
add_library(pyroscope_common INTERFACE)
target_include_directories(pyroscope_common INTERFACE
        ${CMAKE_CURRENT_SOURCE_DIR}
        ${CMAKE_CURRENT_SOURCE_DIR}/../rust/include
        ${CMAKE_CURRENT_SOURCE_DIR}/pyroscope
)
target_include_directories(pyroscope_common SYSTEM INTERFACE ${Python3_INCLUDE_DIRS})

# Memory profiler half. Carries no compile definitions: it has no use for
# echion's PL_* platform macros, and see the note above re: feature macros.
# absl is linked for its include directories; HeapMapType is the only consumer.
add_library(pyroscope_memalloc OBJECT ${MEMALLOC_SOURCES})
target_link_libraries(pyroscope_memalloc PRIVATE pyroscope_common absl::flat_hash_map)

# Vendored echion stack sampler half.
add_library(pyroscope_stack OBJECT ${STACK_SOURCES})
target_link_libraries(pyroscope_stack PRIVATE pyroscope_common)
target_include_directories(pyroscope_stack PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/stack/include)
# The quoted "dd_wrapper/include/..." includes in cpp/stack resolve through
# pyroscope_common's ${CMAKE_CURRENT_SOURCE_DIR}; this dir is needed only for
# stack_renderer.cpp's angle-bracket <ddup_interface.hpp>, which upstream picks
# up from the imported dd_wrapper target's INTERFACE_INCLUDE_DIRECTORIES.
target_include_directories(pyroscope_stack PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/dd_wrapper/include)
target_include_directories(pyroscope_stack SYSTEM PRIVATE
        ${CMAKE_CURRENT_SOURCE_DIR}/stack/echion
        # include/util holds cast_to_pyfunc.hpp, whose whole job is a cast that
        # -Wold-style-cast and -Wcast-function-type-mismatch both object to.
        # Upstream marks this dir SYSTEM for exactly that reason.
        ${CMAKE_CURRENT_SOURCE_DIR}/stack/include/util
)
# UNWIND_NATIVE_DISABLE is upstream fidelity only: no code in dd-trace-py's stack
# tree references it, it is a leftover from an older echion.
target_compile_definitions(pyroscope_stack PRIVATE
        UNWIND_NATIVE_DISABLE
        $<IF:$<BOOL:${APPLE}>,PL_DARWIN,PL_LINUX>
)
# Upstream's add_ddup_config warning set, which it applies to stack/ddup but NOT
# to memalloc -- so neither do we, since memalloc's vendored sources have never
# been vetted against -Wold-style-cast. Omitted on purpose: -ffunction-sections
# (the cmake crate already adds it) and the -Os/LTO/linker options, which are
# the cmake crate's business here.
#
# -Werror is also omitted, but no longer because cpp/stack fails to build -- it
# now compiles warning-free under both Apple clang and gcc 13 with the flags
# below. The wheels are built in manylinux and musllinux images on older gcc
# releases whose diagnostics we have not seen, and a warning we cannot reproduce
# locally would hard-fail a release build rather than the dev loop. Turn it on
# once CI has been green across the full image matrix for a while.
target_compile_options(pyroscope_stack PRIVATE -Wshadow -Wnon-virtual-dtor -Wold-style-cast)

# LINKER_LANGUAGE is required: a target whose only sources are TARGET_OBJECTS
# gives CMake nothing to infer the language from.
add_library(${FULL_EXTENSION_NAME} STATIC
        $<TARGET_OBJECTS:pyroscope_memalloc>
        $<TARGET_OBJECTS:pyroscope_stack>
)
set_target_properties(${FULL_EXTENSION_NAME} PROPERTIES LINKER_LANGUAGE CXX)


# Set properties to prevent CMake from adding any prefix or suffix
#set_target_properties(${FULL_EXTENSION_NAME} PROPERTIES PREFIX "" SUFFIX "")

# Set output directory if specified
if (DEFINED LIB_INSTALL_DIR)
    set_target_properties(${FULL_EXTENSION_NAME} PROPERTIES LIBRARY_OUTPUT_DIRECTORY ${LIB_INSTALL_DIR})
endif ()

# Link libraries Python3::Python target might not exist in all build environments (e.g., manylinux) Python modules
# should use -undefined dynamic_lookup on macOS and not link to libpython on Linux
if (TARGET Python3::Python AND NOT APPLE)
    target_link_libraries(${FULL_EXTENSION_NAME} PRIVATE Python3::Python)
endif ()

# Compile against absl::flat_hash_map (brings in include dirs and records absl as a link
# dependency). Since this is a STATIC library, the absl object code is not archived in here;
# we merge it explicitly via bundle_static_library below.
target_link_libraries(${FULL_EXTENSION_NAME} PRIVATE absl::flat_hash_map)

include(${CMAKE_CURRENT_SOURCE_DIR}/BundleStaticLibrary.cmake)

bundle_static_library(${FULL_EXTENSION_NAME} ${FULL_EXTENSION_NAME}_bundled)
