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
    Sources/App/CenterLog.swift
    Sources/App/ServiceStatusModel.swift
    Sources/App/ServiceSettingsClient.swift
    Sources/App/EmacsReportedStatus.swift
    Sources/App/SystemNotificationController.swift
    Sources/App/NotificationReceiptClient.swift
    Sources/Features/Dashboard/DashboardView.swift
)
list(TRANSFORM LOKI_APP_SOURCES PREPEND "${CMAKE_CURRENT_SOURCE_DIR}/")

file(GLOB LOKI_NOTIFICATION_APP_ICONS CONFIGURE_DEPENDS
    "${CMAKE_CURRENT_SOURCE_DIR}/Resources/NotificationAppIcons/*.png"
)
set_source_files_properties(${LOKI_NOTIFICATION_APP_ICONS} PROPERTIES
    MACOSX_PACKAGE_LOCATION "Resources/NotificationAppIcons"
)
set(LOKI_APP_ICONS
    Resources/AppIcon/loki-assistant-menu-v1.png
    Resources/AppIcon/loki-assistant-menu-v1@2x.png
    Resources/Assets.xcassets
)
list(TRANSFORM LOKI_APP_ICONS PREPEND "${CMAKE_CURRENT_SOURCE_DIR}/")
set_source_files_properties(${LOKI_APP_ICONS} PROPERTIES
    MACOSX_PACKAGE_LOCATION "Resources"
)
add_executable(LokiAssistant MACOSX_BUNDLE ${LOKI_APP_SOURCES} ${LOKI_NOTIFICATION_APP_ICONS} ${LOKI_APP_ICONS})
source_group(TREE "${CMAKE_CURRENT_SOURCE_DIR}/Sources" PREFIX Sources FILES ${LOKI_APP_SOURCES})
set_target_properties(LokiAssistant PROPERTIES
    MACOSX_BUNDLE_INFO_PLIST "${CMAKE_CURRENT_SOURCE_DIR}/cmake/Info.plist.in"
    MACOSX_BUNDLE_GUI_IDENTIFIER "cc.loki-assistant.center"
    MACOSX_BUNDLE_BUNDLE_NAME "Loki的助理"
    MACOSX_BUNDLE_ICON_FILE "AppIcon"
    MACOSX_BUNDLE_SHORT_VERSION_STRING "${PROJECT_VERSION}"
    MACOSX_BUNDLE_BUNDLE_VERSION "2"
    XCODE_GENERATE_SCHEME YES
    XCODE_ATTRIBUTE_CODE_SIGN_STYLE Automatic
    XCODE_ATTRIBUTE_CODE_SIGN_IDENTITY "-"
    XCODE_ATTRIBUTE_PRODUCT_BUNDLE_IDENTIFIER "cc.loki-assistant.center"
    XCODE_ATTRIBUTE_ASSETCATALOG_COMPILER_APPICON_NAME "AppIcon"
    XCODE_ATTRIBUTE_ENABLE_APP_SANDBOX NO
    XCODE_ATTRIBUTE_ENABLE_HARDENED_RUNTIME YES
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
        Tests/ServiceSettingsTests.swift
        Tests/EmacsReportedStatusTests.swift
        Tests/CenterLogTests.swift
        Tests/SystemNotificationTests.swift
        Tests/NotificationReceiptTests.swift
    )
    set_target_properties(LokiAssistantTests PROPERTIES
        XCODE_GENERATE_SCHEME YES
        XCODE_ATTRIBUTE_PRODUCT_BUNDLE_IDENTIFIER "cc.loki-assistant.center.tests"
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
