// swift-tools-version:6.1
// Sensor telemetry over UDP in Swift: the binding's own API with SwiftProtobuf messages, the binding and the C header referenced from the repository, not copied.
import PackageDescription

let package = Package(
  name: "sensors",
  platforms: [.macOS(.v15)],
  dependencies: [.package(url: "https://github.com/apple/swift-protobuf.git", exact: "1.39.0")],
  targets: [
    .systemLibrary(name: "CMudRPC", path: "Sources/CMudRPC"),
    .executableTarget(name: "Sensors", dependencies: ["CMudRPC", .product(name: "SwiftProtobuf", package: "swift-protobuf")]),
  ]
)
