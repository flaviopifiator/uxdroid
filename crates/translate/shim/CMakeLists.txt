# Builds libuxdroid_dynarmic.so: Dynarmic's A64 JIT behind the C interface of dynarmic_shim.cpp.
# Run by docker/translation/Dockerfile with DYNARMIC_SOURCE pointing at the pinned Dynarmic tree.
cmake_minimum_required(VERSION 3.12)
project(uxdroid_dynarmic CXX)

set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(DYNARMIC_FRONTENDS "A64" CACHE STRING "" FORCE)
set(DYNARMIC_TESTS OFF CACHE BOOL "" FORCE)
set(DYNARMIC_USE_BUNDLED_EXTERNALS ON CACHE BOOL "" FORCE)
set(DYNARMIC_WARNINGS_AS_ERRORS OFF CACHE BOOL "" FORCE)
add_subdirectory(${DYNARMIC_SOURCE} dynarmic EXCLUDE_FROM_ALL)

add_library(uxdroid_dynarmic SHARED dynarmic_shim.cpp)
target_compile_features(uxdroid_dynarmic PRIVATE cxx_std_20)
target_compile_options(uxdroid_dynarmic PRIVATE -fvisibility=hidden -mcx16)
target_link_libraries(uxdroid_dynarmic PRIVATE dynarmic)
# Nothing but the uxd_ functions leaves the library, so its C++ runtime never meets the process'.
target_link_options(uxdroid_dynarmic PRIVATE -static-libstdc++ -static-libgcc
    -Wl,--exclude-libs,ALL -Wl,-z,now -Wl,-z,relro)
