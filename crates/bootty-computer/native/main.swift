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

func accessibilityElements(_ app: NSRunningApplication?) -> [[String: Any]] {
    guard AXIsProcessTrusted(), let app else { return [] }
    var result: [[String: Any]] = []
    var pending = [AXUIElementCreateApplication(app.processIdentifier)]
    var visited = 0
    let deadline = Date().addingTimeInterval(2)
    // Bounded AX reads prevent an unresponsive or enormous app from monopolizing capture.
    AXUIElementSetMessagingTimeout(pending[0], 0.2)
    while let element = pending.popLast(), visited < 500, Date() < deadline {
        visited += 1
        let role = attribute(element, kAXRoleAttribute) as? String ?? "AXUnknown"
        let subrole = attribute(element, kAXSubroleAttribute) as? String
        if role == "AXSecureTextField" || subrole == "AXSecureTextField" { continue }
        let label = (attribute(element, kAXTitleAttribute) as? String)
            ?? (attribute(element, kAXDescriptionAttribute) as? String)
        var row: [String: Any] = ["role": role]
        if let label, !label.isEmpty { row["label"] = String(label.prefix(512)) }
        if let value = attribute(element, kAXValueAttribute) as? String {
            row["value"] = String(value.prefix(2048))
        }
        if let bounds = elementBounds(element) { row["bounds"] = boundsDictionary(bounds) }
        result.append(row)
        if let children = attribute(element, kAXChildrenAttribute) as? [AXUIElement] {
            pending.append(contentsOf: children.prefix(500 - visited))
        }
    }
    return result
}

func point(_ request: [String: Any]) throws -> CGPoint {
    guard let x = request["x"] as? Double, let y = request["y"] as? Double,
          x.isFinite, y.isFinite else { try fail("invalid_action", "coordinates must be finite") }
    let point = CGPoint(x: x, y: y)
    var displays = [CGDirectDisplayID](repeating: 0, count: 32)
    var count: UInt32 = 0
    guard CGGetActiveDisplayList(32, &displays, &count) == .success,
          displays.prefix(Int(count)).contains(where: { CGDisplayBounds($0).contains(point) }) else {
        try fail("invalid_action", "coordinates are outside the active displays")
    }
    return point
}

func mouse(_ point: CGPoint, _ kind: CGEventType, _ button: CGMouseButton) throws {
    // The action checks secure input before starting. Always complete its release event.
    guard let event = CGEvent(mouseEventSource: nil, mouseType: kind,
                              mouseCursorPosition: point, mouseButton: button) else {
        try fail("input_error", "could not create mouse event")
    }
    event.post(tap: .cghidEventTap)
}

func key(_ code: CGKeyCode, flags: CGEventFlags = [], text: [UniChar]? = nil) throws {
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
    down.post(tap: .cghidEventTap)
    up.post(tap: .cghidEventTap)
}

func typeText(_ text: String) throws {
    guard !text.isEmpty, text.utf8.count <= 4096 else { try fail("invalid_action", "text must contain 1–4096 bytes") }
    // Keep surrogate pairs together and stay below Quartz's 20 UTF-16-unit event limit.
    var buffer: [UniChar] = []
    for scalar in text.unicodeScalars {
        let units = Array(String(scalar).utf16)
        if buffer.count + units.count > 20 {
            try key(0, text: buffer)
            // AppKit may merge adjacent synthetic text chunks without input-event spacing.
            Thread.sleep(forTimeInterval: 0.05)
            buffer.removeAll(keepingCapacity: true)
        }
        buffer.append(contentsOf: units)
    }
    if !buffer.isEmpty { try key(0, text: buffer) }
}

func sendKey(_ request: [String: Any]) throws {
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
    try key(CGKeyCode(code), flags: flags)
}

@available(macOS 14.0, *)
func snapshot(_ request: [String: Any]) async throws -> [String: Any] {
    guard CGPreflightScreenCaptureAccess() else { try fail("screen_recording_denied", "grant Screen Recording to Bootty") }
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    let requested = (request["display_id"] as? NSNumber)?.uint32Value ?? CGMainDisplayID()
    guard let display = content.displays.first(where: { $0.displayID == requested }) else {
        try fail("invalid_action", "display is unavailable")
    }
    let filter = SCContentFilter(display: display, excludingWindows: [])
    let config = SCStreamConfiguration()
    let scale = min(1.0, 1600.0 / Double(display.width))
    config.width = max(1, Int(Double(display.width) * scale))
    config.height = max(1, Int(Double(display.height) * scale))
    config.showsCursor = true
    let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
    guard !secureInput() else { try fail("secure_input", "secure input became active during capture") }
    guard let data = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:]) else {
        try fail("capture_error", "could not encode screenshot")
    }
    let app = NSWorkspace.shared.frontmostApplication
    var result: [String: Any] = ["result": "snapshot", "png_base64": data.base64EncodedString(),
        "pixel_width": image.width, "pixel_height": image.height, "display_id": display.displayID,
        "bounds": boundsDictionary(CGDisplayBounds(display.displayID)), "elements": accessibilityElements(app)]
    if let name = app?.localizedName { result["application"] = name }
    if let bundle = app?.bundleIdentifier { result["bundle_id"] = bundle }
    return result
}

func execute(_ request: [String: Any]) async throws -> [String: Any] {
    guard request["enabled"] as? Bool == true else { try fail("disabled", "computer use is disabled") }
    guard AXIsProcessTrusted() else { try fail("accessibility_denied", "grant Accessibility to Bootty") }
    guard !secureInput() else { try fail("secure_input", "secure input is active") }
    switch request["action"] as? String {
    case "snapshot":
        if #available(macOS 14.0, *) { return try await snapshot(request) }
        try fail("unsupported", "screenshots require macOS 14 or later")
    case "click":
        let location = try point(request)
        switch request["button"] as? String {
        case "left": try mouse(location, .leftMouseDown, .left); try mouse(location, .leftMouseUp, .left)
        case "right": try mouse(location, .rightMouseDown, .right); try mouse(location, .rightMouseUp, .right)
        case "middle": try mouse(location, .otherMouseDown, .center); try mouse(location, .otherMouseUp, .center)
        default: try fail("invalid_action", "unknown mouse button")
        }
    case "move": try mouse(point(request), .mouseMoved, .left)
    case "type_text":
        guard let text = request["text"] as? String else { try fail("invalid_action", "text is required") }
        try typeText(text)
    case "key": try sendKey(request)
    case "scroll":
        let location = try point(request)
        guard let dx = request["delta_x"] as? Int32, let dy = request["delta_y"] as? Int32,
              dx >= -10000, dx <= 10000, dy >= -10000, dy <= 10000,
              let event = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 2,
                                  wheel1: dy, wheel2: dx, wheel3: 0) else {
            try fail("invalid_action", "invalid scroll delta")
        }
        event.location = location
        event.post(tap: .cghidEventTap)
    case "activate":
        guard let identifier = request["bundle_id"] as? String, !identifier.isEmpty,
              identifier.utf8.count <= 255 else { try fail("invalid_action", "invalid bundle identifier") }
        guard let app = NSRunningApplication.runningApplications(withBundleIdentifier: identifier).first,
              app.activate(options: []) else { try fail("application_unavailable", "application could not be activated") }
    default: try fail("invalid_action", "unknown computer action")
    }
    return ["result": "posted"]
}

@main
struct ComputerHelper {
    static func main() async {
        var response: [String: Any]
        do {
            let data = FileHandle.standardInput.readDataToEndOfFile()
            guard data.count <= 131072, let request = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                try fail("invalid_action", "invalid request")
            }
            switch request["method"] as? String {
            case "status": response = ["value": permissionStatus()]
            case "request_permission":
                switch request["permission"] as? String {
                case "accessibility":
                    _ = AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary)
                    if !AXIsProcessTrusted(), let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility") {
                        NSWorkspace.shared.open(url)
                    }
                case "screen_recording":
                    if !CGRequestScreenCaptureAccess(), let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") {
                        NSWorkspace.shared.open(url)
                    }
                default: try fail("invalid_action", "unknown permission")
                }
                response = ["value": permissionStatus()]
            case "execute": response = ["value": try await execute(request)]
            default: try fail("invalid_action", "unknown method")
            }
        } catch let failure as Failure {
            response = ["error": ["code": failure.code, "message": failure.message]]
        } catch {
            response = ["error": ["code": "platform_error", "message": error.localizedDescription]]
        }
        if let data = try? JSONSerialization.data(withJSONObject: response, options: [.sortedKeys]) {
            FileHandle.standardOutput.write(data)
        }
    }
}
