cmake_minimum_required(VERSION 3.30)

if(NOT CMAKE_GENERATOR STREQUAL "Xcode")
    message(FATAL_ERROR "Generate the macOS project with cmake -G Xcode.")
endif()

set(CMAKE_OSX_DEPLOYMENT_TARGET "14.0" CACHE STRING "Minimum supported macOS version")
set(CMAKE_OSX_SYSROOT "macosx" CACHE STRING "macOS SDK")
set(CMAKE_XCODE_ATTRIBUTE_SWIFT_VERSION "6.0")

project(LokiConfigEditor VERSION 0.1.0 LANGUAGES Swift)

set(EDITOR_SOURCES
    Sources/App/ConfigEditorApp.swift
    Sources/Model/ConfigEditorModel.swift
    Sources/Views/ConfigEditorView.swift
)
list(TRANSFORM EDITOR_SOURCES PREPEND "${CMAKE_CURRENT_SOURCE_DIR}/")

add_executable(LokiConfigEditor MACOSX_BUNDLE ${EDITOR_SOURCES})
source_group(TREE "${CMAKE_CURRENT_SOURCE_DIR}/Sources" PREFIX Sources FILES ${EDITOR_SOURCES})
set_target_properties(LokiConfigEditor PROPERTIES
    MACOSX_BUNDLE_INFO_PLIST "${CMAKE_CURRENT_SOURCE_DIR}/cmake/Info.plist.in"
    MACOSX_BUNDLE_GUI_IDENTIFIER "cc.loki-assistant.config-editor"
    MACOSX_BUNDLE_BUNDLE_NAME "Loki Config Editor"
    MACOSX_BUNDLE_SHORT_VERSION_STRING "${PROJECT_VERSION}"
    MACOSX_BUNDLE_BUNDLE_VERSION "1"
    XCODE_GENERATE_SCHEME YES
    XCODE_ATTRIBUTE_CODE_SIGN_STYLE Automatic
    XCODE_ATTRIBUTE_CODE_SIGN_IDENTITY "-"
    XCODE_ATTRIBUTE_PRODUCT_BUNDLE_IDENTIFIER "cc.loki-assistant.config-editor"
    XCODE_ATTRIBUTE_ENABLE_APP_SANDBOX NO
    XCODE_ATTRIBUTE_ENABLE_HARDENED_RUNTIME YES
    XCODE_ATTRIBUTE_DEFINES_MODULE YES
    XCODE_ATTRIBUTE_SWIFT_STRICT_CONCURRENCY complete
    Swift_COMPILATION_MODE "$<IF:$<CONFIG:Release>,wholemodule,incremental>"
)

include(CTest)
if(BUILD_TESTING)
    set_property(TARGET LokiConfigEditor PROPERTY XCODE_ATTRIBUTE_ENABLE_TESTABILITY YES)
    set_property(TARGET LokiConfigEditor PROPERTY XCODE_ATTRIBUTE_ENABLE_HARDENED_RUNTIME NO)
    find_package(XCTest REQUIRED)
    xctest_add_bundle(LokiConfigEditorTests LokiConfigEditor
        Tests/ConfigEditorModelTests.swift
    )
    set_target_properties(LokiConfigEditorTests PROPERTIES
        XCODE_GENERATE_SCHEME YES
        XCODE_ATTRIBUTE_PRODUCT_BUNDLE_IDENTIFIER "cc.loki-assistant.config-editor.tests"
        XCODE_ATTRIBUTE_CODE_SIGN_STYLE Automatic
        XCODE_ATTRIBUTE_CODE_SIGN_IDENTITY "-"
        XCODE_ATTRIBUTE_SWIFT_STRICT_CONCURRENCY complete
    )
    find_program(EDITOR_XCODEBUILD xcodebuild REQUIRED)
    add_test(NAME LokiConfigEditorTests
        COMMAND "${EDITOR_XCODEBUILD}"
            -project "${CMAKE_CURRENT_BINARY_DIR}/LokiConfigEditor.xcodeproj"
            -scheme LokiConfigEditor
            -configuration "$<CONFIG>"
            -destination "platform=macOS"
            test
    )
    set_tests_properties(LokiConfigEditorTests PROPERTIES TIMEOUT 120)
endif()
