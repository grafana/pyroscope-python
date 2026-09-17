cmake_minimum_required(VERSION 3.19)

project(_memalloc)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

add_compile_options(-fPIC -fvisibility=hidden -pthread -Wall -Wextra)

add_compile_definitions(_POSIX_C_SOURCE=200809L)

# Pyroscope patch: Abseil is gone. It was fetched from GitHub at build time
# purely to back the heap tracker's map with absl::flat_hash_map; that tracker
# now lives in Rust and uses hashbrown, which is already a dependency. With it
# go FetchContent, the position-independent-code and testing toggles it needed,
# the _DARWIN_C_SOURCE definition it required on newer macOS SDKs, and
# BundleStaticLibrary.cmake, which existed only to merge absl's archives into
# ours because the Rust link step cannot follow transitive CMake dependencies.

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

# Pyroscope patch: only the allocator hooks are left here. Everything they
# used to call -- the heap tracker, traceback capture, frame walking, the
# reentrancy guard -- is Rust now, reached through pyroscope_ffi.h.
set(SOURCE_FILES _memalloc.cpp)

set(FULL_EXTENSION_NAME "datadog_mem_profiler")

add_library(${FULL_EXTENSION_NAME} STATIC ${SOURCE_FILES})

# Set output directory if specified
if (DEFINED LIB_INSTALL_DIR)
    set_target_properties(${FULL_EXTENSION_NAME} PROPERTIES LIBRARY_OUTPUT_DIRECTORY ${LIB_INSTALL_DIR})
endif ()

# Include directories
target_include_directories(
        ${FULL_EXTENSION_NAME}
        PRIVATE ${Python3_INCLUDE_DIRS}
        ${CMAKE_CURRENT_SOURCE_DIR}
        ${CMAKE_CURRENT_SOURCE_DIR}/../rust/include
)

# Link libraries Python3::Python target might not exist in all build environments (e.g. manylinux) Python modules
# should use -undefined dynamic_lookup on macOS and not link to libpython on Linux
if (TARGET Python3::Python AND NOT APPLE)
    target_link_libraries(${FULL_EXTENSION_NAME} PRIVATE Python3::Python)
endif ()

# Pyroscope patch: the cmake crate drives the `install` target, which used to
# exist only because Abseil and BundleStaticLibrary.cmake brought install rules
# with them. Install the archive straight into the install prefix, which is
# where build.rs points rustc-link-search.
install(TARGETS ${FULL_EXTENSION_NAME} ARCHIVE DESTINATION .)
