import AppKit

/// 原生菜单会将符号图像当作模板处理；将状态圆点渲染成原色图像以保留红黄绿。
@MainActor
enum MenuStatusDot {
    private static var images: [ServiceIndicator: NSImage] = [:]

    static func image(for indicator: ServiceIndicator) -> NSImage {
        if let cached = images[indicator] { return cached }
        let color: NSColor = switch indicator {
        case .red: .systemRed
        case .yellow: .systemYellow
        case .green: .systemGreen
        }
        let size = NSSize(width: 9, height: 9)
        let image = NSImage(size: size)
        for scale in [1, 2] {
            guard let bitmap = NSBitmapImageRep(
                bitmapDataPlanes: nil,
                pixelsWide: 9 * scale, pixelsHigh: 9 * scale,
                bitsPerSample: 8, samplesPerPixel: 4,
                hasAlpha: true, isPlanar: false,
                colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
            ), let context = NSGraphicsContext(bitmapImageRep: bitmap) else { continue }
            bitmap.size = size
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = context
            context.cgContext.scaleBy(x: CGFloat(scale), y: CGFloat(scale))
            color.setFill()
            NSBezierPath(ovalIn: NSRect(origin: .zero, size: size).insetBy(dx: 0.5, dy: 0.5)).fill()
            NSGraphicsContext.restoreGraphicsState()
            image.addRepresentation(bitmap)
        }
        image.isTemplate = false
        images[indicator] = image
        return image
    }
}
