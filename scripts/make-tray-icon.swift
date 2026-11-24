import AppKit
let directory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
let size = 36
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
                          bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
                          isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: size * 4, bitsPerPixel: 32)!
rep.bitmapData!.initialize(repeating: 0, count: size * size * 4)
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSColor.black.setFill()
for blade in 0..<5 {
    NSGraphicsContext.saveGraphicsState()
    var transform = AffineTransform(translationByX: 18, byY: 18)
    transform.rotate(byDegrees: Double(blade) * 72)
    (transform as NSAffineTransform).concat()
    let path = NSBezierPath()
    path.move(to: NSPoint(x: 0.3, y: 3))
    path.curve(to: NSPoint(x: 3.5, y: 14), controlPoint1: NSPoint(x: -3, y: 7), controlPoint2: NSPoint(x: -3, y: 13))
    path.curve(to: NSPoint(x: 10.2, y: 7), controlPoint1: NSPoint(x: 9, y: 15), controlPoint2: NSPoint(x: 13, y: 11))
    path.curve(to: NSPoint(x: 0.3, y: 3), controlPoint1: NSPoint(x: 8, y: 4), controlPoint2: NSPoint(x: 4, y: 6))
    path.close(); path.fill()
    NSGraphicsContext.restoreGraphicsState()
}
NSBezierPath(ovalIn: NSRect(x: 15.7, y: 15.7, width: 4.6, height: 4.6)).fill()
NSGraphicsContext.restoreGraphicsState()
try rep.representation(using: .png, properties: [:])!.write(to: directory.appendingPathComponent("tray-template.png"))
try Data(bytes: rep.bitmapData!, count: size * size * 4).write(to: directory.appendingPathComponent("tray-template.rgba"))
print("Created original 36px template icon (PNG and RGBA).")
