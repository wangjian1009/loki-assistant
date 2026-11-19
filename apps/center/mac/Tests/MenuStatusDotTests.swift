import AppKit
import XCTest
@testable import LokiAssistant

final class MenuStatusDotTests: XCTestCase {
    @MainActor
    func testMenuDotsKeepStatusColorsAndTransparentCornersAtBothScales() throws {
        for (indicator, color) in [
            (ServiceIndicator.red, NSColor.systemRed),
            (.yellow, .systemYellow),
            (.green, .systemGreen),
        ] {
            let image = MenuStatusDot.image(for: indicator)
            XCTAssertFalse(image.isTemplate)
            XCTAssertEqual(image.size, NSSize(width: 9, height: 9))
            XCTAssertTrue(image === MenuStatusDot.image(for: indicator))
            let bitmaps = image.representations.compactMap { $0 as? NSBitmapImageRep }
            XCTAssertEqual(bitmaps.map(\.pixelsWide), [9, 18])
            let expected = try XCTUnwrap(color.usingColorSpace(.deviceRGB))
            for bitmap in bitmaps {
                let center = try XCTUnwrap(bitmap.colorAt(x: bitmap.pixelsWide / 2, y: bitmap.pixelsHigh / 2)?.usingColorSpace(.deviceRGB))
                XCTAssertEqual(center.redComponent, expected.redComponent, accuracy: 0.02)
                XCTAssertEqual(center.greenComponent, expected.greenComponent, accuracy: 0.02)
                XCTAssertEqual(center.blueComponent, expected.blueComponent, accuracy: 0.02)
                XCTAssertEqual(center.alphaComponent, 1, accuracy: 0.02)
                XCTAssertEqual(try XCTUnwrap(bitmap.colorAt(x: 0, y: 0)).alphaComponent, 0, accuracy: 0.02)
            }
        }
    }
}
