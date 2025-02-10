import json
import sys
from pathlib import Path
import re

from cealn.rule import Rule
from cealn.attribute import Attribute
from cealn.exec import Executable
from cealn.platform import Os, Arch, Linux, X86_64, Aarch64

from ..providers.cc_toolchain import LLVMToolchain


class DownloadLLVMToolchain(Rule):
    version = Attribute[str]()

    async def analyze(self):
        release_url = f"https://api.github.com/repos/llvm/llvm-project/releases/tags/llvmorg-{self.version}"
        release_dl = await self.download(release_url, filename="response.json", user_agent="curl/7.87.0")
        with await release_dl.files.open_file("response.json", encoding="utf-8") as release_file:
            release = json.load(release_file)

        try:
            target_arch = _ARCH_MAP[self.build_config[Arch]]
        except KeyError as ex:
            raise RuntimeError("unsupported architecture") from ex
        try:
            target_os = _OS_MAP[self.build_config[Os]]
        except KeyError as ex:
            raise RuntimeError("unsupported os") from ex
        target = f"{target_arch}-{target_os}"

        main_context = None

        for asset in release["assets"]:
            match = _MAIN_ARCHIVE_REGEX.fullmatch(asset["name"])
            if match:
                match_target = match.group("target")
                if match_target != target:
                    continue
                version = match.group("version")
                filename = "clang+llvm.tar.xz"
                archive_dl = self.download(asset["browser_download_url"], filename=filename, id="dl")
                archive_extracted = self.extract(
                    archive_dl.files / filename, strip_prefix=match.group("stem"), id="extract"
                )
                main_context = archive_extracted.files
                break

        if not main_context:
            raise RuntimeError("failed to find main LLVM artifact")

        context = self.new_depmap("context")
        context.merge(main_context)
        # FIXME: hack
        context["lib/clang/16/lib/wasi"] = "@io.hardscience//toolchains/cc:wasi_sysroot:context/lib/clang/16/lib/wasi"
        context = context.build()

        toolchain = LLVMToolchain(
            clang=Executable(name="clang", context=context, executable_path="%[execdir]/bin/clang"),
            clang_pp=Executable(name="clang++", context=context, executable_path="%[execdir]/bin/clang++"),
            clang_cl=Executable(name="clang-cl", context=context, executable_path="%[execdir]/bin/clang-cl"),
            lld=Executable(name="lld", context=context, executable_path="%[execdir]/bin/lld"),
            lld_link=Executable(name="lld-link", context=context, executable_path="%[execdir]/bin/lld-link"),
            llvm_ar=Executable(name="llvm-ar", context=context, executable_path="%[execdir]/bin/llvm-ar"),
            llvm_lib=Executable(name="llvm-lib", context=context, executable_path="%[execdir]/bin/llvm-lib"),
            wasm_ld=Executable(name="wasm-ld", context=context, executable_path="%[execdir]/bin/wasm-ld"),
        )

        return [
            toolchain,
            toolchain.clang,
            toolchain.lld,
            toolchain.llvm_ar,
            toolchain.wasm_ld,
        ]


_MAIN_ARCHIVE_REGEX = re.compile(
    r"^(?P<stem>clang\+llvm-(?P<version>\d+\.\d+\.\d+)-(?P<target>.+?)(-ubuntu-(\d+\.\d+))?)\.tar\.xz$"
)

_ARCH_MAP = {
    Aarch64: "aarch64",
    X86_64: "x86_64",
}

_OS_MAP = {
    Linux: "linux-gnu",
}
