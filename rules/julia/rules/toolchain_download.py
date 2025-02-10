import sys
from pathlib import Path

from cealn.rule import Rule
from cealn.attribute import Attribute, GlobalProviderAttribute
from cealn.exec import Executable
from cealn.platform import Os, Arch, Linux, X86_64

from workspaces.com_cealn_cc.providers import LLVMToolchain

from ..providers import JuliaToolchain


class DownloadJuliaToolchain(Rule):
    version = Attribute[str]()

    async def analyze(self):
        os = _OS_MAP[self.build_config[Os]]
        arch_folder = _ARCH_FOLDER_MAP[self.build_config[Arch]]
        arch = _ARCH_MAP[self.build_config[Arch]]
        version_folder = ".".join(self.version.split(".")[:2])

        filename = "julia.tar.gz"
        archive_dl = self.download(
            f"https://julialang-s3.julialang.org/bin/{os}/{arch_folder}/{version_folder}/julia-{self.version}-{os}-{arch}.tar.gz",
            filename=filename,
            id="download",
        )
        archive_extract = self.extract(archive_dl.files / filename, strip_prefix=f"julia-{self.version}", id="extract")

        julia = Executable(
            name="julia",
            executable_path="%[execdir]/bin/julia",
            context=archive_extract.files,
            search_paths=["bin"],
            library_search_paths=["lib"],
        )

        return [JuliaToolchain(julia=julia), julia]


_OS_MAP = {
    Linux: "linux",
}

_ARCH_FOLDER_MAP = {
    X86_64: "x64",
}

_ARCH_MAP = {
    X86_64: "x86_64",
}
