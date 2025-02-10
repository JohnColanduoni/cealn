from cealn.rule import Rule
from cealn.attribute import Attribute, GlobalProviderAttribute
from cealn.exec import Executable
from cealn.depmap import DepmapBuilder
from cealn.platform import Os, Arch, Linux, X86_64, Aarch64

from ..providers import NodeToolchain, YarnToolchain, DenoToolchain


class DownloadNodeToolchain(Rule):
    version = Attribute[str]()

    async def analyze(self):
        # FIXME: detect platform
        try:
            platform_os = _NODE_OS_MAP[self.build_config[Os]]
        except KeyError as ex:
            raise RuntimeError("unsupported OS") from ex
        try:
            platform_arch = _NODE_ARCH_MAP[self.build_config[Arch]]
        except KeyError as ex:
            raise RuntimeError("unsupported architecture") from ex
        platform = f"{platform_os}-{platform_arch}"

        filename = "node.tar.xz"
        archive_dl = self.download(
            f"https://nodejs.org/dist/v{self.version}/node-v{self.version}-{platform}.tar.xz",
            filename=filename,
        )
        archive_extracted = self.extract(archive_dl.files / filename, strip_prefix=f"node-v{self.version}-{platform}")

        return [
            NodeToolchain(
                node=Executable(
                    executable_path="%[execdir]/bin/node",
                    context=archive_extracted.files,
                    search_paths=["bin"],
                ),
                npm=Executable(
                    executable_path="%[execdir]/bin/npm",
                    context=archive_extracted.files,
                    search_paths=["bin"],
                ),
            )
        ]


_NODE_OS_MAP = {Linux: "linux"}

_NODE_ARCH_MAP = {
    X86_64: "x64",
    Aarch64: "arm64",
}


class DownloadYarnToolchain(Rule):
    version = Attribute[str]()

    node_toolchain = GlobalProviderAttribute(NodeToolchain, host=True)

    async def analyze(self):
        dl = self.download(
            f"https://repo.yarnpkg.com/{self.version}/packages/yarnpkg-cli/bin/yarn.js", filename="yarn.js"
        )

        yarn_context = self.new_depmap()
        yarn_context["bin/yarn.js"] = dl.files / "yarn.js"
        yarn_context.merge(self.node_toolchain.node.context)
        yarn_context["bin/yarn"] = DepmapBuilder.file(
            _YARN_SCRIPT,
            executable=True,
        )
        yarn_context = yarn_context.build()

        toolchain = YarnToolchain(
            yarn=Executable(
                name="yarn", executable_path="%[execdir]/bin/yarn", context=yarn_context, search_paths=["bin"]
            )
        )

        return [
            toolchain,
            toolchain.yarn,
        ]


# FIXME: don't include execroot literal here
_YARN_SCRIPT = """#! /bin/bash
exec node /exec/bin/yarn.js "$@"
"""


class DownloadDenoToolchain(Rule):
    version = Attribute[str]()

    async def analyze(self):
        # FIXME: detect platform
        try:
            platform_os = _DENO_OS_MAP[self.build_config[Os]]
        except KeyError as ex:
            raise RuntimeError("unsupported OS") from ex
        try:
            platform_arch = _DENO_ARCH_MAP[self.build_config[Arch]]
        except KeyError as ex:
            raise RuntimeError("unsupported architecture") from ex

        filename = "deno.zip"
        archive_dl = self.download(
            f"https://github.com/denoland/deno/releases/download/v{self.version}/deno-{platform_arch}-{platform_os}.zip",
            filename=filename,
        )
        archive_extracted = self.extract(archive_dl.files / filename)

        context = self.new_depmap("context")
        context["bin/deno"] = archive_extracted.files / "deno"
        context = context.build()

        deno = Executable(
            name="deno",
            executable_path="%[execdir]/bin/deno",
            context=context,
            search_paths=["bin"],
        )

        return [
            DenoToolchain(
                deno=deno,
            ),
            deno,
        ]


_DENO_OS_MAP = {Linux: "unknown-linux-gnu"}

_DENO_ARCH_MAP = {
    X86_64: "x86_64",
    Aarch64: "aarch64",
}
