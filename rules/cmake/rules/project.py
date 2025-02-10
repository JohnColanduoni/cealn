import json
from pathlib import Path
import shlex
from cealn.config import CompilationMode, Optimized

from cealn.rule import Rule
from cealn.attribute import Attribute, FileAttribute, GlobalProviderAttribute, LabelListAttribute
from cealn.exec import Executable
from cealn.depmap import DepmapBuilder
from cealn.platform import Os, Linux, Arch, X86_64, Aarch64
from cealn.label import LabelPath

from workspaces.com_cealn_cc.providers import LLVMToolchain
from workspaces.com_cealn_ninja.providers import NinjaInput
from workspaces.com_cealn_ninja.rules import NinjaBuild

from ..providers.toolchain import CmakeToolchain


class CmakeProject(Rule):
    # FIXME: swap this with CCToolchain, handle inheritance properly
    toolchain = GlobalProviderAttribute(CmakeToolchain, host=True)
    cc_toolchain = GlobalProviderAttribute(LLVMToolchain, host=True)

    cmake_lists = FileAttribute()
    input_files = FileAttribute()

    extra_build_tools = LabelListAttribute(default=[])

    defines = Attribute(default={})

    async def analyze(self):
        build_dir = "build"

        configure_input = self.new_depmap()
        configure_input["CMakeLists.txt"] = self.cmake_lists
        configure_input[".fake_ninja"] = DepmapBuilder.file(_FAKE_NINJA_SCRIPT, executable=True)
        configure_input["CMakeToolchain.txt"] = DepmapBuilder.file(
            await self.generate_toolchain_file(), executable=True
        )
        configure_input[build_dir] = DepmapBuilder.directory()
        configure_input.merge(self.input_files)
        configure_input[f"{build_dir}/.cmake/api/v1/query/codemodel-v2"] = DepmapBuilder.file("")
        self.cc_toolchain.add_extra_inputs(self.build_config, configure_input)
        configure_input = configure_input.build()

        if self.build_config[CompilationMode] == Optimized:
            build_type = "ReleaseWithDebInfo"
        else:
            build_type = "Debug"

        cmake = self.toolchain.cmake.add_dependency_executable(self, self.cc_toolchain.clang)
        for tool_label in self.extra_build_tools:
            tool_executables = [
                provider
                for provider in await self.load_providers(tool_label, host=True)
                if isinstance(provider, Executable)
            ]
            cmake = cmake.add_dependency_executable(self, *tool_executables)

        configure = self.run(
            cmake,
            "../",
            "-G",
            "Ninja",
            "-DCMAKE_MAKE_PROGRAM=%[srcdir]/.fake_ninja",
            "-DCMAKE_TOOLCHAIN_FILE=%[srcdir]/CMakeToolchain.txt",
            f"-DCMAKE_BUILD_TYPE={build_type}",
            "-DCMAKE_SUPPRESS_REGENERATION=ON",
            "-Wno-dev",
            *(f"-D{k}={v}" for k, v in self.defines.items()),
            cwd=f"%[srcdir]/{build_dir}",
            input=configure_input,
            id="configure",
            mnemonic="Cmake",
            progress_message=str(self.cmake_lists),
        )

        configure_output = await configure

        providers = []
        reply_dir = LabelPath(f"{build_dir}/.cmake/api/v1/reply")
        for reply_filename in await configure_output.files.iterdir(reply_dir):
            with await configure_output.files.open_file(reply_dir / reply_filename, encoding="utf-8") as f:
                reply = json.load(f)
            if "type" not in reply:
                continue
            if reply["type"] == "EXECUTABLE":
                executable_context = self.new_depmap(reply["name"])
                executable_context["bin"] = (self.label / "ninja").join_action(reply["name"]) / "build" / "bin"
                executable_context = executable_context.build()
                providers.append(
                    Executable(
                        name=reply["nameOnDisk"],
                        executable_path=f"%[execdir]/bin/{reply['nameOnDisk']}",
                        context=executable_context,
                    )
                )

        ninja_input = self.new_depmap()
        ninja_input.merge(configure.files)
        ninja_input.merge(self.input_files)
        self.cc_toolchain.add_extra_inputs(self.build_config, ninja_input)
        ninja_input = ninja_input.build()

        exec_context = self.new_depmap()
        exec_context.merge(self.toolchain.cmake.context)
        exec_context.merge(self.cc_toolchain.clang.context)
        exec_context = exec_context.build()

        self.synthetic_target(NinjaBuild, "ninja", input=self.label)

        return [NinjaInput(input=ninja_input, exec_context=exec_context, build_root="build", append_env={}), *providers]

    async def generate_toolchain_file(self):
        toolchain_file = ""

        if self.build_config[Os] == Linux:
            toolchain_file += "set(CMAKE_SYSTEM_NAME Linux)\n"
        else:
            raise RuntimeError("unsupported platform")
        toolchain_file += f"set(CMAKE_SYSTEM_PROCESSOR {_ARCH_MAP[self.build_config[Arch]]})\n"
        if self.build_config[Os] == Linux:
            toolchain_file += await self.substitute_for_execution("set(CMAKE_SYSROOT %[srcdir]/.sysroot)\n")

        cxx_compiler = await self.substitute_for_execution(self.cc_toolchain.clang.executable_path)
        toolchain_file += f"set(CMAKE_C_COMPILER {cxx_compiler})\n"
        toolchain_file += f"set(CMAKE_CXX_COMPILER {cxx_compiler})\n"

        toolchain_file += f"set(CMAKE_C_FLAGS_INIT {json.dumps(' '.join(shlex.quote(arg) for arg in self.cc_toolchain.cflags(self.build_config)))})\n"
        toolchain_file += f"set(CMAKE_CXX_FLAGS_INIT {json.dumps(' '.join(shlex.quote(arg) for arg in self.cc_toolchain.cxxflags(self.build_config)))})\n"

        linker_flags = self.cc_toolchain.linker_flags(self.build_config)
        # FIXME: hack
        linker_flags += ["-lc++", "-lc++abi", "-lsupc++"]
        linker_flags = json.dumps(" ".join(shlex.quote(arg) for arg in linker_flags))
        toolchain_file += f"set(CMAKE_EXE_LINKER_FLAGS_INIT {linker_flags})\n"
        toolchain_file += f"set(CMAKE_SHARED_LINKER_FLAGS_INIT {linker_flags})\n"
        toolchain_file += f"set(CMAKE_MODULE_LINKER_FLAGS_INIT {linker_flags})\n"

        toolchain_file += "set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)\n"
        toolchain_file += "set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)\n"
        toolchain_file += "set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)\n"
        toolchain_file += "set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)\n"

        return toolchain_file


_ARCH_MAP = {
    X86_64: "x86_64",
    Aarch64: "aarch64",
}

_FAKE_NINJA_SCRIPT = """#! /bin/bash
echo "1.11.1"
"""
