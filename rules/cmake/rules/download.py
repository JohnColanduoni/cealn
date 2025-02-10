import json
import sys
from pathlib import Path
import re

from cealn.rule import Rule
from cealn.attribute import Attribute
from cealn.exec import Executable
from cealn.platform import Os, Arch, Linux, X86_64, Aarch64

from ..providers.toolchain import CmakeToolchain


class DownloadCmakeToolchain(Rule):
    version = Attribute[str]()
    ninja_version = Attribute[str](default="1.11.1")

    async def analyze(self):
        os = _OS_MAP[self.build_config[Os]]
        ninja_os = _NINJA_OS_MAP[self.build_config[Os]]
        arch = _ARCH_MAP[self.build_config[Arch]]

        archive_url = f"https://github.com/Kitware/CMake/releases/download/v{self.version}/cmake-{self.version}-{os}-{arch}.tar.gz"
        archive_filename = "cmake.tar.gz"
        archive_dl = self.download(archive_url, filename=archive_filename)
        archive_extract = self.extract(
            archive_dl.files / archive_filename, strip_prefix=f"cmake-{self.version}-{os}-{arch}"
        )

        ninja_archive_url = (
            f"https://github.com/ninja-build/ninja/releases/download/v{self.ninja_version}/ninja-{ninja_os}.zip"
        )
        ninja_archive_filename = "ninja.zip"
        ninja_archive_dl = self.download(ninja_archive_url, filename=ninja_archive_filename)
        ninja_archive_extract = self.extract(ninja_archive_dl.files / ninja_archive_filename)

        context = self.new_depmap("context")
        context.merge(archive_extract.files)
        context["bin/ninja"] = ninja_archive_extract.files / "ninja"
        context = context.build()

        toolchain = CmakeToolchain(
            cmake=Executable(
                name="cmake", context=context, executable_path="%[execdir]/bin/cmake", search_paths=["bin"]
            ),
        )

        return [
            toolchain,
            toolchain.cmake,
        ]


_MAIN_ARCHIVE_REGEX = re.compile(
    r"^(?P<stem>clang\+llvm-(?P<version>\d+\.\d+\.\d+)-(?P<target>.+?)(-ubuntu-(\d+\.\d+))?)\.tar\.xz$"
)

_ARCH_MAP = {
    Aarch64: "aarch64",
    X86_64: "x86_64",
}

_OS_MAP = {
    Linux: "linux",
}

_NINJA_OS_MAP = {
    Linux: "linux",
}
