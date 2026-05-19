// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "typeassist-input-macos",
    platforms: [.macOS(.v13)],
    targets: [
        .executableTarget(
            name: "typeassist-input-macos",
            path: "Sources/typeassist-input-macos"
        )
    ]
)
