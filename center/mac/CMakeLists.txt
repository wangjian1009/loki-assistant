cmake_minimum_required(VERSION 3.30)

if(NOT CMAKE_GENERATOR STREQUAL "Xcode")
    message(FATAL_ERROR "Generate the macOS project with cmake -G Xcode.")
endif()

set(CMAKE_OSX_DEPLOYMENT_TARGET "14.0" CACHE STRING "Minimum supported macOS version")
set(CMAKE_OSX_SYSROOT "macosx" CACHE STRING "macOS SDK")
set(CMAKE_XCODE_ATTRIBUTE_SWIFT_VERSION "6.0")

project(LokiAssistant VERSION 0.1.0 LANGUAGES Swift)

set(LOKI_APP_SOURCES
    Sources/App/LokiAssistantApp.swift
    Sources/App/ServiceStatusModel.swift
    Sources/Features/Dashboard/DashboardView.swift
)
list(TRANSFORM LOKI_APP_SOURCES PREPEND "${CMAKE_CURRENT_SOURCE_DIR}/")

add_executable(LokiAssistant MACOSX_BUNDLE ${LOKI_APP_SOURCES})
source_group(TREE "${CMAKE_CURRENT_SOURCE_DIR}/Sources" PREFIX Sources FILES ${LOKI_APP_SOURCES})
set_target_properties(LokiAssistant PROPERTIES
    MACOSX_BUNDLE_INFO_PLIST "${CMAKE_CURRENT_SOURCE_DIR}/cmake/Info.plist.in"
    MACOSX_BUNDLE_GUI_IDENTIFIER "personal.loki-assistant.mac"
    MACOSX_BUNDLE_BUNDLE_NAME "Loki Assistant"
    MACOSX_BUNDLE_SHORT_VERSION_STRING "${PROJECT_VERSION}"
    MACOSX_BUNDLE_BUNDLE_VERSION "1"
    XCODE_GENERATE_SCHEME YES
    XCODE_ATTRIBUTE_CODE_SIGN_STYLE Automatic
    XCODE_ATTRIBUTE_CODE_SIGN_IDENTITY "-"
    XCODE_ATTRIBUTE_PRODUCT_BUNDLE_IDENTIFIER "personal.loki-assistant.mac"
    XCODE_ATTRIBUTE_ENABLE_APP_SANDBOX NO
    XCODE_ATTRIBUTE_ENABLE_HARDENED_RUNTIME YES
    XCODE_ATTRIBUTE_DEFINES_MODULE YES
    XCODE_ATTRIBUTE_SWIFT_STRICT_CONCURRENCY complete
    XCODE_ATTRIBUTE_DEFINES_MODULE YES
    Swift_COMPILATION_MODE "$<IF:$<CONFIG:Release>,wholemodule,incremental>"
)

include(CTest)
if(BUILD_TESTING)
    set_property(TARGET LokiAssistant PROPERTY XCODE_ATTRIBUTE_ENABLE_TESTABILITY YES)
    set_property(TARGET LokiAssistant PROPERTY XCODE_ATTRIBUTE_ENABLE_HARDENED_RUNTIME NO)
    find_package(XCTest REQUIRED)
    xctest_add_bundle(LokiAssistantTests LokiAssistant
        Tests/ServiceStatusTests.swift
    )
    set_target_properties(LokiAssistantTests PROPERTIES
        XCODE_GENERATE_SCHEME YES
        XCODE_ATTRIBUTE_PRODUCT_BUNDLE_IDENTIFIER "personal.loki-assistant.mac.tests"
        XCODE_ATTRIBUTE_CODE_SIGN_STYLE Automatic
        XCODE_ATTRIBUTE_CODE_SIGN_IDENTITY "-"
        XCODE_ATTRIBUTE_SWIFT_STRICT_CONCURRENCY complete
    )
    find_program(LOKI_XCODEBUILD xcodebuild REQUIRED)
    add_test(NAME LokiAssistantTests
        COMMAND "${LOKI_XCODEBUILD}"
            -project "${CMAKE_CURRENT_BINARY_DIR}/LokiAssistant.xcodeproj"
            -scheme LokiAssistant
            -configuration "$<CONFIG>"
            -destination "platform=macOS"
            test
    )
    set_tests_properties(LokiAssistantTests PROPERTIES TIMEOUT 120)
endif()
