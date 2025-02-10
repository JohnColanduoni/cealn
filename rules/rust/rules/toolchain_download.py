import sys
from pathlib import Path
from cealn.config import select
from cealn.depmap import DepmapBuilder
from cealn.platform import Os

from cealn.rule import Rule
from cealn.attribute import Attribute, GlobalProviderAttribute
from cealn.exec import Executable
from workspaces.com_cealn_cc.config import llvm_target_triple
from workspaces.com_cealn_cc.providers import LLVMToolchain

from ..providers.rust_toolchain import RustToolchain, Rustc, RustStd, Cargo

# FIXME: pull this in more gracefully, maybe don't vendor
sys.path.insert(0, str(Path(__file__).parents[1] / "vendor" / "toml"))
import toml


class DownloadRustToolchain(Rule):
    channel = Attribute[str](default="stable")
    date = Attribute[str](default=None)

    llvm_toolchain = GlobalProviderAttribute(LLVMToolchain, host=True)

    async def analyze(self):
        from .workspace import CargoWorkspace

        target = llvm_target_triple(self.build_config)

        manifest_url = f"https://static.rust-lang.org/dist/{self.date}/channel-rust-{self.channel}.toml"
        manifest_file_dl = await self.download(manifest_url, filename="channel-rust")

        with await manifest_file_dl.files.open_file("channel-rust", encoding="utf-8") as manifest_file:
            manifest = toml.load(manifest_file)

        pkgs = manifest["pkg"]

        rustc_context = None
        cargo_context = None
        rustfmt_context = None
        rust_std = None
        rust_analyzer_context = None
        rust_src = None

        for component, component_data in pkgs.items():
            for component_target, target_data in component_data["target"].items():
                if not target_data["available"]:
                    continue
                if "xz_url" in target_data and "xz_hash" in target_data:
                    archive_url = target_data["xz_url"]
                    archive_hash = target_data["xz_hash"]
                    filename = f"{component}-{target}.tar.xz"
                else:
                    archive_url = target_data["url"]
                    archive_hash = target_data["hash"]

                filename = f"{component}-{target}.tar.gz"

                if component_target == target and component == "rustc":
                    archive_dl = self.download(
                        archive_url, hash="sha256:" + archive_hash, filename=filename, id="rustc-dl"
                    )
                    archive_extracted = self.extract(
                        archive_dl.files / filename,
                        strip_prefix=f"rustc-{self.channel}-{target}/rustc",
                        id="rustc-extract",
                    )
                    rustc_context = archive_extracted.files
                elif component_target == target and component == "cargo":
                    archive_dl = self.download(
                        archive_url, hash="sha256:" + archive_hash, filename=filename, id="cargo-dl"
                    )
                    archive_extracted = self.extract(
                        archive_dl.files / filename,
                        strip_prefix=f"cargo-{self.channel}-{target}/cargo",
                        id="cargo-extract",
                    )
                    cargo_context = archive_extracted.files
                elif component_target == target and component == "rustfmt-preview":
                    archive_dl = self.download(
                        archive_url, hash="sha256:" + archive_hash, filename=filename, id="rustfmt-dl"
                    )
                    archive_extracted = self.extract(
                        archive_dl.files / filename,
                        strip_prefix=f"rustfmt-{self.channel}-{target}/rustfmt-preview",
                        id="rustfmt-extract",
                    )
                    rustfmt_context = archive_extracted.files
                elif component_target == target and component == "rust-analyzer-preview":
                    archive_dl = self.download(
                        archive_url, hash="sha256:" + archive_hash, filename=filename, id="rust-analyzer-dl"
                    )
                    archive_extracted = self.extract(
                        archive_dl.files / filename,
                        strip_prefix=f"rust-analyzer-{self.channel}-{target}/rust-analyzer-preview",
                        id="rust-analyzer-extract",
                    )
                    rust_analyzer_context = archive_extracted.files
                elif component_target == target and component == "rust-std":
                    archive_dl = self.download(
                        archive_url, hash="sha256:" + archive_hash, filename=filename, id="rust-std-dl"
                    )
                    archive_extracted = self.extract(
                        archive_dl.files / filename,
                        strip_prefix=f"rust-std-{self.channel}-{target}/rust-std-{target}",
                        id="rust-std-extract",
                    )
                    archive_extrated_out = await archive_extracted
                    stdlib_filenames = []
                    testlib_filenames = []
                    proc_macro_path = None
                    with await archive_extrated_out.files.open_file("manifest.in", encoding="utf-8") as f:
                        for line in f:
                            line = line.strip()
                            if not line.startswith("file:"):
                                continue
                            stdlib_subpath = line.removeprefix("file:")
                            if not stdlib_subpath.endswith(".rlib"):
                                continue
                            if "proc_macro" in stdlib_subpath:
                                proc_macro_path = f"%[srcdir]/.rust/{stdlib_subpath}"
                            if any(_libname_to_filename_prefix(libname) in stdlib_subpath for libname in _STDLIB_NAMES):
                                stdlib_filenames.append(f"%[srcdir]/.rust/{stdlib_subpath}")
                            elif any(_libname_to_filename_prefix(libname) in stdlib_subpath for libname in _TEST_NAMES):
                                testlib_filenames.append(f"%[srcdir]/.rust/{stdlib_subpath}")
                    rust_std = RustStd(
                        target=component_target,
                        files=archive_extracted.files,
                        stdlib_filenames=stdlib_filenames,
                        testlib_filenames=testlib_filenames,
                        proc_macro_path=proc_macro_path,
                    )
                elif component == "rust-src":
                    archive_dl = self.download(
                        archive_url, hash="sha256:" + archive_hash, filename=filename, id="rust-src-dl"
                    )
                    archive_extracted = self.extract(
                        archive_dl.files / filename,
                        strip_prefix=f"rust-src-{self.channel}/rust-src",
                        id="rust-src-extract",
                    )
                    rust_src = archive_extracted.files

        rust_std_input = self.new_depmap("rust-std-input")
        rust_std_input.merge(rust_src / "lib/rustlib/src/rust")
        rust_std_input["Cargo.toml"] = DepmapBuilder.file(_RUST_STD_WORKSPACE)
        rust_std_input["library/backtrace/crates/dylib-dep/Cargo.toml"] = DepmapBuilder.file(_RUST_STD_DYLIB_DEP_FAKE)
        rust_std_input["library/backtrace/crates/dylib-dep/src/lib.rs"] = DepmapBuilder.file("")
        rust_std_input = rust_std_input.build()

        if rust_std is None:
            # Prevent circular dependencies on the build stdlib
            no_std_targets = [target]
        else:
            no_std_targets = []

        rust_std_workspace = self.synthetic_target(
            CargoWorkspace,
            name="rust-std",
            workspace_toml=rust_std_input / "Cargo.toml",
            source_root=rust_std_input,
            extra_inputs={
                "../../library": rust_std_input / "library",
            },
            no_std_targets=no_std_targets,
            # Used to prevent clashes between libstd dependencies and crate dependencies
            metadata_hash_extra={"is_for_std": True},
            # FIXME: get lockfile working
            no_lock=True,
        )

        rust_std_output = self.new_depmap("rust-std-output")
        rust_std_output[f"lib/rustlib/{target}/lib"] = (rust_std_workspace / "std").join_action("link")
        rust_std_output[f"lib/rustlib/{target}/lib"] = (rust_std_workspace / "std").join_action(
            "deps-rlib"
        ) / "target/deps"
        rust_std_output = rust_std_output.build()

        if rustc_context is not None and cargo_context is not None and rustfmt_context is not None:
            rustc = Rustc(
                host=target,
                supported_targets=[target],
                executable=Executable(
                    name="rustc",
                    context=rustc_context,
                    executable_path="%[execdir]/bin/rustc",
                    search_paths=["bin"],
                ),
            )

            merged_context = self.new_depmap(id="tool-context")
            merged_context.merge(rustc.executable.context)
            merged_context.merge(cargo_context)
            merged_context.merge(rustfmt_context)
            merged_context.merge(self.llvm_toolchain.clang.context)
            if rust_std is not None:
                merged_context.merge(rust_std.files)
            merged_context = merged_context.build()

            cargo = Cargo(
                host=target,
                executable=Executable(
                    name="cargo", context=merged_context, executable_path="%[execdir]/bin/cargo", search_paths=["bin"]
                ),
            )

            rustfmt = Executable(
                name="rustfmt",
                executable_path="%[execdir]/bin/rustfmt",
                search_paths=["bin"],
                context=merged_context,
            )

            rust_analyzer = None
            if rust_analyzer_context is not None:
                merged_rust_analyzer_context = self.new_depmap(id="rust-analyzer-context")
                merged_rust_analyzer_context.merge(merged_context)
                merged_rust_analyzer_context.merge(rust_src)
                merged_rust_analyzer_context.merge(rust_analyzer_context)
                merged_rust_analyzer_context = merged_rust_analyzer_context.build()

                rust_analyzer = Executable(
                    name="rust-analyzer",
                    executable_path="%[execdir]/bin/rust-analyzer",
                    search_paths=["bin"],
                    context=merged_rust_analyzer_context,
                )

            return [
                RustToolchain(
                    rustc=rustc, rust_std=rust_std, cargo=cargo, rust_analyzer=rust_analyzer, rust_src=rust_src
                ),
                rustc.executable,
                cargo.executable,
                rustfmt,
                *filter(lambda x: x is not None, [rust_analyzer]),
            ]
        elif rust_std is not None:
            # Just rust_std for this target
            return [RustToolchain(rust_std=rust_std, rust_src=rust_src)]
        else:
            # No rust_std, we'll probably have to build it
            return [
                RustToolchain(
                    rust_std=RustStd(
                        target=target,
                        files=rust_std_output,
                        stdlib_filenames=[],
                        testlib_filenames=[],
                        proc_macro_path="",
                    ),
                    rust_src=rust_src,
                )
            ]


def _libname_to_filename_prefix(libname):
    return f"lib{libname.replace('-', '_')}-"


_STDLIB_NAMES = [
    "std",
    "core",
    "alloc",
    "std_detect",
    "rustc-std-workspace-alloc",
    "rustc-std-workspace-core",
    "libc",
    "hashbrown",
    "memchr",
    "compiler_builtins",
    "cfg-if",
    "adler",
    "miniz_oxide",
    "unwind",
    "object",
    "gimli",
    "addr2line",
    "rustc_demangle",
    "dlmalloc",
    # FIXME: configure this
    "panic_unwind",
]

_TEST_NAMES = ["test", "getopts", "unicode_width"]

_RUST_STD_WORKSPACE = """
[workspace]
resolver = "2"
members = [
    "library/alloc",
    "library/core",
    "library/test",
    "library/std",
    "library/proc_macro",
    "library/panic_unwind",
    "library/unwind",
    "library/stdarch/crates/stdarch-test",
    "library/stdarch/crates/stdarch-gen",
    "library/stdarch/crates/assert-instr-macro",
    "library/stdarch/crates/std_detect",
    "library/stdarch/crates/core_arch",
    "library/stdarch/crates/simd-test-macro",
    "library/stdarch/examples",
    "library/rustc-std-workspace-std",
    "library/profiler_builtins",
    "library/panic_abort",
]

[patch.crates-io]
rustc-std-workspace-core = { path = "library/rustc-std-workspace-core" }
rustc-std-workspace-alloc = { path = "library/rustc-std-workspace-alloc" }
rustc-std-workspace-std = { path = "library/rustc-std-workspace-std" }
"""

_RUST_STD_DYLIB_DEP_FAKE = """
[package]
name = "dylib-dep"
version = "0.0.0"
"""
