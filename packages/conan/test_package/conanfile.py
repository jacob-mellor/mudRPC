"""Conan's install check: a C program against the package, compiled with the system compiler, prints the block size the ABI reports."""
import os
import subprocess

from conan import ConanFile


class Test(ConanFile):
    settings = "os", "arch", "compiler", "build_type"

    def requirements(self):
        self.requires(self.tested_reference_str)

    def test(self):
        info = self.dependencies["mudrpc"].cpp_info
        src = os.path.join(self.recipe_folder, "main.c")
        exe = os.path.join(self.build_folder, "main")
        subprocess.run(["cc", "-o", exe, src] + [f"-I{d}" for d in info.includedirs] + [f"-L{d}" for d in info.libdirs] + ["-lmudrpc"] + [f"-l{l}" for l in info.system_libs], check=True)
        out = subprocess.run([exe], check=True, capture_output=True, text=True).stdout
        assert out.startswith("mud_size "), out
