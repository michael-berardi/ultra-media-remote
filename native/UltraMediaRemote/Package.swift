// swift-tools-version:5.9
// The swift-tools-version declares the minimum version of Swift required to build this package.
//
// UltraMediaRemote
// Runtime-only access to the macOS MediaRemote framework (Now Playing info and
// transport control) behind a C-compatible ABI callable from Rust.
//
// License: MIT (see /LICENSE in the repository root)

import PackageDescription

let package = Package(
    name: "UltraMediaRemote",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .library(
            name: "UltraMediaRemote",
            type: .static,
            targets: ["UltraMediaRemote"]
        )
    ],
    targets: [
        .target(
            name: "UltraMediaRemote",
            path: "Sources/UltraMediaRemote"
        ),
        .testTarget(
            name: "UltraMediaRemoteTests",
            dependencies: ["UltraMediaRemote"],
            path: "Tests/UltraMediaRemoteTests"
        )
    ]
)
