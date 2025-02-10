from typing import Dict, List
from cealn.config import CompilationMode, Debug, Fastbuild, Optimized

from cealn.exec import Executable
from cealn.label import Label
from cealn.platform import Arch, Cuda, Linux, Os, Wasm32, Windows, Wasi
from cealn.provider import Provider, Field

from ..config import CrtLinkage, CrtDynamic, CrtStatic


class CcToolchain(Provider):
    def dylib_name(self, build_config, name: str) -> str:
        raise NotImplementedError

    def cflags(self, build_config) -> List[str]:
        raise NotImplementedError

    def cxxflags(self, build_config) -> List[str]:
        raise NotImplementedError


class LLVMToolchain(CcToolchain):
    clang = Field(Executable)
    clang_pp = Field(Executable)
    clang_cl = Field(Executable)
    lld = Field(Executable)
    lld_link = Field(Executable)
    llvm_ar = Field(Executable)
    llvm_lib = Field(Executable)
    wasm_ld = Field(Executable)

    def cc(self, build_config) -> Executable:
        if build_config[Os] == Windows:
            return self.clang_cl
        else:
            return self.clang

    def cxx(self, build_config) -> Executable:
        if build_config[Os] == Windows:
            return self.clang_cl
        else:
            return self.clang_pp

    def linker(self, build_config) -> Executable:
        if build_config[Os] == Windows:
            return self.lld_link
        elif build_config[Arch] == Wasm32 and build_config[Os] != Wasi:
            return self.wasm_ld
        else:
            return self.clang

    def ar(self, build_config, force_unix=False) -> Executable:
        if build_config[Os] == Windows and not force_unix:
            return self.llvm_lib
        else:
            return self.llvm_ar

    def dylib_name(self, build_config, name: str) -> str:
        if build_config[Os] == Linux:
            return f"lib{name}.so"
        elif build_config[Os] == Cuda:
            return f"{name}.cubin"
        elif build_config[Arch] == Wasm32:
            return f"{name}.wasm"
        else:
            raise RuntimeError("unsupported target")

    def exe_name(self, build_config, name: str) -> str:
        if build_config[Arch] == Wasm32:
            return f"{name}.wasm"
        elif build_config[Os] == Windows:
            return f"{name}.exe"
        else:
            return name

    def cflags(self, build_config) -> List[str]:
        flags = list(self._clang_flags(build_config))
        return flags

    def cxxflags(self, build_config) -> List[str]:
        flags = list(self._clang_flags(build_config))
        cc = self.cc(build_config)
        if cc.name == "clang-cl":
            flags += ["/std:c++20"]
        elif cc.name == "clang":
            flags += [
                "-stdlib=libc++",
                "-std=c++20",
            ]
        return flags

    def _clang_flags(self, build_config) -> List[str]:
        from ..config import llvm_target_triple

        target = llvm_target_triple(build_config)
        cc = self.cc(build_config)
        if cc.name == "clang":
            flags = [
                "-target",
                target,
                "-Wno-builtin-macro-redefined",
                "-D__TIME__=",
                "-D__DATE__=",
                "-D__TIMESTAMP_=",
                "-ffunction-sections",
                "-fdata-sections",
                "-fdebug-default-version=4",
                "-fcolor-diagnostics",
            ]
            if build_config[CompilationMode] == Optimized:
                if build_config[Arch] == Wasm32:
                    flags += ["-Oz"]
                else:
                    flags += ["-O3"]
                flags += ["-flto=thin", "-DNDEBUG"]
            else:
                flags += ["-O0", "-DDEBUG"]
            if build_config[CompilationMode] == Debug:
                flags += ["-g3"]
            else:
                flags += ["-gline-tables-only"]
        elif cc.name == "clang-cl":
            flags = [
                "/nologo",
                "-target",
                target,
                "-Wno-builtin-macro-redefined",
                "/D__TIME__=",
                "/D__DATE__=",
                "/D__TIMESTAMP__=",
                "/Brepro",
                "/Zc:inline",
                "/utf-8",
                "-fcolor-diagnostics",
            ]
            if build_config[CompilationMode] == Optimized:
                flags += ["/O2", "/Ob2", "/Oy-", "-flto=thin", "/DNDEBUG"]
            else:
                flags += ["/DDEBUG"]
            if build_config[CompilationMode] == Debug:
                flags += ["/Z7"]
            else:
                flags += ["/Z7", "-gline-tables-only"]
            crt_linkage = CrtLinkage.get_or_default(build_config)
            if crt_linkage == CrtDynamic:
                flags += ["/MD"]
            elif crt_linkage == CrtStatic:
                flags += ["/MT"]
        if build_config[Os] == Linux:
            flags += ["--sysroot", "%[srcdir]/.sysroot"]
        if build_config[Os] == Wasi:
            flags += [
                "--sysroot",
                "%[srcdir]/.sysroot",
            ]
        return flags

    def linker_output_flags(self, build_config, output_filename: str) -> List[str]:
        linker = self.linker(build_config)
        if linker.name == "clang" or linker.name == "wasm-ld":
            return ["-o", output_filename]
        elif linker.name == "lld-link":
            return [f"/OUT:{output_filename}"]
        else:
            raise RuntimeError("unsupported linker")

    def linker_flags(self, build_config) -> List[str]:
        from ..config import llvm_target_triple

        flags = []

        linker = self.linker(build_config)
        if linker.name == "clang":
            target = llvm_target_triple(build_config)
            flags += ["-Wl,--gc-sections", f"--target={target}"]
            if build_config[Arch] != Wasm32:
                flags += [
                    "-fuse-ld=lld",
                ]
        elif linker.name == "lld-link":
            flags += [
                "/Brepro",
                "/pdbaltpath:%_PDB%",
            ]
        if build_config[Os] == Cuda:
            flags += [
                "--cuda-gpu-arch=sm_89",
            ]
        if linker.name == "clang" or linker.name == "wasm-ld":
            if build_config[CompilationMode] == Optimized:
                flags += [
                    "-O3",
                ]
            elif build_config[CompilationMode] == Debug:
                flags += [
                    "-O0",
                ]
            elif build_config[CompilationMode] == Fastbuild:
                flags += [
                    "-O0",
                ]
        elif linker.name == "lld-link":
            if build_config[CompilationMode] == Optimized:
                flags += [
                    "/opt:lldlto=3",
                ]
            else:
                flags += [
                    "/opt:lldlto=0",
                ]
            if build_config[CompilationMode] == Debug:
                flags += ["/opt:noref", "/opt:noicf"]
            else:
                flags += ["/opt:ref", "/opt:icf"]
            if build_config[CompilationMode] == Fastbuild:
                flags += ["/DEBUG"]
            else:
                flags += ["/DEBUG:FULL"]
        if build_config[Os] == Linux:
            flags += [
                "--sysroot",
                "%[srcdir]/.sysroot",
                # FIXME: detect when these are needed
                "-lpthread",
                "-ldl",
                "-lm",
            ]
        if build_config[Os] == Wasi:
            flags += [
                "--sysroot",
                "%[srcdir]/.sysroot",
            ]
        return flags

    def add_extra_inputs(self, build_config, depmap):
        if build_config[Os] == Windows:
            # FIXME: find this
            depmap[".windows"] = Label("@io.hardscience//toolchains/windows:downloaded:sdk")
        elif build_config[Os] == Linux:
            # FIXME: find this
            depmap[".sysroot"] = Label("@io.hardscience//toolchains/cc:sysroot:sysroot")
        elif build_config[Os] == Wasi:
            # FIXME: find this
            depmap[".sysroot"] = Label("@io.hardscience//toolchains/cc:wasi_sysroot:sysroot")

    def cc_env(self, build_config) -> Dict[str, str]:
        envs = {}
        if build_config[Os] == Windows:
            include_subpaths = [
                "sdk/include/shared",
                "sdk/include/um",
                "sdk/include/ucrt",
                "crt/include",
            ]
            envs["INCLUDE"] = ";".join(f"%[srcdir]/.windows/{subpath}" for subpath in include_subpaths)
        return envs

    def linker_env(self, build_config) -> Dict[str, str]:
        envs = {}
        if build_config[Os] == Windows:
            lib_subpaths = [
                "sdk/lib/um/x86_64",
                "sdk/lib/ucrt/x86_64",
                "crt/lib/x86_64",
            ]
            envs["LIB"] = ";".join(f"%[srcdir]/.windows/{subpath}" for subpath in lib_subpaths)
        return envs
