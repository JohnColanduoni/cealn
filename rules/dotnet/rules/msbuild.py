from pathlib import Path
from typing import List

from cealn.config import CompilationMode, Optimized
from cealn.label import LabelPath
from cealn.rule import Rule
from cealn.attribute import Attribute, GlobalProviderAttribute, FileAttribute, LabelAttribute
from cealn.exec import Executable
from cealn.platform import Os, Linux, Arch, Aarch64
from cealn.depmap import DepmapBuilder

from ..providers import DotNetToolchain


class MSBuild(Rule):
    project_file = FileAttribute()

    source_root = LabelAttribute(default="//")
    source_patterns = Attribute[List[str]](default=["*.cs", "*.json", "*.proto"])

    output_directory = Attribute(default=None)

    toolchain = GlobalProviderAttribute(DotNetToolchain, host=True)

    async def analyze(self):
        root_subpath = LabelPath(str(self.project_file.parent.relative_to(self.label.parent / self.source_root)))

        envs = {
            "DOTNET_NOLOGO": "1",
            "DOTNET_CONSOLE_ANSI_COLOR": "1",
        }

        restore_input = self.new_depmap("restore-input")
        restore_input[root_subpath / self.project_file.file_name] = self.project_file
        restore_input[root_subpath / "packages.lock.json"] = self.project_file.parent / "packages.lock.json"
        restore_input = restore_input.build()

        restore = self.run(
            self.toolchain.runner,
            "restore",
            "--packages",
            "%[srcdir]/.nuget/packages",
            "--use-lock-file",
            "--locked-mode",
            "--v:q",
            self.project_file.file_name,
            input=restore_input,
            cwd=root_subpath,
            append_env=envs,
            id="restore",
            mnemonic="DotNetRestore",
            progress_message=str(self.project_file),
        )

        list_references = await self.run(
            self.toolchain.runner,
            "list",
            "reference",
            input=restore_input,
            cwd=root_subpath,
            append_env=envs,
            id="list-references",
            hide_stdout=True,
            mnemonic="DotNetListReferences",
            progress_message=str(self.project_file),
        )

        references = []
        with await list_references.open_stdout(encoding="utf-8") as f:
            for i, line in enumerate(f):
                if i < 2:
                    continue
                references.append(LabelPath(line.strip().replace("\\", "/")))

        sources = self.new_depmap("sources")
        sources[root_subpath / self.project_file.file_name] = self.project_file
        sources[root_subpath] = DepmapBuilder.glob(self.project_file.parent, *self.source_patterns)
        sources = sources.build()

        build_input = self.new_depmap("build-input")
        build_input.merge(sources)

        for reference_path in references:
            reference_root_subpath = (root_subpath / reference_path).normalize().parent
            reference_cealn_target = (self.source_root / reference_root_subpath).join_action("msbuild")
            build_input.merge(reference_cealn_target.join_action("sources"))
            build_input.merge(reference_cealn_target.join_action("build"))

        build_input.merge(restore.files)
        build_input = build_input.build()

        if self.build_config[CompilationMode] == Optimized:
            configuration = "Release"
        else:
            configuration = "Debug"

        # FIXME: use target arch/os

        build = self.run(
            self.toolchain.runner,
            "build",
            "--configuration",
            configuration,
            "--no-restore",
            "--no-dependencies",
            "--nologo",
            "--verbosity=quiet",
            self.project_file.file_name,
            input=build_input,
            cwd=root_subpath,
            append_env=envs,
            id="build",
            mnemonic="MsBuild",
            progress_message=str(self.project_file),
        )

        output_directory = self.output_directory or "bin"

        output = self.new_depmap("output")
        # FIXME: detect .net version to use here
        output.merge(build.files / root_subpath / output_directory / configuration / "net7.0")
        output.build()
