// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "typeassist-input-macos",
    platforms: [.macOS(.v13)],
    targets: [
        .executableTarget(
            name: "typeassist-input-macos",
            path: "Sources/typeassist-input-macos",
            // Embed Resources/Info.plist into the Mach-O so macOS shows a
            // friendly name ("TenCalmDigits Input") in Privacy & Security →
            // Input Monitoring. The sidecar is an unbundled binary, so a
            // __TEXT,__info_plist section is the only way to set its display
            // name. Path is relative to the package root (swift build's cwd).
            linkerSettings: [
                .unsafeFlags([
                    "-Xlinker", "-sectcreate",
                    "-Xlinker", "__TEXT",
                    "-Xlinker", "__info_plist",
                    "-Xlinker", "Resources/Info.plist",
                ])
            ]
        )
    ]
)
