import sys
from pathlib import Path
from typing import List

from cealn.rule import Rule
from cealn.attribute import Attribute
from cealn.exec import Executable
from cealn.platform import Os, Linux, Arch, Aarch64, X86_64

from ..providers import DotNetToolchain


class DownloadDotNetToolchain(Rule):
    version = Attribute[str]()

    tools = Attribute[List[str]](default=["dotnet-ef"])

    async def analyze(self):
        try:
            os_name = _OS_MAP[self.build_config[Os]]
        except KeyError as ex:
            raise RuntimeError("unsupported OS") from ex
        try:
            arch_name = _ARCH_MAP[self.build_config[Arch]]
        except KeyError as ex:
            raise RuntimeError("unsupported arch") from ex
        filename = "dotnet-sdk.tar.gz"
        arch_dl = self.download(
            f"https://dotnetcli.azureedge.net/dotnet/Sdk/{self.version}/dotnet-sdk-{self.version}-{os_name}-{arch_name}.tar.gz",
            filename=filename,
            id="download",
        )
        arch_extract = self.extract(arch_dl.files / filename, id="install")

        # FIXME: lock versions

        tools_install_input = self.new_depmap("tools-install-input")
        tools_install_input[".dotnet"] = arch_extract.files
        tools_install_input = tools_install_input.build()
        tools_install = self.run(
            "%[srcdir]/.dotnet/dotnet",
            "tool",
            "install",
            "--tool-path",
            "%[srcdir]/.dotnet/tools",
            *self.tools,
            input=tools_install_input,
            id="tools-install",
        )

        context = self.new_depmap("context")
        context[".dotnet"] = arch_extract.files
        context[".dotnet/tools"] = tools_install.files / ".dotnet" / "tools"
        context = context.build()

        toolchain = DotNetToolchain(
            sdk=arch_extract.files / "sdk" / self.version,
            runner=Executable(
                name="dotnet",
                executable_path="%[execdir]/.dotnet/dotnet",
                context=context,
                search_paths=[".dotnet", ".dotnet/tools"],
            ),
        )

        return [toolchain, toolchain.runner]


_OS_MAP = {Linux: "linux"}

_ARCH_MAP = {X86_64: "x64", Aarch64: "arm64"}
