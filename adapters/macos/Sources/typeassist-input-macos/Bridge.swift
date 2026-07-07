import Foundation

struct Modifiers: Codable {
    var shift: Bool
    var control: Bool
    var option: Bool
    var command: Bool
    var capsLock: Bool
    var function: Bool

    enum CodingKeys: String, CodingKey {
        case shift, control, option, command
        case capsLock = "caps_lock"
        case function
    }
}

enum InputEvent {
    case key(key: String, timestampMs: UInt64, modifiers: Modifiers, dwellMs: UInt32)
    case backspace(timestampMs: UInt64)
    /// Isolated Shift tap — the accept gesture for a correction suggestion.
    /// Content-free. See `InputEvent::ShiftTap` in `events.rs`.
    case shiftTap(timestampMs: UInt64)
    case permissionRequired
    case ready
    case shutdown
    /// Periodic proof-of-life — engine watchdog uses these to detect a
    /// silent sidecar (no events flowing) independent of whether the
    /// user is currently typing. `tapEnabled` is our own view of the
    /// CGEvent tap state.
    case heartbeat(timestampMs: UInt64, tapEnabled: Bool)
    /// Per-grant permission snapshot — the two independent TCC grants capture
    /// needs (Accessibility for the AX API, Input Monitoring for the tap). Both
    /// are read-only no-prompt checks. Emitted BEFORE the startup permission
    /// gates (so a pending grant is visible even though the process is about to
    /// exit) and on each heartbeat. See `InputEvent::PermissionStatus` in
    /// `crates/behavioural-model/src/events.rs`.
    case permissionStatus(accessibility: Bool, inputMonitoring: Bool)
    /// The caret may have moved somewhere the engine can't dead-reckon — a
    /// mouse/trackpad click (from the tap) or a focus/app change (from the
    /// secure-field AX observer). Content-free: `reason` is only a log tag —
    /// `mouse`, `app` (real application switch), or `focus-element` (in-app
    /// focused-element re-publish; noisy in rich-text/web surfaces). Never an
    /// app or element identity (Principle #8).
    case caretMoved(reason: String)
    /// **An auto-repeated keystroke the adapter did NOT forward.** Holding a
    /// (non-backspace) key fires repeated keyDowns but the adapter only emits
    /// that key once, on keyUp — so the repeats are lost. This content-free
    /// marker lets the engine's funnel count that loss (Principle #7: capture
    /// must see its own drop). Carries nothing — no key, no count, no timing.
    case autorepeatDropped
    /// **Space keyDown observed (capture-integrity probe, QA-15).** Emitted on
    /// each deliberate (non-auto-repeat) space keyDown, carrying the running
    /// count. The keyDown is seen reliably even when the space's text key
    /// (emitted on keyUp) is lost, so the engine can reconcile this against the
    /// spaces it actually receives to flag a dropped space. Observe-only;
    /// content-free (a count, never the character). See `InputEvent::SpaceObserved`.
    case spaceObserved(total: UInt64)
    /// **Injection dead-zone (TF-08, host-scoped suppression).** `dead=true` when
    /// the focused field is a place a synthetic correction can't land — the
    /// confirmed case is a Safari web/contenteditable surface on Intel (phantom AX
    /// caret). The engine goes watch-only there (keeps learning, withholds the
    /// bubble). Level signal, emitted on focus change only when it flips.
    /// Content-free (a single bool). See `InputEvent::InjectionZone`.
    case injectionZone(dead: Bool)
}

enum OutboundCommand {
    case injectCorrection(deleteCount: Int, replacement: String)
    /// Anchored replace — see `InputEvent::InjectAnchored` in `events.rs`.
    case injectAnchored(left: Int, deleteCount: Int, replacement: String, right: Int)
    case shutdown
    /// Tear down the current event tap and create a fresh one. Soft
    /// recovery path for the case where auto-re-enable hasn't worked
    /// (e.g. the tap port itself is in a bad state); the engine's
    /// "Restart capture" button drives this.
    case restartTap
    /// Phase 0 / M3 debug: run one content-blind text-geometry probe of the
    /// currently focused element and report the result on stderr. Rides the
    /// same proven command path as `injectCorrection`, so it exercises AX
    /// through the sidecar's working Accessibility grant.
    case axProbe
}

/// Line-delimited JSON over stdout (events) and stdin (commands).
/// Matches `crates/behavioural-model/src/events.rs`.
final class Bridge {
    private let stdoutQueue = DispatchQueue(label: "typeassist.bridge.stdout")

    func emit(_ event: InputEvent) {
        let json = Self.encode(event)
        stdoutQueue.async {
            FileHandle.standardOutput.write(Data((json + "\n").utf8))
        }
    }

    func runInputLoop(commandHandler: @escaping (OutboundCommand) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            let stdin = FileHandle.standardInput
            var buffer = Data()
            while true {
                let chunk = stdin.availableData
                if chunk.isEmpty {
                    // EOF — parent has gone away. Exit cleanly.
                    DispatchQueue.main.async { commandHandler(.shutdown) }
                    return
                }
                buffer.append(chunk)
                while let nlIndex = buffer.firstIndex(of: 0x0a) {
                    let lineData = buffer.subdata(in: 0..<nlIndex)
                    buffer.removeSubrange(0...nlIndex)
                    if let cmd = Self.decode(lineData) {
                        DispatchQueue.main.async { commandHandler(cmd) }
                    }
                }
            }
        }
    }

    private static func encode(_ event: InputEvent) -> String {
        let payload: [String: Any]
        switch event {
        case let .key(key, ts, mods, dwell):
            payload = [
                "type": "key",
                "key": key,
                "timestamp_ms": ts,
                "modifiers": [
                    "shift": mods.shift,
                    "control": mods.control,
                    "option": mods.option,
                    "command": mods.command,
                    "caps_lock": mods.capsLock,
                    "function": mods.function,
                ],
                "dwell_ms": dwell,
            ]
        case let .backspace(ts):
            payload = ["type": "backspace", "timestamp_ms": ts]
        case let .shiftTap(ts):
            payload = ["type": "shift_tap", "timestamp_ms": ts]
        case .permissionRequired:
            payload = ["type": "permission_required"]
        case .ready:
            payload = ["type": "ready"]
        case .shutdown:
            payload = ["type": "shutdown"]
        case let .heartbeat(ts, tapEnabled):
            payload = [
                "type": "heartbeat",
                "timestamp_ms": ts,
                "tap_enabled": tapEnabled,
            ]
        case let .permissionStatus(accessibility, inputMonitoring):
            payload = [
                "type": "permission_status",
                "accessibility": accessibility,
                "input_monitoring": inputMonitoring,
            ]
        case let .caretMoved(reason):
            payload = ["type": "caret_moved", "reason": reason]
        case .autorepeatDropped:
            payload = ["type": "autorepeat_dropped"]
        case let .spaceObserved(total):
            payload = ["type": "space_observed", "total": total]
        case let .injectionZone(dead):
            payload = ["type": "injection_zone", "dead": dead]
        }
        guard let data = try? JSONSerialization.data(withJSONObject: payload, options: []),
              let s = String(data: data, encoding: .utf8) else {
            return "{\"type\":\"shutdown\"}"
        }
        return s
    }

    private static func decode(_ data: Data) -> OutboundCommand? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let type = obj["type"] as? String else { return nil }
        switch type {
        case "inject_correction":
            guard let deleteCount = obj["delete_count"] as? Int,
                  let replacement = obj["replacement"] as? String else { return nil }
            return .injectCorrection(deleteCount: deleteCount, replacement: replacement)
        case "inject_anchored":
            guard let left = obj["left"] as? Int,
                  let deleteCount = obj["delete_count"] as? Int,
                  let replacement = obj["replacement"] as? String,
                  let right = obj["right"] as? Int else { return nil }
            return .injectAnchored(
                left: left, deleteCount: deleteCount, replacement: replacement, right: right)
        case "shutdown":
            return .shutdown
        case "restart_tap":
            return .restartTap
        case "ax_probe":
            return .axProbe
        default:
            return nil
        }
    }
}
