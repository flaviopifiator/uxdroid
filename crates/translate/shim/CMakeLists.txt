# Builds libuxdroid_dynarmic.so: Dynarmic's A64 JIT behind the C interface of dynarmic_shim.cpp.
# Run by docker/translation/Dockerfile with DYNARMIC_SOURCE pointing at the pinned Dynarmic tree.
cmake_minimum_required(VERSION 3.12)
project(uxdroid_dynarmic CXX)

set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(DYNARMIC_FRONTENDS "A64" CACHE STRING "" FORCE)
set(DYNARMIC_TESTS OFF CACHE BOOL "" FORCE)
set(DYNARMIC_USE_BUNDLED_EXTERNALS ON CACHE BOOL "" FORCE)
set(DYNARMIC_WARNINGS_AS_ERRORS OFF CACHE BOOL "" FORCE)

# uxdroid's change to Dynarmic (0BSD, like Dynarmic itself): one code cache shared by every guest
# CPU of the process, described in shared_code.patch and at the top of dynarmic_shim.cpp. It goes on
# top of docker/translation/dynarmic.patch, once: a tree it already applies to is left as it is.
execute_process(
    COMMAND patch -d ${DYNARMIC_SOURCE} -p1 -R --dry-run -s -f -i ${CMAKE_CURRENT_SOURCE_DIR}/shared_code.patch
    RESULT_VARIABLE SHARED_CODE_APPLIED OUTPUT_QUIET ERROR_QUIET)
if (NOT SHARED_CODE_APPLIED EQUAL 0)
    execute_process(
        COMMAND patch -d ${DYNARMIC_SOURCE} -p1 -s -f -i ${CMAKE_CURRENT_SOURCE_DIR}/shared_code.patch
        RESULT_VARIABLE SHARED_CODE_RESULT)
    if (NOT SHARED_CODE_RESULT EQUAL 0)
        message(FATAL_ERROR "shared_code.patch does not apply to ${DYNARMIC_SOURCE}")
    endif()
endif()

add_subdirectory(${DYNARMIC_SOURCE} dynarmic EXCLUDE_FROM_ALL)

add_library(uxdroid_dynarmic SHARED dynarmic_shim.cpp)
target_compile_features(uxdroid_dynarmic PRIVATE cxx_std_20)
target_compile_options(uxdroid_dynarmic PRIVATE -fvisibility=hidden -mcx16)
target_link_libraries(uxdroid_dynarmic PRIVATE dynarmic)
# Nothing but the uxd_ functions leaves the library, so its C++ runtime never meets the process'.
target_link_options(uxdroid_dynarmic PRIVATE -static-libstdc++ -static-libgcc
    -Wl,--exclude-libs,ALL -Wl,-z,now -Wl,-z,relro)
