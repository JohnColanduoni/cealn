import sys
from pathlib import Path
import re

from cealn.rule import Rule
from cealn.attribute import Attribute, FileAttribute, GlobalProviderAttribute
from cealn.exec import Executable
from cealn.platform import Os, Arch, Linux, X86_64

from workspaces.com_cealn_cc.providers import LLVMToolchain

from ..providers import JuliaToolchain


class JuliaApp(Rule):
    project_toml = FileAttribute(default=":Project.toml")

    toolchain = GlobalProviderAttribute(JuliaToolchain)

    llvm = GlobalProviderAttribute(LLVMToolchain)

    async def analyze(self):
        julia = self.toolchain.julia.add_dependency_executable(self, self.llvm.clang)

        package_input = self.new_depmap("package-input")
        package_input.merge(self.project_toml.parent)
        package_input = package_input.build()

        compile_command = """
        using Pkg
        Pkg.instantiate()
        using PackageCompiler
        PackageCompiler.create_app(".", "app", cpu_target=ENV["APP_CPU_TARGET"])
        """.strip()
        compile_command = re.sub(r"\n\s*", ";", compile_command)
        self.run(
            julia,
            "--project=.",
            "--eval",
            compile_command,
            input=package_input,
            # FIXME: source this from build config
            # FIXME: bump this to x86-64-v4
            append_env={"APP_CPU_TARGET": "x86-64-v3"},
            id="package",
        )
