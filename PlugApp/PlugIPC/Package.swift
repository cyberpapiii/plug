// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "PlugIPC",
    platforms: [.macOS(.v14)],
    products: [.library(name: "PlugIPC", targets: ["PlugIPC"])],
    targets: [
        .target(name: "PlugIPC"),
        .testTarget(name: "PlugIPCTests", dependencies: ["PlugIPC"]),
    ]
)
