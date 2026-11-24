import AppKit
let output = CommandLine.arguments[1]
let size = 1024
let rep = NSBitmapImageRep(bitmapDataPlanes:nil,pixelsWide:size,pixelsHigh:size,bitsPerSample:8,samplesPerPixel:4,hasAlpha:true,isPlanar:false,colorSpaceName:.deviceRGB,bytesPerRow:0,bitsPerPixel:0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep:rep)
let base=NSBezierPath(roundedRect:NSRect(x:52,y:52,width:920,height:920),xRadius:205,yRadius:205)
NSColor(calibratedRed:0.08,green:0.19,blue:0.21,alpha:1).setFill(); base.fill()
let ring=NSBezierPath(ovalIn:NSRect(x:190,y:190,width:644,height:644));ring.lineWidth=5
NSColor(calibratedRed:0.4,green:0.68,blue:0.64,alpha:0.35).setStroke();ring.stroke()
for i in 0..<5 {
 NSGraphicsContext.saveGraphicsState()
 let t=AffineTransform(translationByX:512,byY:512);var r=t;r.rotate(byDegrees:Double(i)*72);(r as NSAffineTransform).concat()
 let blade=NSBezierPath();blade.move(to:NSPoint(x:5,y:45));blade.curve(to:NSPoint(x:78,y:285),controlPoint1:NSPoint(x:-72,y:130),controlPoint2:NSPoint(x:-68,y:275));blade.curve(to:NSPoint(x:218,y:151),controlPoint1:NSPoint(x:195,y:310),controlPoint2:NSPoint(x:282,y:235));blade.curve(to:NSPoint(x:5,y:45),controlPoint1:NSPoint(x:179,y:83),controlPoint2:NSPoint(x:92,y:139));blade.close()
 NSColor(calibratedRed:0.68,green:0.91,blue:0.83,alpha:1).setFill();blade.fill();NSGraphicsContext.restoreGraphicsState()
}
NSColor(calibratedRed:0.95,green:0.95,blue:0.89,alpha:1).setFill();NSBezierPath(ovalIn:NSRect(x:460,y:460,width:104,height:104)).fill()
NSGraphicsContext.restoreGraphicsState()
try rep.representation(using:.png,properties:[:])!.write(to:URL(fileURLWithPath:output))
print("Created original five-blade Macs Fan icon: \(output)")
