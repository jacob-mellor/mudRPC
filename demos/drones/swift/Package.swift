// swift-tools-version:6.1
// gRPC drone telemetry in Swift: grpc-swift 2 over mudRPC, the binding and the C header referenced from the repository, not copied.
import PackageDescription

let package = Package(
  name: "drones",
  platforms: [.macOS(.v15)],
  dependencies: [
    .package(url: "https://github.com/grpc/grpc-swift-2.git", exact: "2.4.4"),
    .package(url: "https://github.com/grpc/grpc-swift-protobuf.git", exact: "2.5.0"),
    .package(url: "https://github.com/apple/swift-protobuf.git", exact: "1.39.0"),
  ],
  targets: [
    .systemLibrary(name: "CMudRPC", path: "Sources/CMudRPC"),
    .executableTarget(name: "Drones", dependencies: [
      "CMudRPC",
      .product(name: "GRPCCore", package: "grpc-swift-2"),
      .product(name: "GRPCProtobuf", package: "grpc-swift-protobuf"),
      .product(name: "SwiftProtobuf", package: "swift-protobuf"),
    ]),
  ]
)
