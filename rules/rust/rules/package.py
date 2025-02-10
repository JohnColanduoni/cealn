import json
import shlex
import sys
import os.path
from pathlib import Path
import re
import hashlib
from cealn.config import CompilationMode, Debug, Fastbuild, Optimized
from cealn.depmap import DepmapBuilder
from cealn.exec import Executable
from cealn.label import Label, LabelPath
from cealn.action import StructuredMessageConfig, TemplateArgument, RespfileArgument
from cealn.platform import Arch, Cuda, Linux, NvPtx, Os, UnknownOs, Wasm32, Windows

from cealn.rule import Rule
from cealn.attribute import Attribute, FileAttribute, GlobalProviderAttribute, ProviderAttribute, LabelMapAttribute
from workspaces.com_cealn_cc.providers import LLVMToolchain
from workspaces.com_cealn_cc.config import llvm_target_triple, CrtLinkage, CrtStatic
from workspaces.com_cealn_cmake.providers import CmakeToolchain

from ..providers.rust_toolchain import RustStd, RustToolchain
from ..providers.workspace import Workspace
from ..util.cfg import CfgSet

# FIXME: pull this in more gracefully, maybe don't vendor
sys.path.insert(0, str(Path(__file__).parents[1] / "vendor" / "toml"))
import toml


class CargoPackage(Rule):
    cargo_toml = FileAttribute(default="Cargo.toml")
    # TODO: autodetect instead of assuming location
    workspace = ProviderAttribute(Workspace, default="//:cargo_workspace")

    package_id = Attribute(default=None)

    rust_toolchain = GlobalProviderAttribute(RustToolchain, host=True)
    target_toolchain = GlobalProviderAttribute(RustToolchain, host=False)
    cc_toolchain = GlobalProviderAttribute(LLVMToolchain, host=True)

    extra_build_tools = Attribute(default=[])
    extra_inputs = LabelMapAttribute(default={})
    build_script_input_filter = Attribute(default=None)
    build_script_extra_inputs = LabelMapAttribute(default={})
    extra_rustflags = Attribute(default=[])

    executable_dependencies = LabelMapAttribute(default={})

    no_std_targets = Attribute(default=[])

    metadata_hash_extra = Attribute(default={})

    async def analyze(self):
        self.host_triple = llvm_target_triple(self.host_build_config)
        self.target_triple = llvm_target_triple(self.build_config)

        package_toml_contents = await self.build_file_contents(self.cargo_toml, encoding="utf-8")
        package_toml = toml.loads(package_toml_contents)

        package_name = package_toml["package"]["name"]
        package_version = package_toml["package"]["version"]
        if self.package_id is not None:
            package_id = self.package_id
        else:
            package_root_path = LabelPath(
                await self.substitute_for_execution(f"%[srcdir]/{self.cargo_toml.relative_to(self.workspace.root)}")
            ).parent
            package_id = f"{package_name} {package_version} (path+file://{package_root_path})"

        cargo_metadata = await self.workspace.metadata_for_package(package_id, rule=self)

        try:
            package = cargo_metadata["packages"][package_id]
        except StopIteration as ex:
            raise RuntimeError(f"package not found in workspace (expected package ID {package_id!r})") from ex

        package_manifest_path = LabelPath(package["manifest_path"])

        self.target_rustflags = self.calculate_rust_flags(self.build_config)
        self.target_linker_flags = self.cc_toolchain.linker_flags(self.build_config)

        self.target_cfgset = await CfgSet.enumerate(
            self, self.rust_toolchain, ["--target", self.target_triple, *self.target_rustflags]
        )

        if self.build_config[CompilationMode] == Optimized:
            self.profile = "release"
        else:
            self.profile = "debug"

        if self.target_triple not in self.no_std_targets:
            rust_std = self.target_toolchain.rust_std
        else:
            rust_std = None

        self.target_stdlib_link_args = []

        if self.build_config[Os] == Linux:
            self.target_stdlib_link_args += ["-lm"]
        elif self.build_config[Os] == Windows:
            self.target_stdlib_link_args += [
                "/DEFAULTLIB:ntdll.lib",
                "/DEFAULTLIB:advapi32.lib",
                "/DEFAULTLIB:userenv.lib",
                "/DEFAULTLIB:bcrypt.lib",
                "/DEFAULTLIB:ws2_32.lib",
            ]
            crt_linkage = CrtLinkage.get_or_default(self.build_config)
            if crt_linkage == CrtStatic:
                self.target_stdlib_link_args += ["/DEFAULTLIB:libcmt.lib"]
            else:
                self.target_stdlib_link_args += ["/DEFAULTLIB:msvcrt.lib"]

        self.target_linker = self.cc_toolchain.linker(self.build_config)
        if self.build_config[Os] == Cuda:
            self.target_linker = await self.resolve_executable(
                "@io.hardscience//tools/cuda/linker:crate", "hs-cuda-linker"
            )

        metadata_extern_args = []
        rlib_extern_args = []
        build_script_extern_args = []

        package_env_vars = {
            # FIXME: do these
            "CARGO_MANIFEST_DIR": f"%[srcdir]/{package_manifest_path.parent}",
            "CARGO_PKG_VERSION": package["version"],
            "CARGO_PKG_VERSION_MAJOR": "",
            "CARGO_PKG_VERSION_MINOR": "",
            "CARGO_PKG_VERSION_PATCH": "",
            "CARGO_PKG_VERSION_PRE": "",
            "CARGO_PKG_NAME": package["name"],
            "CARGO_PKG_DESCRIPTION": "",
            "CARGO_PKG_HOMEPAGE": "",
            "CARGO_PKG_REPOSITORY": "",
            "CARGO_PKG_LICENSE": "",
            "CARGO_PKG_LICENSE_FILE": "",
            "CARGO_PKG_RUST_VERSION": "",
            "CARGO_PKG_AUTHORS": "",
        }

        extra_inputs = self.new_depmap("extra-inputs")
        for k, v in self.extra_inputs.items():
            if isinstance(v, list):
                for item in v:
                    extra_inputs[package_manifest_path.parent / k] = item
            else:
                extra_inputs[package_manifest_path.parent / k] = v
        extra_inputs = extra_inputs.build()

        build_script_extra_inputs = self.new_depmap("build-script-extra-inputs")
        for k, v in self.build_script_extra_inputs.items():
            if isinstance(v, list):
                for item in v:
                    build_script_extra_inputs[package_manifest_path.parent / k] = item
            else:
                build_script_extra_inputs[package_manifest_path.parent / k] = v
        build_script_extra_inputs = build_script_extra_inputs.build()

        crate_dependencies_depmap = self.new_depmap("deps-meta")
        crate_link_dependencies_depmap = self.new_depmap("deps-link")
        crate_rlib_dependencies_depmap = self.new_depmap("deps-rlib")
        crate_fake_rlib_dependencies_depmap = self.new_depmap("deps-fake-rlib")
        crate_build_dependencies_depmap = self.new_depmap("deps-build-meta")
        crate_build_link_dependencies_depmap = self.new_depmap("deps-build-link")
        self_resolve_node = cargo_metadata["resolve"]["nodes"][package_id]
        build_needs_cc = False
        build_needs_cmake = False
        for dependency in self_resolve_node["deps"]:
            has_normal_dependency = False
            has_build_dependency = False
            has_dev_dependency = False
            for dependency_kind in dependency["dep_kinds"]:
                if dependency_kind["target"] is not None:
                    if dependency_kind["target"].startswith("cfg("):
                        if not self.target_cfgset.evaluate(dependency_kind["target"]):
                            continue
                    else:
                        if dependency_kind["target"] != self.target_triple:
                            continue
                if dependency_kind["kind"] is None:
                    has_normal_dependency = True
                elif dependency_kind["kind"] == "build":
                    has_build_dependency = True
                elif dependency_kind["kind"] == "dev":
                    has_dev_dependency = True
                else:
                    raise RuntimeError(dependency_kind.kind)
            if has_normal_dependency:
                dependency_package = cargo_metadata["packages"][dependency["pkg"]]
                dep_target = Label(dependency_package["cealn_target"])
                for dependency_target in dependency_package["targets"]:
                    libname = _crate_name(dependency_target)
                    if "lib" in dependency_target["kind"]:
                        dependency_metadata_hash = _metadata_hash(
                            dependency_package, self.target_triple, self.metadata_hash_extra
                        )
                        imported_libname = _crate_name_rename(dependency["name"]) or _crate_name(dependency_target)
                        libname = _crate_name(dependency_target)
                        metadata_extern_args += [
                            "--extern",
                            f"{imported_libname}=%[srcdir]/target/deps/lib{libname}-{dependency_metadata_hash}.rmeta",
                        ]
                        rlib_extern_args += [
                            "--extern",
                            f"{imported_libname}=%[srcdir]/target/deps/lib{libname}-{dependency_metadata_hash}.rlib",
                        ]
                        crate_dependencies_depmap.merge(DepmapBuilder.glob(dep_target.join_action("rustc"), "*.rmeta"))
                        crate_link_dependencies_depmap.merge(DepmapBuilder.glob(dep_target.join_action("rustc"), "*.o"))
                        crate_fake_rlib_dependencies_depmap.merge(
                            DepmapBuilder.glob(dep_target.join_action("fake-rlib"), "*.rlib")
                        )
                        crate_rlib_dependencies_depmap["target/deps"] = dep_target.join_action("link")
                        # We need the rmeta files and proc-macro dylibs for any transitive dependencies
                        crate_dependencies_depmap.merge(
                            dep_target.join_action("deps-meta"),
                        )
                        crate_fake_rlib_dependencies_depmap.merge(
                            dep_target.join_action("deps-fake-rlib"),
                        )
                        crate_rlib_dependencies_depmap.merge(
                            dep_target.join_action("deps-rlib"),
                        )
                        crate_link_dependencies_depmap.merge(
                            dep_target.join_action("deps-link"),
                        )
                    elif "proc-macro" in dependency_target["kind"]:
                        dependency_metadata_hash = _metadata_hash(
                            dependency_package, self.host_triple, self.metadata_hash_extra
                        )
                        # FIXME: cross platform file dylib extension
                        libname = _crate_name(dependency_target)
                        metadata_extern_args += [
                            "--extern",
                            f"{libname}=%[srcdir]/target/deps/lib{libname}-{dependency_metadata_hash}.so",
                        ]
                        rlib_extern_args += [
                            "--extern",
                            f"{libname}=%[srcdir]/target/deps/lib{libname}-{dependency_metadata_hash}.so",
                        ]
                        link_dep = self.transition(dep_target.join_action("link"), host=True).files
                        crate_dependencies_depmap.merge(
                            DepmapBuilder.glob(
                                link_dep,
                                "*.so",
                            )
                        )
                        crate_fake_rlib_dependencies_depmap.merge(
                            DepmapBuilder.glob(
                                link_dep,
                                "*.so",
                            )
                        )
            if has_build_dependency:
                dependency_package = cargo_metadata["packages"][dependency["pkg"]]
                if dependency_package["name"] == "cc":
                    build_needs_cc = True
                elif dependency_package["name"] == "cmake":
                    build_needs_cmake = True
                dep_target = Label(dependency_package["cealn_target"])
                for dependency_target in dependency_package["targets"]:
                    libname = _crate_name(dependency_target)
                    if "lib" in dependency_target["kind"]:
                        dependency_metadata_hash = _metadata_hash(
                            dependency_package, self.target_triple, self.metadata_hash_extra
                        )
                        build_script_extern_args += [
                            "--extern",
                            f"{libname}=%[srcdir]/target/deps/lib{libname}-{dependency_metadata_hash}.rlib",
                        ]
                        crate_build_dependencies_depmap.merge(
                            DepmapBuilder.glob(
                                self.transition(dep_target.join_action("fake-rlib"), host=True).files,
                                "*.rlib",
                            )
                        )
                        crate_build_link_dependencies_depmap.merge(
                            DepmapBuilder.glob(
                                self.transition(dep_target.join_action("rustc"), host=True).files,
                                "*.o",
                            )
                        )
                        # We need the rmeta files and proc-macro dylibs for any transitive dependencies
                        crate_build_dependencies_depmap.merge(
                            self.transition(dep_target.join_action("deps-fake-rlib"), host=True).files
                        )
                        crate_build_link_dependencies_depmap.merge(
                            self.transition(dep_target.join_action("deps-link"), host=True).files
                        )
                    elif "proc-macro" in dependency_target["kind"]:
                        dependency_metadata_hash = _metadata_hash(
                            dependency_package, self.target_triple, self.metadata_hash_extra
                        )
                        # FIXME: cross platform file dylib extension
                        libname = _crate_name(dependency_target)
                        build_script_extern_args += [
                            "--extern",
                            f"{libname}=%[srcdir]/target/deps/lib{libname}-{dependency_metadata_hash}.so",
                        ]
                        link_dep = self.transition(dep_target.join_action("link"), host=True).files
                        crate_build_dependencies_depmap.merge(
                            DepmapBuilder.glob(
                                link_dep,
                                "*.so",
                            ),
                        )

        crate_build_dependencies_depmap = crate_build_dependencies_depmap.build()
        crate_build_link_dependencies_depmap = crate_build_link_dependencies_depmap.build()

        feature_args = []
        feature_env = {}
        for feature in self_resolve_node["features"]:
            feature_args += ["--cfg", f"feature={json.dumps(feature)}"]
            feature_env["CARGO_FEATURE_" + feature.upper().replace("-", "_")] = ""

        # Handle build scripts
        build_script_target = next((target for target in package["targets"] if "custom-build" in target["kind"]), None)
        build_script_run = None
        if build_script_target is not None:
            build_script_out_dir = (
                f"target/build/{_metadata_hash(package, self.target_triple, self.metadata_hash_extra)}/out"
            )
            build_script_env_vars = {
                **package_env_vars,
                **self.target_cfgset.as_env_vars(),
                **feature_env,
                "CARGO_CRATE_NAME": build_script_target["name"],
                "RUSTC": self.rust_toolchain.rustc.executable.executable_path,
                "TARGET": self.target_triple,
                "HOST": self.host_triple,
                "PROFILE": self.profile,
                "OPT_LEVEL": next(
                    flag.removeprefix("opt-level=") for flag in self.target_rustflags if "opt-level" in flag
                ),
                "DEBUG": json.dumps(self.build_config[CompilationMode] != Optimized),
                "OUT_DIR": f"%[srcdir]/{build_script_out_dir}",
                # FIXME: much more
                "CARGO_ENCODED_RUSTFLAGS": "\x1f".join(["--sysroot", "%[srcdir]/.rust"]),
            }

            build_script_input = self.new_depmap()
            if rust_std is not None:
                build_script_input[".rust"] = rust_std.files
            build_script_input.merge(self.transition(self.label.join_action("custom-build"), host=True).files)
            if not str(package_manifest_path).startswith(".cargo/"):
                if self.build_script_input_filter is None:
                    build_script_input[package_manifest_path.parent] = (
                        self.workspace.root / package_manifest_path.parent
                    )
                else:
                    build_script_input[package_manifest_path.parent] = DepmapBuilder.glob(
                        self.workspace.root / package_manifest_path.parent, *self.build_script_input_filter
                    )
            else:
                build_script_input[package_manifest_path.parent] = (
                    self.workspace.registry / package_manifest_path.parent
                )
            build_script_input.merge(extra_inputs)
            build_script_input.merge(build_script_extra_inputs)
            rustc_respfile_path = "target/rustc.resp"
            envfile_path = "target/envfile.txt"
            linker_respfile_path = (
                f"target/deps/{_metadata_hash(package, self.target_triple, self.metadata_hash_extra)}.linker.resp"
            )

            build_script_wrapper = Executable(
                executable_path="/exec/cealn-rules-rust-support",
                context=Label("@com.cealn.rust//support:crate:cealn-rules-rust-support"),
            ).add_dependency_executable(self, self.rust_toolchain.rustc.executable)
            for tool_label in self.extra_build_tools:
                tool_executables = [
                    provider
                    for provider in await self.load_providers(tool_label, host=True)
                    if isinstance(provider, Executable)
                ]
                build_script_wrapper = build_script_wrapper.add_dependency_executable(self, *tool_executables)
            if build_needs_cc or build_needs_cmake:
                build_script_env_vars["CC"] = self.cc_toolchain.cc(self.build_config).executable_path
                build_script_env_vars["CXX"] = self.cc_toolchain.cxx(self.build_config).executable_path
                build_script_env_vars["AR"] = self.cc_toolchain.ar(self.build_config).executable_path
                build_script_env_vars["CRATE_CC_NO_DEFAULTS"] = "true"
                # FIXME: should we quote these?
                build_script_env_vars["CFLAGS"] = " ".join(arg for arg in self.cc_toolchain.cflags(self.build_config))
                build_script_env_vars["CXXFLAGS"] = " ".join(
                    arg for arg in self.cc_toolchain.cxxflags(self.build_config)
                )
                build_script_env_vars["LDFLAGS"] = " ".join(
                    arg for arg in self.cc_toolchain.linker_flags(self.build_config)
                )
                # FIXME: hack
                build_script_env_vars["OPENSSL_LIB_DIR"] = "/src/.sysroot/usr/lib/x86_64-linux-gnu"
                build_script_env_vars["OPENSSL_INCLUDE_DIR"] = "/src/.sysroot/usr/include"
                build_script_env_vars.update(self.cc_toolchain.cc_env(self.build_config))
                build_script_wrapper = build_script_wrapper.add_dependency_executable(self, self.cc_toolchain.clang)
                self.cc_toolchain.add_extra_inputs(self.build_config, build_script_input)
            if build_needs_cmake:
                build_script_env_vars["CMAKE_GENERATOR"] = "Ninja"
                cmake = await self.resolve_global_provider(CmakeToolchain, host=True)
                build_script_wrapper = build_script_wrapper.add_dependency_executable(self, cmake.cmake)

            build_script_input = build_script_input.build()

            build_script_run = self.run(
                build_script_wrapper,
                "build-script",
                f"%[srcdir]/target/deps/{package['name']}/{build_script_target['name']}",
                f"%[srcdir]/{rustc_respfile_path}",
                f"%[srcdir]/{envfile_path}",
                f"%[srcdir]/{linker_respfile_path}",
                input=build_script_input,
                append_env=build_script_env_vars,
                cwd=f"%[srcdir]/{package_manifest_path.parent}",
                id="build-script-run",
                mnemonic="CargoScript",
                progress_message=package["name"],
            )

            crate_link_dependencies_depmap[build_script_out_dir] = build_script_run.files / build_script_out_dir
            crate_link_dependencies_depmap[linker_respfile_path] = build_script_run.files / linker_respfile_path

        crate_dependencies_depmap = crate_dependencies_depmap.build()
        crate_link_dependencies_depmap = crate_link_dependencies_depmap.build()
        crate_fake_rlib_dependencies_depmap = crate_fake_rlib_dependencies_depmap.build()
        crate_rlib_dependencies_depmap = crate_rlib_dependencies_depmap.build()

        output_providers = []

        check_all_targets = self.new_depmap("check-all")

        targets = [(target, None) for target in package["targets"]]
        for target in package["targets"]:
            if target["test"] and rust_std is not None:
                targets.append((target, "test"))

        for target, target_aux_type in targets:
            emit = ["metadata"]
            extra_args = []
            extra_env = {}
            extra_kwargs = {}
            if target_aux_type:
                qualified_target_name = f"{target['name']}-{target_aux_type}"
            else:
                qualified_target_name = target["name"]

            if "custom-build" in target["kind"]:
                target_dependencies_depmap = crate_build_dependencies_depmap
                target_fake_rlib_dependencies_depmap = crate_build_dependencies_depmap
                target_link_dependencies_depmap = crate_build_link_dependencies_depmap
            else:
                target_dependencies_depmap = crate_dependencies_depmap
                target_fake_rlib_dependencies_depmap = crate_fake_rlib_dependencies_depmap
                target_link_dependencies_depmap = crate_link_dependencies_depmap

            rustc_input = self.new_depmap()
            if not str(package_manifest_path).startswith(".cargo/"):
                rustc_input[package_manifest_path.parent] = self.workspace.root / package_manifest_path.parent
            else:
                rustc_input[package_manifest_path.parent] = self.workspace.registry / package_manifest_path.parent
            rustc_input.merge(extra_inputs)
            if rust_std is not None:
                rustc_input[".rust"] = rust_std.files
            rustc_input.merge(target_dependencies_depmap)
            if (
                "bin" in target["kind"]
                or "proc-macro" in target["kind"]
                or "dylib" in target["kind"]
                or "cdylib" in target["kind"]
                or "custom-build" in target["kind"]
                or target_aux_type == "test"
            ):
                rustc_input.merge(target_fake_rlib_dependencies_depmap)
            if "bin" in target["kind"] or "test" in target["kind"] or "example" in target["kind"]:
                lib_target = next((target for target in package["targets"] if "lib" in target["kind"]), None)
                if lib_target is not None:
                    extra_args += [
                        "--extern",
                        f"{_crate_name(lib_target)}=%[srcdir]/target/deps/lib{_crate_name(lib_target)}-{metadata_hash}.rlib",
                    ]
                    rustc_input.merge(self.label.join_action("rustc"))
                    rustc_input.merge(self.label.join_action("fake-rlib"))
            if build_script_run is not None and not "custom-build" in target["kind"]:
                rustc_input.merge(build_script_run.files)
                extra_args += [f"@%[srcdir]/{rustc_respfile_path}"]
                extra_kwargs.setdefault("append_env_files", []).append(build_script_run.files / envfile_path)
                extra_env["OUT_DIR"] = f"%[srcdir]/{build_script_out_dir}"
            rustc_input = rustc_input.build()

            if (
                "lib" in target["kind"]
                or "rlib" in target["kind"]
                or "proc-macro" in target["kind"]
                or "dylib" in target["kind"]
                or "cdylib" in target["kind"]
            ):
                if target_aux_type == "test":
                    rustc_action_id = "test-rustc"
                    link_action_id = "test-link"
                    check_action_id = "test-check"
                else:
                    rustc_action_id = "rustc"
                    link_action_id = "link"
                    check_action_id = "check"
            elif "custom-build" in target["kind"]:
                assert not target_aux_type
                rustc_action_id = "custom-build-rustc"
                link_action_id = "custom-build"
                check_action_id = "custom-build-check"
            else:
                if target_aux_type == "test":
                    rustc_action_id = f"test-{target['name']}-rustc"
                    link_action_id = f"test-{target['name']}-link"
                    check_action_id = f"test-{target['name']}-check"
                else:
                    rustc_action_id = f"{target['name']}-rustc"
                    link_action_id = target["name"]
                    check_action_id = f"{target['name']}-check"

            if (
                "bin" in target["kind"]
                or "proc-macro" in target["kind"]
                or "dylib" in target["kind"]
                or "cdylib" in target["kind"]
                or target_aux_type == "test"
            ):
                extern_args = rlib_extern_args
                emit += ["link"]
                extra_args += ["-Zno-link"]
                if "proc-macro" in target["kind"]:
                    extra_args += ["--extern", f"proc_macro={rust_std.proc_macro_path}"]
            elif ("lib" in target["kind"] or "rlib" in target["kind"]) and target_aux_type is None:
                extern_args = metadata_extern_args
                emit += ["link"]
                extra_args += ["-Zno-link"]
            elif "custom-build" in target["kind"]:
                extern_args = build_script_extern_args
                emit += ["link"]
                extra_args += ["-Zno-link"]
            elif "test" in target["kind"]:
                # TODO
                continue
            elif "example" in target["kind"]:
                # TODO
                continue
            elif "bench" in target["kind"]:
                # TODO
                continue
            else:
                raise RuntimeError(f"unsupported target kind {target['kind']}")

            out_dir = "%[srcdir]/target/deps"

            metadata_hash = _metadata_hash(package, self.target_triple, self.metadata_hash_extra)

            if package["name"] == target["name"]:
                progress_message = package["name"]
            else:
                progress_message = f"{package['name']} {target['name']}"

            has_extra_filename = False
            if "lib" in target["kind"] or "proc-macro" in target["kind"]:
                extra_args += [
                    "-C",
                    f"extra-filename=-{metadata_hash}",
                ]
                has_extra_filename = True
            if target_aux_type == "test":
                extra_args += ["--test"]

            rustc = self.run(
                self.rust_toolchain.rustc.executable,
                "--error-format=json",
                "--json=artifacts,diagnostic-rendered-ansi",
                "-C",
                f"metadata={metadata_hash}",
                "--sysroot",
                "%[srcdir]/.rust",
                "--out-dir",
                out_dir,
                "-L",
                "dependency=%[srcdir]/target/deps",
                "--crate-name",
                _crate_name(target),
                "--crate-type",
                ",".join(target["crate_types"]),
                "--edition",
                target["edition"],
                "--target",
                self.target_triple,
                f"--emit={','.join(emit)}",
                *self.target_rustflags,
                *extern_args,
                *feature_args,
                *extra_args,
                *self.extra_rustflags,
                *self.workspace.global_rustflags,
                target["src_path"],
                append_env={
                    **package_env_vars,
                    **extra_env,
                    "CARGO_CRATE_NAME": target["name"],
                },
                **extra_kwargs,
                input=rustc_input,
                id=rustc_action_id,
                mnemonic="Rustc",
                progress_message=progress_message,
                structured_messages=_RUST_STRUCTURED_MESSAGE_CONFIG,
            )

            check = self.run(
                self.rust_toolchain.rustc.executable,
                "--error-format=json",
                "--json=artifacts,diagnostic-rendered-ansi",
                "-C",
                f"metadata={metadata_hash}",
                "--sysroot",
                "%[srcdir]/.rust",
                "--out-dir",
                out_dir,
                "-L",
                "dependency=%[srcdir]/target/deps",
                "--crate-name",
                _crate_name(target),
                "--crate-type",
                ",".join(target["crate_types"]),
                "--edition",
                target["edition"],
                "--target",
                self.target_triple,
                "--emit=metadata",
                *self.target_rustflags,
                *extern_args,
                *feature_args,
                *extra_args,
                *self.workspace.global_rustflags,
                target["src_path"],
                append_env={
                    **package_env_vars,
                    **extra_env,
                    "CARGO_CRATE_NAME": target["name"],
                },
                **extra_kwargs,
                input=rustc_input,
                id=check_action_id,
                mnemonic="RustcCheck",
                progress_message=progress_message,
                structured_messages=_RUST_STRUCTURED_MESSAGE_CONFIG,
            )
            check_all_targets.merge(check.files)

            if (
                "bin" in target["kind"]
                or "proc-macro" in target["kind"]
                or "cdylib" in target["kind"]
                or "custom-build" in target["kind"]
                or "test" in target["kind"]
                or target_aux_type == "test"
            ):
                # FIXME: dylibs too
                direct_object_files = self.new_depmap()
                direct_object_files.merge(DepmapBuilder.glob(rustc.files, "*.o"))
                if "proc-macro" in target["kind"]:
                    direct_object_files.merge(DepmapBuilder.glob(rustc.files, "*.rcgu.rmeta"))
                direct_object_files = direct_object_files.build()

                link_input = self.new_depmap()
                transitive_object_files = self.new_depmap()
                transitive_linker_respfiles = self.new_depmap()

                transitive_object_files.merge(DepmapBuilder.glob(target_link_dependencies_depmap, "*.o"))
                transitive_linker_respfiles.merge(DepmapBuilder.glob(target_link_dependencies_depmap, "*.linker.resp"))

                direct_linker_respfile_path = "target/deps/self.linker.resp"
                if target["name"] != "cealn-rules-rust-support":
                    if has_extra_filename:
                        rlink_filename = f"%[srcdir]/target/deps/{_crate_name(target)}-{metadata_hash}.rlink"
                    else:
                        rlink_filename = f"%[srcdir]/target/deps/{_crate_name(target)}.rlink"
                    rlink_parse = self.run(
                        Executable(
                            executable_path="/exec/cealn-rules-rust-support",
                            context=Label("@com.cealn.rust//support:crate:cealn-rules-rust-support"),
                        ),
                        "rlink-parse",
                        rlink_filename,
                        f"%[srcdir]/{direct_linker_respfile_path}",
                        input=rustc.files,
                        id=f"rlink-parse-{qualified_target_name}",
                        mnemonic="RustcRlink",
                        progress_message=progress_message,
                    )
                    rlink_parse_files = rlink_parse.files
                else:
                    # We can't use the rlink parser when building the support executable itself
                    rlink_parse_files = self.new_depmap()
                    stdlib_filenames = await self.gather(
                        list(self.substitute_for_execution(filename) for filename in rust_std.stdlib_filenames)
                    )
                    rlink_parse_files[direct_linker_respfile_path] = DepmapBuilder.file("\n".join(stdlib_filenames))
                    rlink_parse_files = rlink_parse_files.build()

                if rust_std is not None:
                    link_input[".rust"] = rust_std.files
                self.cc_toolchain.add_extra_inputs(self.build_config, link_input)
                link_input.merge(extra_inputs)
                link_input.merge(direct_object_files)
                link_input.merge(target_link_dependencies_depmap)
                link_input.merge(rlink_parse_files)
                if "bin" in target["kind"] or "test" in target["kind"] or "example" in target["kind"]:
                    lib_target = next((target for target in package["targets"] if "lib" in target["kind"]), None)
                    if lib_target is not None:
                        link_input.merge(self.label.join_action("rustc"))
                        transitive_object_files.merge(DepmapBuilder.glob(self.label.join_action("rustc"), "*.o"))
                if build_script_run is not None and "custom-build" not in target["kind"]:
                    link_input.merge(build_script_run.files)
                if "custom-build" in target["kind"]:
                    link_input[f"target/deps/{package['name']}"] = DepmapBuilder.directory()

                link_input = link_input.build()
                transitive_object_files = transitive_object_files.build()
                transitive_linker_respfiles = transitive_linker_respfiles.build()

                link_extra_args = [
                    *self.target_linker_flags,
                    *self.target_stdlib_link_args,
                ]
                if target_aux_type == "test":
                    output_filename = self.cc_toolchain.exe_name(self.build_config, f"test_{target['name']}")
                elif "bin" in target["kind"]:
                    output_filename = self.cc_toolchain.exe_name(self.build_config, target["name"])
                elif "proc-macro" in target["kind"]:
                    output_filename = "target/deps/" + self.cc_toolchain.dylib_name(
                        self.build_config, f"{_crate_name(target)}-{metadata_hash}"
                    )
                    link_extra_args += ["-shared"]
                elif "cdylib" in target["kind"]:
                    output_filename = self.cc_toolchain.dylib_name(
                        self.build_config,
                        _crate_name(target),
                    )
                    if self.build_config[Os] == Linux:
                        link_extra_args += ["-shared"]
                    if self.build_config[Arch] == Wasm32:
                        link_extra_args += ["--no-entry", "--export-dynamic", "--allow-undefined"]
                elif "custom-build" in target["kind"]:
                    output_filename = f"target/deps/{package['name']}/{target['name']}"
                    if rust_std is not None:
                        link_extra_args += [rust_std.proc_macro_path]
                if build_script_run is not None and "custom-build" not in target["kind"]:
                    link_extra_args += [f"@{linker_respfile_path}"]

                link = self.run(
                    self.target_linker,
                    *self.cc_toolchain.linker_output_flags(self.build_config, output_filename),
                    *link_extra_args,
                    direct_object_files,
                    f"@%[srcdir]/{direct_linker_respfile_path}",
                    RespfileArgument("@$1", transitive_object_files),
                    TemplateArgument("@$1", transitive_linker_respfiles),
                    input=link_input,
                    append_env=self.cc_toolchain.linker_env(self.build_config),
                    id=link_action_id,
                    mnemonic="RustLink",
                    progress_message=progress_message,
                )
            elif "lib" in target["kind"] or "rlib" in target["kind"]:
                direct_object_files = self.new_depmap()
                direct_object_files.merge(DepmapBuilder.glob(rustc.files, "*.o"))
                direct_object_files = direct_object_files.build()

                real_rlib_input = self.new_depmap()
                if "rlib" in target["kind"]:
                    real_rlib_input["lib.rmeta"] = rustc.files / f"target/deps/lib{_crate_name(target)}.rmeta"
                else:
                    real_rlib_input["lib.rmeta"] = (
                        rustc.files / f"target/deps/lib{_crate_name(target)}-{metadata_hash}.rmeta"
                    )
                real_rlib_input.merge(direct_object_files)
                real_rlib_input = real_rlib_input.build()
                self.run(
                    self.cc_toolchain.ar(self.build_config, force_unix=True),
                    "rc",
                    f"lib{_crate_name(target)}.rlib",
                    "lib.rmeta",
                    direct_object_files,
                    input=real_rlib_input,
                    id=link_action_id,
                )

                fake_rlib_input = self.new_depmap()
                fake_rlib_input["target/deps"] = DepmapBuilder.directory()
                fake_rlib_input["lib.rmeta"] = (
                    rustc.files / f"target/deps/lib{_crate_name(target)}-{metadata_hash}.rmeta"
                )
                fake_rlib_input = fake_rlib_input.build()
                self.run(
                    self.cc_toolchain.ar(self.build_config, force_unix=True),
                    "rc",
                    f"%[srcdir]/target/deps/lib{_crate_name(target)}-{metadata_hash}.rlib",
                    "lib.rmeta",
                    input=fake_rlib_input,
                    id="fake-rlib",
                    mnemonic="FakeRlib",
                    progress_message=progress_message,
                )

            if "bin" in target["kind"] or target_aux_type == "test":
                exec_context = self.new_depmap()
                exec_context[f"bin/{output_filename}"] = link.files / output_filename
                exec_context["target/build"] = target_link_dependencies_depmap / "target" / "build"
                if exec_dependencies := self.executable_dependencies.get(target["name"]):
                    if not isinstance(exec_dependencies, list):
                        exec_dependencies = [exec_dependencies]
                    for exec_dependency in exec_dependencies:
                        exec_context.merge(exec_dependency)
                exec_context = exec_context.build()

                if "bin" in target["kind"] and target_aux_type is None:
                    output_providers.append(
                        Executable(
                            name=output_filename,
                            executable_path=f"%[execdir]/bin/{output_filename}",
                            context=exec_context,
                            search_paths=["bin"],
                        )
                    )
                if target_aux_type == "test":
                    test_executable = Executable(
                        name=f"test_{target['name']}",
                        executable_path=f"%[execdir]/bin/{output_filename}",
                        context=exec_context,
                        search_paths=["bin"],
                    )
                    if "lib" in target["kind"] or "cdylib" in target["kind"]:
                        test_run_id = "test-lib"
                    else:
                        test_run_id = f"test-{target['name']}"
                    run_test_wrapper = Executable(
                        executable_path="/exec/cealn-rules-rust-support",
                        context=Label("@com.cealn.rust//support:crate:cealn-rules-rust-support"),
                    )
                    self.run(
                        run_test_wrapper,
                        "run-test",
                        f"/src/bin/{output_filename}",
                        input=exec_context,
                        id=test_run_id,
                    )
                    output_providers.append(test_executable)

        check_all_targets = check_all_targets.build()

        return output_providers

    def calculate_rust_flags(self, build_config):
        flags = [
            "-Z",
            "dwarf-version=4",
        ]
        if build_config[CompilationMode] == Optimized:
            if build_config[Arch] == Wasm32:
                flags += ["-C", "opt-level=z"]
            else:
                flags += [
                    "-C",
                    "opt-level=3",
                ]
            flags += [
                "-C",
                "lto=thin",
                "-C",
                "linker-plugin-lto=true",
                "-C",
                # TODO: consider increasing
                "codegen-units=1",
                "-C",
                "debuginfo=1",
                "-C",
                "debug-assertions=false",
                "-C",
                "overflow-checks=false",
            ]
        elif build_config[CompilationMode] == Debug:
            flags += [
                "-C",
                "opt-level=0",
                "-C",
                "codegen-units=16",
                "-C",
                "embed-bitcode=false",
                "-C",
                "debuginfo=2",
                "-C",
                "debug-assertions=true",
                "-C",
                "overflow-checks=true",
            ]
        elif build_config[CompilationMode] == Fastbuild:
            flags += [
                "-C",
                "opt-level=0",
                "-C",
                "codegen-units=16",
                "-C",
                "embed-bitcode=false",
                "-C",
                "debuginfo=1",
                "-C",
                "debug-assertions=true",
                "-C",
                "overflow-checks=true",
            ]
        # FIXME: don't source this here
        if build_config[Arch] == Wasm32:
            flags += [
                # TODO: add +multivalue, but it requires compiling the stdlib with the same flag
                "-C",
                "target-feature=+reference-types,+nontrapping-fptoint,+bulk-memory,+simd128,+sign-ext,+mutable-globals",
            ]
        # FIXME: don't source this here
        if build_config[Arch] == NvPtx:
            flags += ["-C", "target-cpu=sm_89"]
        return flags


def _crate_name(target):
    return target["name"].replace("-", "_")


def _crate_name_rename(rename):
    if rename is None:
        return None
    return rename.replace("-", "_")


def _metadata_hash(package, triple, extra):
    # FIXME: more stuff in key
    return hashlib.sha256(
        json.dumps(
            {
                "id": package["id"],
                "target": triple,
                **extra,
            },
            sort_keys=True,
        ).encode("utf-8")
    ).hexdigest()[:16]


_RUST_STRUCTURED_MESSAGE_CONFIG = StructuredMessageConfig()
_RUST_STRUCTURED_MESSAGE_CONFIG.human_messages.append("$.rendered")
_RUST_STRUCTURED_MESSAGE_CONFIG.level_map["$.[?($.level=='error')]"] = "error"
_RUST_STRUCTURED_MESSAGE_CONFIG.level_map["$.[?($.level=='warning')]"] = "warn"
