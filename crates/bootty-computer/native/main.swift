import AppKit
import ApplicationServices
import Carbon
import ScreenCaptureKit

struct Failure: Error {
    let code: String
    let message: String
}

func fail(_ code: String, _ message: String) throws -> Never {
    throw Failure(code: code, message: message)
}

func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success else { return nil }
    return value
}

func focusedElement() -> AXUIElement? {
    guard let app = NSWorkspace.shared.frontmostApplication else { return nil }
    let application = AXUIElementCreateApplication(app.processIdentifier)
    AXUIElementSetMessagingTimeout(application, 0.2)
    guard let value = attribute(application, kAXFocusedUIElementAttribute),
          CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
    return (value as! AXUIElement)
}

func secureInput() -> Bool {
    if IsSecureEventInputEnabled() { return true }
    guard let element = focusedElement() else { return false }
    let role = attribute(element, kAXRoleAttribute) as? String
    let subrole = attribute(element, kAXSubroleAttribute) as? String
    return role == "AXSecureTextField" || subrole == "AXSecureTextField"
}

func permissionStatus() -> [String: Any] {
    let recording: String
    if #available(macOS 14.0, *) {
        recording = CGPreflightScreenCaptureAccess() ? "granted" : "not_granted"
    } else {
        recording = "unsupported"
    }
    return [
        "accessibility": AXIsProcessTrusted() ? "granted" : "not_granted",
        "screen_recording": recording,
        "secure_input": secureInput()
    ]
}

func boundsDictionary(_ bounds: CGRect) -> [String: Double] {
    return ["x": bounds.origin.x, "y": bounds.origin.y, "width": bounds.width, "height": bounds.height]
}

func elementBounds(_ element: AXUIElement) -> CGRect? {
    guard let position = attribute(element, kAXPositionAttribute),
          let size = attribute(element, kAXSizeAttribute),
          CFGetTypeID(position) == AXValueGetTypeID(),
          CFGetTypeID(size) == AXValueGetTypeID() else { return nil }
    var point = CGPoint.zero
    var dimensions = CGSize.zero
    guard AXValueGetValue(position as! AXValue, .cgPoint, &point),
          AXValueGetValue(size as! AXValue, .cgSize, &dimensions) else { return nil }
    return CGRect(origin: point, size: dimensions)
}

func windows() -> [[String: Any]] {
    return CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
        as? [[String: Any]] ?? []
}

func windowBounds(_ window: [String: Any]) -> CGRect? {
    guard let value = window[kCGWindowBounds as String] as? [String: Any] else { return nil }
    return CGRect(dictionaryRepresentation: value as CFDictionary)
}

func target(_ window: [String: Any]) -> [String: Any]? {
    guard let id = window[kCGWindowNumber as String] as? UInt32,
          let pid = window[kCGWindowOwnerPID as String] as? Int32,
          window[kCGWindowLayer as String] as? Int == 0,
          let bounds = windowBounds(window), bounds.width > 0, bounds.height > 0,
          let app = NSRunningApplication(processIdentifier: pid),
          let bundle = app.bundleIdentifier, let launch = app.launchDate else { return nil }
    var result: [String: Any] = ["window_id": id, "process_id": pid, "bundle_id": bundle,
        "launch_time": launch.timeIntervalSince1970, "bounds": boundsDictionary(bounds)]
    if let title = window[kCGWindowName as String] as? String {
        result["title"] = String(title.prefix(512))
    }
    return result
}

struct ValidatedTarget {
    let windowID: UInt32
    let pid: Int32
    let bounds: CGRect
    let token: [String: Any]
}

func validateTarget(_ request: [String: Any], input: Bool) throws -> ValidatedTarget {
    guard let expected = request["target"] as? [String: Any],
          let id = expected["window_id"] as? UInt32,
          let pid = expected["process_id"] as? Int32,
          let bundle = expected["bundle_id"] as? String,
          let launch = expected["launch_time"] as? Double,
          let expectedBounds = expected["bounds"] as? [String: Double],
          let x = expectedBounds["x"], let y = expectedBounds["y"],
          let width = expectedBounds["width"], let height = expectedBounds["height"],
          [x, y, width, height, launch].allSatisfy({ $0.isFinite }),
          width > 0, height > 0 else { try fail("invalid_target", "invalid window token") }
    let currentWindows = windows()
    guard let window = currentWindows.first(where: { $0[kCGWindowNumber as String] as? UInt32 == id }),
          let current = target(window), let bounds = windowBounds(window) else {
        try fail("target_unavailable", "selected window is unavailable")
    }
    guard current["process_id"] as? Int32 == pid,
          current["bundle_id"] as? String == bundle,
          current["launch_time"] as? Double == launch,
          bounds == CGRect(x: x, y: y, width: width, height: height) else {
        try fail("stale_target", "selected window changed")
    }
    if input {
        guard AXIsProcessTrusted() else { try fail("accessibility_denied", "Accessibility is not granted") }
        guard !secureInput() else { try fail("secure_input", "secure input is active") }
        guard NSWorkspace.shared.frontmostApplication?.processIdentifier == pid else {
            try fail("target_not_focused", "selected application is not focused")
        }
        let app = AXUIElementCreateApplication(pid)
        AXUIElementSetMessagingTimeout(app, 0.2)
        guard let focused = attribute(app, kAXFocusedWindowAttribute),
              CFGetTypeID(focused) == AXUIElementGetTypeID(),
              elementBounds(focused as! AXUIElement) == bounds else {
            try fail("target_not_focused", "selected window is not focused")
        }
        // Public AX focus is matched by bounds. Support ambiguous windows only when a public
        // exact window-id API can validate them; until then they cannot receive input.
        let matching = currentWindows.filter {
            $0[kCGWindowOwnerPID as String] as? Int32 == pid
                && $0[kCGWindowLayer as String] as? Int == 0 && windowBounds($0) == bounds
        }
        guard matching.count == 1 else { try fail("target_not_focused", "window focus is ambiguous") }
    }
    return ValidatedTarget(windowID: id, pid: pid, bounds: bounds, token: current)
}

func point(_ request: [String: Any], _ selected: ValidatedTarget) throws -> CGPoint {
    guard let x = request["x"] as? Double, let y = request["y"] as? Double,
          x.isFinite, y.isFinite else { try fail("invalid_action", "coordinates must be finite") }
    let point = CGPoint(x: x, y: y)
    guard selected.bounds.contains(point) else { try fail("invalid_action", "coordinates are outside the target") }
    // Never send a pointer action through another visible window or system overlay.
    let hit = windows().first { window in
        guard let bounds = windowBounds(window), bounds.contains(point) else { return false }
        return (window[kCGWindowAlpha as String] as? Double ?? 1) > 0
    }
    guard hit?[kCGWindowNumber as String] as? UInt32 == selected.windowID else {
        try fail("target_not_focused", "selected point is covered by another window")
    }
    return point
}

func mouseEvent(_ point: CGPoint, _ kind: CGEventType, _ button: CGMouseButton) throws -> CGEvent {
    guard let event = CGEvent(mouseEventSource: nil, mouseType: kind,
                              mouseCursorPosition: point, mouseButton: button) else {
        try fail("input_error", "could not create mouse event")
    }
    return event
}

func click(_ request: [String: Any], _ selected: ValidatedTarget) throws {
    let location = try point(request, selected)
    let kinds: (CGEventType, CGEventType, CGMouseButton)
    switch request["button"] as? String {
    case "left": kinds = (.leftMouseDown, .leftMouseUp, .left)
    case "right": kinds = (.rightMouseDown, .rightMouseUp, .right)
    case "middle": kinds = (.otherMouseDown, .otherMouseUp, .center)
    default: try fail("invalid_action", "unknown mouse button")
    }
    let down = try mouseEvent(location, kinds.0, kinds.2)
    let up = try mouseEvent(location, kinds.1, kinds.2)
    _ = try validateTarget(request, input: true)
    _ = try point(request, selected)
    // Allocate both events before posting, then always complete the release to the same PID.
    down.postToPid(selected.pid)
    up.postToPid(selected.pid)
}

func key(_ code: CGKeyCode, pid: Int32, flags: CGEventFlags = [], text: [UniChar]? = nil) throws {
    guard !secureInput() else { try fail("secure_input", "secure input is active") }
    guard let down = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: true),
          let up = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: false) else {
        try fail("input_error", "could not create keyboard event")
    }
    down.flags = flags
    up.flags = flags
    if let text {
        text.withUnsafeBufferPointer { buffer in
            down.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: buffer.baseAddress)
            up.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: buffer.baseAddress)
        }
    }
    down.postToPid(pid)
    up.postToPid(pid)
}

func typeText(_ text: String, request: [String: Any], pid: Int32) throws {
    guard !text.isEmpty, text.utf8.count <= 4096 else { try fail("invalid_action", "text must contain 1–4096 bytes") }
    // Keep surrogate pairs together and stay below Quartz's 20 UTF-16-unit event limit.
    var buffer: [UniChar] = []
    for scalar in text.unicodeScalars {
        let units = Array(String(scalar).utf16)
        if buffer.count + units.count > 20 {
            _ = try validateTarget(request, input: true)
            try key(0, pid: pid, text: buffer)
            // AppKit may merge adjacent synthetic text chunks without input-event spacing.
            Thread.sleep(forTimeInterval: 0.05)
            buffer.removeAll(keepingCapacity: true)
        }
        buffer.append(contentsOf: units)
    }
    if !buffer.isEmpty {
        _ = try validateTarget(request, input: true)
        try key(0, pid: pid, text: buffer)
    }
}

func sendKey(_ request: [String: Any], pid: Int32) throws {
    let codes: [String: Int] = [
        "return": kVK_Return, "tab": kVK_Tab, "escape": kVK_Escape,
        "space": kVK_Space, "backspace": kVK_Delete, "delete": kVK_ForwardDelete,
        "left": kVK_LeftArrow, "right": kVK_RightArrow, "up": kVK_UpArrow,
        "down": kVK_DownArrow, "home": kVK_Home, "end": kVK_End,
        "page_up": kVK_PageUp, "page_down": kVK_PageDown, "a": kVK_ANSI_A,
        "b": kVK_ANSI_B, "c": kVK_ANSI_C, "d": kVK_ANSI_D,
        "e": kVK_ANSI_E, "f": kVK_ANSI_F, "g": kVK_ANSI_G,
        "h": kVK_ANSI_H, "i": kVK_ANSI_I, "j": kVK_ANSI_J,
        "k": kVK_ANSI_K, "l": kVK_ANSI_L, "m": kVK_ANSI_M,
        "n": kVK_ANSI_N, "o": kVK_ANSI_O, "p": kVK_ANSI_P,
        "q": kVK_ANSI_Q, "r": kVK_ANSI_R, "s": kVK_ANSI_S,
        "t": kVK_ANSI_T, "u": kVK_ANSI_U, "v": kVK_ANSI_V,
        "w": kVK_ANSI_W, "x": kVK_ANSI_X, "y": kVK_ANSI_Y,
        "z": kVK_ANSI_Z, "0": kVK_ANSI_0, "1": kVK_ANSI_1,
        "2": kVK_ANSI_2, "3": kVK_ANSI_3, "4": kVK_ANSI_4,
        "5": kVK_ANSI_5, "6": kVK_ANSI_6, "7": kVK_ANSI_7,
        "8": kVK_ANSI_8, "9": kVK_ANSI_9, "f1": kVK_F1,
        "f2": kVK_F2, "f3": kVK_F3, "f4": kVK_F4,
        "f5": kVK_F5, "f6": kVK_F6, "f7": kVK_F7,
        "f8": kVK_F8, "f9": kVK_F9, "f10": kVK_F10,
        "f11": kVK_F11, "f12": kVK_F12, "f13": kVK_F13,
        "f14": kVK_F14, "f15": kVK_F15, "f16": kVK_F16,
        "f17": kVK_F17, "f18": kVK_F18, "f19": kVK_F19,
        "f20": kVK_F20, "minus": kVK_ANSI_Minus, "equal": kVK_ANSI_Equal,
        "left_bracket": kVK_ANSI_LeftBracket, "right_bracket": kVK_ANSI_RightBracket, "backslash": kVK_ANSI_Backslash,
        "semicolon": kVK_ANSI_Semicolon, "quote": kVK_ANSI_Quote, "comma": kVK_ANSI_Comma,
        "period": kVK_ANSI_Period, "slash": kVK_ANSI_Slash, "backtick": kVK_ANSI_Grave
    ]
    guard let name = request["key"] as? String, let code = codes[name],
          let modifiers = request["modifiers"] as? [String], modifiers.count <= 4 else {
        try fail("invalid_action", "invalid key or modifiers")
    }
    var flags: CGEventFlags = []
    for modifier in modifiers {
        switch modifier {
        case "command": flags.insert(.maskCommand)
        case "control": flags.insert(.maskControl)
        case "option": flags.insert(.maskAlternate)
        case "shift": flags.insert(.maskShift)
        default: try fail("invalid_action", "unknown keyboard modifier")
        }
    }
    _ = try validateTarget(request, input: true)
    try key(CGKeyCode(code), pid: pid, flags: flags)
}

@available(macOS 14.0, *)
func snapshot(_ request: [String: Any]) async throws -> [String: Any] {
    guard CGPreflightScreenCaptureAccess() else { try fail("screen_recording_denied", "Screen Recording is not granted") }
    let selected = try validateTarget(request, input: false)
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    guard let window = content.windows.first(where: {
        $0.windowID == selected.windowID && $0.owningApplication?.processID == selected.pid
    }) else { try fail("target_unavailable", "selected window cannot be captured") }
    let filter = SCContentFilter(desktopIndependentWindow: window)
    let config = SCStreamConfiguration()
    let scale = min(1.0, 1600.0 / max(selected.bounds.width, selected.bounds.height))
    config.width = max(1, Int(selected.bounds.width * scale))
    config.height = max(1, Int(selected.bounds.height * scale))
    config.showsCursor = true
    // Frame metadata maps only window pixels, without compositor shadow padding.
    config.ignoreShadowsSingleWindow = true
    let region: CGRect?
    if request["action"] as? String == "snapshot_region" {
        guard let value = request["rect"] as? [String: Double],
              let x = value["x"], let y = value["y"],
              let width = value["width"], let height = value["height"],
              [x, y, width, height, x + width, y + height].allSatisfy({ $0.isFinite }),
              width > 0, height > 0 else { try fail("invalid_action", "capture rectangle is invalid") }
        let requested = CGRect(x: x, y: y, width: width, height: height)
        guard selected.bounds.contains(requested), window.frame == selected.bounds else {
            try fail("stale_target", "capture geometry changed")
        }
        region = requested
        config.showsCursor = false
    } else {
        region = nil
    }
    let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
    guard CGPreflightScreenCaptureAccess() else { try fail("screen_recording_denied", "Screen Recording was revoked") }
    guard !secureInput() else { try fail("secure_input", "secure input became active during capture") }
    _ = try validateTarget(request, input: false)
    let output: CGImage
    let actualRegion: CGRect?
    if let region {
        let sx = Double(image.width) / selected.bounds.width
        let sy = Double(image.height) / selected.bounds.height
        // Snap inward: a fractional edge must never include adjacent host chrome pixels.
        let left = ceil((region.minX - selected.bounds.minX) * sx)
        let top = ceil((region.minY - selected.bounds.minY) * sy)
        let right = floor((region.maxX - selected.bounds.minX) * sx)
        let bottom = floor((region.maxY - selected.bounds.minY) * sy)
        guard right > left, bottom > top,
              let cropped = image.cropping(to: CGRect(x: left, y: top, width: right - left, height: bottom - top)) else {
            try fail("invalid_action", "capture rectangle has no complete pixels")
        }
        output = cropped
        let sourceLeft = selected.bounds.minX + left / sx
        let sourceTop = selected.bounds.minY + top / sy
        let sourceRight = selected.bounds.minX + right / sx
        let sourceBottom = selected.bounds.minY + bottom / sy
        actualRegion = CGRect(x: sourceLeft, y: sourceTop,
            width: sourceRight - sourceLeft, height: sourceBottom - sourceTop)
    } else {
        output = image
        actualRegion = nil
    }
    guard let data = NSBitmapImageRep(cgImage: output).representation(using: .png, properties: [:]),
          data.count <= 8 * 1024 * 1024 else {
        try fail("capture_error", "screenshot exceeds the encoding limit")
    }
    var result: [String: Any] = ["result": "snapshot", "png_base64": data.base64EncodedString(),
        "pixel_width": output.width, "pixel_height": output.height, "target": selected.token]
    if let region, let actualRegion {
        result["requested_region"] = boundsDictionary(region)
        result["region"] = boundsDictionary(actualRegion)
    }
    return result
}

func focusWindow(_ request: [String: Any]) async throws -> [String: Any] {
    guard AXIsProcessTrusted() else { try fail("accessibility_denied", "Accessibility is not granted") }
    guard !secureInput() else { try fail("secure_input", "secure input is active") }
    let selected = try validateTarget(request, input: false)
    let matching = windows().filter {
        $0[kCGWindowOwnerPID as String] as? Int32 == selected.pid
            && $0[kCGWindowLayer as String] as? Int == 0 && windowBounds($0) == selected.bounds
    }
    guard matching.count == 1 else { try fail("target_not_focused", "window focus is ambiguous") }
    let ax = AXUIElementCreateApplication(selected.pid)
    AXUIElementSetMessagingTimeout(ax, 0.2)
    guard let candidates = attribute(ax, kAXWindowsAttribute) as? [AXUIElement] else {
        try fail("target_unavailable", "selected window has no accessibility handle")
    }
    let exact = candidates.filter { elementBounds($0) == selected.bounds }
    guard exact.count == 1, let window = exact.first else { try fail("target_not_focused", "window focus is ambiguous") }
    _ = try validateTarget(request, input: false)
    guard AXUIElementPerformAction(window, kAXRaiseAction as CFString) == .success,
          let app = NSRunningApplication(processIdentifier: selected.pid),
          app.activate(options: [.activateIgnoringOtherApps]) else { try fail("target_not_focused", "selected window could not be raised") }
    // Activation is asynchronous. Confirm the observed focus within a bounded interval.
    for _ in 0..<10 {
        do { _ = try validateTarget(request, input: true); return ["result": "posted"] }
        catch { try await Task.sleep(nanoseconds: 20_000_000) }
    }
    _ = try validateTarget(request, input: true)
    return ["result": "posted"]
}

func execute(_ request: [String: Any]) async throws -> [String: Any] {
    guard request["enabled"] as? Bool == true else { try fail("disabled", "computer use is disabled") }
    guard !secureInput() else { try fail("secure_input", "secure input is active") }
    if ["snapshot", "snapshot_region"].contains(request["action"] as? String ?? "") {
        if #available(macOS 14.0, *) { return try await snapshot(request) }
        try fail("unsupported", "screenshots require macOS 14 or later")
    }
    if request["action"] as? String == "focus" { return try await focusWindow(request) }
    let selected = try validateTarget(request, input: true)
    switch request["action"] as? String {
    case "click": try click(request, selected)
    case "move":
        let location = try point(request, selected)
        _ = try validateTarget(request, input: true)
        _ = try point(request, selected)
        try mouseEvent(location, .mouseMoved, .left).postToPid(selected.pid)
    case "type_text":
        guard let text = request["text"] as? String else { try fail("invalid_action", "text is required") }
        try typeText(text, request: request, pid: selected.pid)
    case "key": try sendKey(request, pid: selected.pid)
    case "scroll":
        let location = try point(request, selected)
        guard let dx = request["delta_x"] as? Int32, let dy = request["delta_y"] as? Int32,
              dx >= -10000, dx <= 10000, dy >= -10000, dy <= 10000,
              let event = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 2,
                                  wheel1: dy, wheel2: dx, wheel3: 0) else {
            try fail("invalid_action", "invalid scroll delta")
        }
        _ = try validateTarget(request, input: true)
        _ = try point(request, selected)
        event.location = location
        event.postToPid(selected.pid)
    default: try fail("invalid_action", "unknown computer action")
    }
    return ["result": "posted"]
}

@main
struct ComputerHelper {
    static func main() async {
        var response: [String: Any]
        do {
            let data = FileHandle.standardInput.readData(ofLength: 131073)
            guard data.count <= 131072, let request = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                try fail("invalid_action", "invalid request")
            }
            switch request["method"] as? String {
            case "status": response = ["value": permissionStatus()]
            case "targets": response = ["value": windows().prefix(256).compactMap(target)]
            case "execute": response = ["value": try await execute(request)]
            default: try fail("invalid_action", "unknown method")
            }
        } catch let failure as Failure {
            response = ["error": ["code": failure.code, "message": failure.message]]
        } catch {
            response = ["error": ["code": "platform_error", "message": "native platform operation failed"]]
        }
        if let data = try? JSONSerialization.data(withJSONObject: response, options: [.sortedKeys]) {
            FileHandle.standardOutput.write(data)
        }
    }
}
