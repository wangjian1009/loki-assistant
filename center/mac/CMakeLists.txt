cmake_minimum_required(VERSION 3.30)

if(NOT CMAKE_GENERATOR STREQUAL "Xcode")
    message(FATAL_ERROR "Generate the macOS project with cmake -G Xcode.")
endif()

set(CMAKE_OSX_DEPLOYMENT_TARGET "14.0" CACHE STRING "Minimum supported macOS version")
set(CMAKE_OSX_SYSROOT "macosx" CACHE STRING "macOS SDK")
# Let Xcode pass the language mode once, rather than adding -swift-version
# to OTHER_SWIFT_FLAGS as well as SWIFT_VERSION.
set(CMAKE_XCODE_ATTRIBUTE_SWIFT_VERSION "6.0")

project(LokiAssistant VERSION 0.1.0 LANGUAGES Swift)

set(LOKI_APP_SOURCES
    Sources/App/ApplicationModel.swift
    Sources/App/LokiAssistantApp.swift
    Sources/Emacs/EmacsClient.swift
    Sources/Emacs/EmacsProtocolCodec.swift
    Sources/Features/AgentShell/AgentShellView.swift
    Sources/Features/Settings/SettingsView.swift
    Sources/Models/EmacsConfiguration.swift
    Sources/Models/EmacsProtocol.swift
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
    XCODE_ATTRIBUTE_SWIFT_STRICT_CONCURRENCY complete
    XCODE_ATTRIBUTE_ENABLE_TESTABILITY "$<IF:$<CONFIG:Debug>,YES,NO>"
    XCODE_ATTRIBUTE_DEFINES_MODULE YES
    XCODE_ATTRIBUTE_LD_RUNPATH_SEARCH_PATHS "$(inherited) @executable_path/../Frameworks"
    Swift_COMPILATION_MODE "$<IF:$<CONFIG:Release>,wholemodule,incremental>"
)

include(CTest)
if(BUILD_TESTING)
    set_property(TARGET LokiAssistant PROPERTY XCODE_ATTRIBUTE_ENABLE_TESTABILITY YES)
    # Local ad-hoc signatures cannot satisfy library validation for hosted tests.
    set_property(TARGET LokiAssistant PROPERTY XCODE_ATTRIBUTE_ENABLE_HARDENED_RUNTIME NO)
    find_package(XCTest REQUIRED)
    xctest_add_bundle(LokiAssistantTests LokiAssistant
        Tests/EmacsProtocolCodecTests.swift
        Tests/EmacsConnectionIntegrationTests.swift
    )
    set_target_properties(LokiAssistantTests PROPERTIES
        XCODE_GENERATE_SCHEME YES
        XCODE_ATTRIBUTE_PRODUCT_BUNDLE_IDENTIFIER "personal.loki-assistant.mac.tests"
        XCODE_ATTRIBUTE_CODE_SIGN_STYLE Automatic
        XCODE_ATTRIBUTE_CODE_SIGN_IDENTITY "-"
        XCODE_ATTRIBUTE_SWIFT_STRICT_CONCURRENCY complete
        XCODE_ATTRIBUTE_LD_RUNPATH_SEARCH_PATHS "$(inherited) @executable_path/../Frameworks @loader_path/../Frameworks"
    )
    target_include_directories(LokiAssistantTests PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/$<CONFIG>")

    # App-hosted XCTest bundles must run through Xcode to load the test host.
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
