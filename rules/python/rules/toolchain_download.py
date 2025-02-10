import sys
from pathlib import Path

from cealn.rule import Rule
from cealn.attribute import Attribute
from cealn.exec import Executable
from cealn.depmap import DepmapBuilder

from workspaces.com_cealn_cc.config import llvm_target_triple

from ..providers import PythonToolchain


class DownloadPythonToolchain(Rule):
    version = Attribute[str]()

    async def analyze(self):
        target = llvm_target_triple(self.build_config)
        filename = "cpython.tar.zst"
        download = self.download(
            f"https://github.com/indygreg/python-build-standalone/releases/download/20230116/cpython-{self.version}+20230116-{target}-lto-full.tar.zst",
            filename=filename,
        )
        extracted = self.extract(download.files / filename, strip_prefix="python/install")

        context = self.new_depmap()
        context.merge(extracted.files)
        context["bin/python"] = DepmapBuilder.symlink("./python3")
        context = context.build()

        python = Executable(
            name="python",
            executable_path="%[execdir]/bin/python3",
            context=context,
            search_paths=["bin"],
            library_search_paths=["lib"],
        )

        return [
            PythonToolchain(python=python),
            python,
        ]
