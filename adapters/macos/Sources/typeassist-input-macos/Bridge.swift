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
    case permissionRequired
    case ready
    case shutdown
}

enum OutboundCommand {
    case injectCorrection(deleteCount: Int, replacement: String)
    case shutdown
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
        case .permissionRequired:
            payload = ["type": "permission_required"]
        case .ready:
            payload = ["type": "ready"]
        case .shutdown:
            payload = ["type": "shutdown"]
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
        case "shutdown":
            return .shutdown
        default:
            return nil
        }
    }
}
