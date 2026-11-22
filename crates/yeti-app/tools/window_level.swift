// Lists on-screen windows of a process with their CoreGraphics window layer.
// Usage: swift crates/yeti-app/tools/window_level.swift [owner-name]   (default: yeti-studio)
// kCGWindowLayer 0 = normal window, 3 = kCGFloatingWindowLevel (always on top).
// Window titles need Screen Recording permission; owner/layer/bounds do not.
import CoreGraphics
import Foundation

let owner = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "yeti-studio"
let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
guard let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
    print("ERROR: CGWindowListCopyWindowInfo returned nil")
    exit(2)
}
var found = 0
for w in list where (w[kCGWindowOwnerName as String] as? String) == owner {
    let layer = w[kCGWindowLayer as String] as? Int ?? -1
    let name = w[kCGWindowName as String] as? String ?? "<no title permission>"
    let b = w[kCGWindowBounds as String] as? [String: Any] ?? [:]
    let width = b["Width"] as? Double ?? 0
    let height = b["Height"] as? Double ?? 0
    let pid = w[kCGWindowOwnerPID as String] as? Int ?? 0
    print("pid=\(pid) layer=\(layer) size=\(Int(width))x\(Int(height)) title=\(name)")
    found += 1
}
if found == 0 {
    print("NO_WINDOWS for owner \(owner)")
    exit(1)
}
