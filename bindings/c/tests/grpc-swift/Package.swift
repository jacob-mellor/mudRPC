// swift-tools-version:6.1
// The Swift drop-in's test: grpc-swift 2's client and service for drone.proto, generated and run over mudRPC unchanged;
// Example is the Swift skill's step 2.
import PackageDescription

let package = Package(
  name: "mudrpc-grpc-swift-test",
  platforms: [.macOS(.v15)],
  dependencies: [
    .package(url: "https://github.com/grpc/grpc-swift-2.git", exact: "2.4.4"),
    .package(url: "https://github.com/grpc/grpc-swift-protobuf.git", exact: "2.5.0"),
    .package(url: "https://github.com/apple/swift-protobuf.git", exact: "1.39.0"),
  ],
  targets: [
    .systemLibrary(name: "CMudRPC", path: "Sources/CMudRPC"),
    .executableTarget(name: "Example", dependencies: ["CMudRPC"]),
    .executableTarget(name: "App", dependencies: [
      "CMudRPC",
      .product(name: "GRPCCore", package: "grpc-swift-2"),
      .product(name: "GRPCProtobuf", package: "grpc-swift-protobuf"),
      .product(name: "SwiftProtobuf", package: "swift-protobuf"),
    ]),
  ]
)
