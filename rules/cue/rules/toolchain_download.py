import sys
from pathlib import Path

from cealn.rule import Rule
from cealn.attribute import Attribute, GlobalProviderAttribute
from cealn.exec import Executable
from cealn.platform import Os, Arch, Linux, X86_64

from ..providers import CueToolchain


class DownloadCueToolchain(Rule):
    version = Attribute[str]()

    async def analyze(self):
        os = _OS_MAP[self.build_config[Os]]
        arch = _ARCH_MAP[self.build_config[Arch]]

        filename = "cue.tar.gz"
        archive_dl = self.download(
            f"https://github.com/cue-lang/cue/releases/download/v{self.version}/cue_v{self.version}_{os}_{arch}.tar.gz",
            filename=filename,
            id="download",
        )
        archive_extract = self.extract(archive_dl.files / filename, id="extract")

        context = self.new_depmap("context")
        context["bin/cue"] = archive_extract.files / "cue"
        context = context.build()

        cue = Executable(
            name="cue",
            executable_path="%[execdir]/bin/cue",
            context=context,
            search_paths=["bin"],
        )

        return [CueToolchain(cue=cue), cue]


_OS_MAP = {
    Linux: "linux",
}

_ARCH_MAP = {
    X86_64: "amd64",
}
