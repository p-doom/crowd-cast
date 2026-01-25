cmake_minimum_required(VERSION 3.16...3.26)

project(obs-crowd-cast VERSION 1.0.0)
set(PROJECT_FULL_NAME "OBS crowd-cast Plugin")

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)

set(PLUGIN_AUTHOR "crowd-cast Team")

# Options for CI builds
set(OBS_SOURCE_DIR "" CACHE PATH "Path to OBS source tree (for headers)")
set(OBS_INSTALLED_DIR "" CACHE PATH "Path to installed OBS (Windows)")

# Try to find libobs via CMake package first (works on Linux with libobs-dev)
find_package(libobs QUIET)

if(libobs_FOUND)
    message(STATUS "Found libobs via CMake package")
    set(USE_SYSTEM_LIBOBS TRUE)
elseif(OBS_SOURCE_DIR)
    # CI/Dev build: use OBS source for headers
    message(STATUS "Using OBS source headers from: ${OBS_SOURCE_DIR}")
    set(USE_SYSTEM_LIBOBS FALSE)
    
    if(NOT EXISTS "${OBS_SOURCE_DIR}/libobs/obs.h")
        message(FATAL_ERROR "OBS_SOURCE_DIR specified but libobs/obs.h not found at ${OBS_SOURCE_DIR}/libobs/obs.h")
    endif()
elseif(APPLE)
    # macOS: Check for OBS.app
    set(OBS_APP_FRAMEWORK "/Applications/OBS.app/Contents/Frameworks")
    if(EXISTS "${OBS_APP_FRAMEWORK}/libobs.framework")
        message(STATUS "Found libobs.framework in OBS.app (linking only, need OBS_SOURCE_DIR for headers)")
        message(FATAL_ERROR 
            "On macOS, you need OBS source for headers. Set OBS_SOURCE_DIR:\n"
            "  git clone --depth 1 https://github.com/obsproject/obs-studio.git\n"
            "  cmake -DOBS_SOURCE_DIR=path/to/obs-studio ...")
    else()
        message(FATAL_ERROR "Could not find OBS. Install OBS.app or set OBS_SOURCE_DIR")
    endif()
else()
    message(FATAL_ERROR 
        "Could not find libobs. Options:\n"
        "  Linux: sudo apt install libobs-dev\n"
        "  macOS/Windows: Set OBS_SOURCE_DIR to OBS source tree")
endif()

# Plugin source files (common)
set(PLUGIN_SOURCES src/plugin.c)

# Platform-specific sources
if(APPLE)
    list(APPEND PLUGIN_SOURCES src/platform_macos.m)
    # Enable ARC for Objective-C
    set_source_files_properties(src/platform_macos.m PROPERTIES
        COMPILE_FLAGS "-fobjc-arc"
    )
elseif(WIN32)
    list(APPEND PLUGIN_SOURCES src/platform_windows.c)
else()
    # Linux
    list(APPEND PLUGIN_SOURCES src/platform_linux.c)
    
    # Try to find X11 for active window detection
    find_package(X11 QUIET)
    if(X11_FOUND)
        message(STATUS "X11 found - enabling frontmost app detection for X11")
    else()
        message(STATUS "X11 not found - frontmost app detection disabled (Wayland fallback only)")
    endif()
endif()

add_library(${PROJECT_NAME} MODULE ${PLUGIN_SOURCES})

# Include directories
if(USE_SYSTEM_LIBOBS)
    target_include_directories(${PROJECT_NAME} PRIVATE
        ${CMAKE_SOURCE_DIR}/src
        ${CMAKE_SOURCE_DIR}/deps
    )
else()
    target_include_directories(${PROJECT_NAME} PRIVATE
        ${CMAKE_SOURCE_DIR}/src
        ${CMAKE_SOURCE_DIR}/deps
        ${OBS_SOURCE_DIR}/libobs
        ${OBS_SOURCE_DIR}
        ${OBS_SOURCE_DIR}/UI/obs-frontend-api
    )
endif()

# Linking - libobs
if(USE_SYSTEM_LIBOBS)
    target_link_libraries(${PROJECT_NAME} OBS::libobs)
elseif(APPLE)
    # macOS: link to framework in OBS.app
    set(OBS_APP_FRAMEWORK "/Applications/OBS.app/Contents/Frameworks")
    target_link_libraries(${PROJECT_NAME}
        "${OBS_APP_FRAMEWORK}/libobs.framework/libobs"
    )
    # Add obs-frontend-api if it exists
    if(EXISTS "${OBS_APP_FRAMEWORK}/obs-frontend-api.dylib")
        target_link_libraries(${PROJECT_NAME}
            "${OBS_APP_FRAMEWORK}/obs-frontend-api.dylib"
        )
    endif()
elseif(WIN32)
    # Windows: link to installed OBS
    if(OBS_INSTALLED_DIR)
        find_library(LIBOBS_LIB obs PATHS "${OBS_INSTALLED_DIR}/bin/64bit" "${OBS_INSTALLED_DIR}/lib")
        if(LIBOBS_LIB)
            target_link_libraries(${PROJECT_NAME} ${LIBOBS_LIB})
        else()
            message(WARNING "Could not find obs library in OBS_INSTALLED_DIR")
        endif()
    endif()
endif()

# Platform-specific linking
if(APPLE)
    # macOS: Link AppKit framework for NSWorkspace
    target_link_libraries(${PROJECT_NAME} "-framework AppKit")
elseif(WIN32)
    # Windows: Link user32 for GetForegroundWindow, psapi for process info
    target_link_libraries(${PROJECT_NAME} user32 psapi)
else()
    # Linux: Link X11 if found
    if(X11_FOUND)
        target_include_directories(${PROJECT_NAME} PRIVATE ${X11_INCLUDE_DIR})
        target_link_libraries(${PROJECT_NAME} ${X11_LIBRARIES})
        target_compile_definitions(${PROJECT_NAME} PRIVATE HAVE_X11)
    endif()
endif()

# Output settings
set_target_properties(${PROJECT_NAME} PROPERTIES
    OUTPUT_NAME ${PROJECT_NAME}
    PREFIX ""
)

if(WIN32)
    set_target_properties(${PROJECT_NAME} PROPERTIES SUFFIX ".dll")
elseif(APPLE)
    set_target_properties(${PROJECT_NAME} PROPERTIES SUFFIX ".so")
    set(CMAKE_MACOSX_RPATH ON)
    set_target_properties(${PROJECT_NAME} PROPERTIES
        INSTALL_RPATH "@loader_path/../Frameworks"
        BUILD_WITH_INSTALL_RPATH TRUE
    )
else()
    set_target_properties(${PROJECT_NAME} PROPERTIES SUFFIX ".so")
endif()

# Install
file(GLOB locale_files data/locale/*.ini)

if(WIN32)
    install(TARGETS ${PROJECT_NAME} RUNTIME DESTINATION obs-plugins/64bit)
    install(FILES ${locale_files} DESTINATION data/obs-plugins/${PROJECT_NAME}/locale)
elseif(APPLE)
    set(MACOS_PLUGIN_DIR "$ENV{HOME}/Library/Application Support/obs-studio/plugins/${PROJECT_NAME}")
    install(TARGETS ${PROJECT_NAME} LIBRARY DESTINATION "${MACOS_PLUGIN_DIR}/bin/macos")
    install(FILES ${locale_files} DESTINATION "${MACOS_PLUGIN_DIR}/data/locale")
else()
    install(TARGETS ${PROJECT_NAME} LIBRARY DESTINATION lib/obs-plugins)
    install(FILES ${locale_files} DESTINATION share/obs/obs-plugins/${PROJECT_NAME}/locale)
endif()
