"""The Conan recipe: the C ABI as a static library built by cargo from this tree, with mudrpc.h, mud.hpp and mud_grpc.hpp.
`conan create packages/conan --version <v>` builds it; packages/build.sh exports it."""
import os
import subprocess

from conan import ConanFile
from conan.tools.files import copy


class MudRpc(ConanFile):
    name = "mudrpc"
    description = "Mud RPC: gRPC for unstable networks. gRPC's service model over mesh, radio and UDP as sealed packets, without HTTP/2 or a server stack, and a Stub and Service for C++ so the code protoc generates runs with the include changed."
    license = "MIT"
    url = homepage = "https://github.com/jacob-mellor/mudRPC"
    topics = ("grpc", "rpc", "protobuf", "mesh", "udp", "delay-tolerant", "iot", "drone", "lora", "no-http2")
    settings = "os", "arch", "compiler", "build_type"
    package_type = "static-library"

    def export_sources(self):
        root = os.path.join(self.recipe_folder, "..", "..")   # the tree, or MUDRPC_TREE when the recipe is a copy outside it
        root = os.environ.get("MUDRPC_TREE", root)
        for f in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "LICENSE"]:
            copy(self, f, root, self.export_sources_folder)
        for d in ["core", "auth", "gcm", "adapters", "gen", "bindings/c", "bindings/cpp", "proto", "scenarios"]:
            copy(self, "*", os.path.join(root, d), os.path.join(self.export_sources_folder, d), excludes=["tests/*", "__pycache__/*"])

    def build(self):
        subprocess.run(["cargo", "build", "--locked", "--release", "-p", "mudrpc-c"], cwd=self.source_folder, check=True)

    def package(self):
        copy(self, "libmudrpc.a", os.path.join(self.source_folder, "target", "release"), os.path.join(self.package_folder, "lib"))
        copy(self, "mudrpc.h", os.path.join(self.source_folder, "bindings", "c"), os.path.join(self.package_folder, "include"))
        copy(self, "*.hpp", os.path.join(self.source_folder, "bindings", "cpp"), os.path.join(self.package_folder, "include"))
        copy(self, "SKILL.md", os.path.join(self.source_folder, "bindings", "cpp"), os.path.join(self.package_folder, "share", "mudrpc"))
        copy(self, "LICENSE", self.source_folder, os.path.join(self.package_folder, "licenses"))

    def package_info(self):
        self.cpp_info.libs = ["mudrpc"]
        if self.settings.os == "Linux":
            self.cpp_info.system_libs = ["pthread", "dl", "m"]   # what a Rust staticlib with std links
