import sys
from pathlib import Path

from cealn.rule import Rule
from cealn.attribute import Attribute, GlobalProviderAttribute
from cealn.exec import Executable
from cealn.platform import Os, Arch, Linux, X86_64

from ..providers import GoToolchain


class DownloadGoToolchain(Rule):
    version = Attribute[str]()

    async def analyze(self):
        os = _OS_MAP[self.build_config[Os]]
        arch = _ARCH_MAP[self.build_config[Arch]]

        filename = "go.tar.gz"
        archive_dl = self.download(
            f"https://go.dev/dl/go{self.version}.{os}-{arch}.tar.gz",
            filename=filename,
            id="download",
        )
        archive_extract = self.extract(archive_dl.files / filename, strip_prefix="go", id="context")

        go = Executable(
            name="go",
            executable_path="%[execdir]/bin/go",
            context=archive_extract.files,
            search_paths=["bin"],
        )

        return [GoToolchain(go=go), go]


_OS_MAP = {
    Linux: "linux",
}

_ARCH_MAP = {
    X86_64: "amd64",
}
