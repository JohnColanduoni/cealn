import sys
from pathlib import Path
import re

from cealn.rule import Rule
from cealn.attribute import Attribute, GlobalProviderAttribute, FileAttribute, LabelAttribute, LabelMapAttribute
from cealn.exec import Executable
from cealn.platform import Os, Arch, Linux, X86_64
from cealn.depmap import DepmapBuilder

from ..providers import GoToolchain


class GoModule(Rule):
    module_file = FileAttribute(default=":go.mod")
    sum_file = FileAttribute(default=":go.sum")

    sources = LabelAttribute(default="")
    source_patterns = Attribute(default=["*.go"])
    commands = Attribute(default={})

    go_toolchain = GlobalProviderAttribute(GoToolchain, host=True)

    async def analyze(self):
        mod_input = self.new_depmap("mod-input")
        mod_input["go.mod"] = self.module_file
        mod_input["go.sum"] = self.sum_file
        mod_input = mod_input.build()

        with await self.open_file(self.module_file, encoding="utf-8") as f:
            module_file_contents = f.read()
        module_match = _MODULE_REGEX.search(module_file_contents)
        if module_match is None:
            raise RuntimeError("missing module name")
        module_name = module_match.group("module")

        envs = {
            "CGO_ENABLED": "0",
            "GOOS": _OS_MAP[self.build_config[Os]],
            "GOARCH": _ARCH_MAP[self.build_config[Arch]],
            "GOPATH": "%[srcdir]/go",
            "GOCACHE": "%[srcdir]/.go-cache",
        }

        mod_download = self.run(
            self.go_toolchain.go,
            "mod",
            "download",
            input=mod_input,
            append_env=envs,
            id="mod-download",
            mnemonic="GoModDownload",
            progress_message=module_name,
        )

        sources = self.new_depmap("sources")
        sources.merge(mod_input)
        sources.merge(mod_download.files)
        sources[""] = DepmapBuilder.glob(self.sources, *self.source_patterns)
        sources = sources.build()

        providers = []

        if self.commands:
            for exe_name, command_subpath in self.commands.items():
                build_command = self.run(
                    self.go_toolchain.go,
                    "build",
                    "-o",
                    exe_name,
                    command_subpath,
                    input=sources,
                    append_env=envs,
                    id=f"build-command-{exe_name}",
                    mnemonic="GoBuild",
                    progress_message=f"{module_name} {exe_name}",
                )

                exe_context = self.new_depmap(f"command-context-{exe_name}")
                exe_context[f"bin/{exe_name}"] = build_command.files / exe_name
                exe_context = exe_context.build()

                exe = Executable(
                    name=exe_name,
                    executable_path=f"%[execdir]/bin/{exe_name}",
                    context=exe_context,
                )
                providers.append(exe)
        else:
            exe_name = module_name.split("/")[-1]
            build = self.run(
                self.go_toolchain.go,
                "build",
                input=sources,
                append_env=envs,
                id="build",
                mnemonic="GoBuild",
                progress_message=module_name,
            )

            exe_context = self.new_depmap("exe-context")
            exe_context[f"bin/{exe_name}"] = build.files / exe_name
            exe_context = exe_context.build()

            exe = Executable(
                name=exe_name,
                executable_path=f"%[execdir]/bin/{exe_name}",
                context=exe_context,
            )
            providers.append(exe)

        return providers


_MODULE_REGEX = re.compile(r"^module\s+(?P<module>.+)$", re.MULTILINE)

_OS_MAP = {
    Linux: "linux",
}

_ARCH_MAP = {
    X86_64: "amd64",
}
