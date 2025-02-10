import json
from typing import Dict, List, Optional
from itertools import chain
import re
from cealn.config import CompilationMode, Optimized

from cealn.rule import Rule
from cealn.attribute import (
    Attribute,
    FileAttribute,
    GlobalProviderAttribute,
    ProviderAttribute,
    LabelMapAttribute,
    LabelListAttribute,
)
from cealn.exec import Executable
from cealn.label import Label, LabelPath
from cealn.depmap import DepmapBuilder

from ..providers import YarnToolchain, NodeToolchain, YarnWorkspace


class Esbuild(Rule):
    package_json = FileAttribute(default="package.json")

    entry_points = Attribute[Dict[str, str]]()
    source_patterns = Attribute[List[str]](
        default=[
            "*.ts",
            "*.js",
            "*.css",
            "*.png",
            "*.svg",
            "*.woff2",
            "*.woff",
        ]
    )
    ssr = Attribute[bool](default=False)

    platform = Attribute[str](default="browser")
    target = Attribute[str]()
    format = Attribute[str](default="esm")
    defines = Attribute[Dict[str, str]](default={})
    sourcemap = Attribute[str](default="linked")
    splitting = Attribute[bool](default=False)
    tree_shaking = Attribute[Optional[bool]](default=None)
    minify = Attribute[bool](default=False)
    keep_names = Attribute[bool](default=False)
    entry_names = Attribute[str](default=None)
    chunk_names = Attribute[str](default=None)
    loaders = Attribute[Dict[str, str]](default={})
    externals = Attribute[List[str]](default=[])
    public_path = Attribute[str](default=None)
    aliases = Attribute[Dict[str, str]](default={})
    supported = Attribute[Dict[str, bool]](default={})
    inject = LabelListAttribute(default=[])

    extra_inputs = LabelMapAttribute(default={})

    # TODO: autodetect instead of assuming location
    yarn_workspace = ProviderAttribute(YarnWorkspace, default="//:yarn_workspace", host=True)

    yarn_toolchain = GlobalProviderAttribute(YarnToolchain, host=True)
    node_toolchain = GlobalProviderAttribute(NodeToolchain, host=True)

    async def analyze(self):
        # FIXME: detect
        workspace_root = Label("@io.hardscience//")
        workspace_subpath = LabelPath(str(self.package_json.parent.relative_to(workspace_root)))

        packages_to_scan = [workspace_subpath]
        visited_packages = set()
        self.visited_imports = set()

        self.bundle_input = self.new_depmap()
        self.bundle_input.merge(self.yarn_workspace.files)

        for k, v in self.extra_inputs.items():
            self.bundle_input[k] = v

        while packages_to_scan:
            package_path = packages_to_scan.pop()
            if package_path in visited_packages:
                continue
            visited_packages.add(package_path)

            package_json = json.loads(
                await self.build_file_contents(workspace_root / package_path / "package.json", encoding="utf-8")
            )

            self.bundle_input[package_path] = DepmapBuilder.glob(workspace_root / package_path, *self.source_patterns)

            # FIXME: Hard Science specific
            if str(package_path).startswith("vendor/"):
                vendor_subdir = str(package_path).split("/")[1]
                build_package_input = self.new_depmap()
                build_package_input.merge(self.yarn_workspace.files)
                build_package_input[f"vendor/{vendor_subdir}"] = workspace_root / "vendor" / vendor_subdir
                build_package_input = build_package_input.build()
                build_package = self.run(
                    self.yarn_toolchain.yarn,
                    "build",
                    input=build_package_input,
                    cwd=package_path,
                    mnemonic="YarnBuild",
                    progress_message=str(workspace_root / package_path),
                )
                self.bundle_input[package_path] = build_package.files / package_path

            for dependency_name, dependency_resolution in chain(
                package_json.get("dependencies", {}).items(), package_json.get("devDependencies", {}).items()
            ):
                if m := _WORKSPACE_DEPENDENCY_REGEX.fullmatch(dependency_resolution):
                    if m.group(1) == "*" or m.group(1) == "^":
                        # FIXME: implement
                        continue
                    packages_to_scan.append(LabelPath(m.group(1)))

        for entrypoint_path in self.entry_points.values():
            package_json = json.loads(await self.build_file_contents(self.package_json, encoding="utf-8"))
            await self.prepare_imports(
                workspace_root / workspace_subpath / entrypoint_path,
                workspace_subpath / entrypoint_path,
                package_json,
                workspace_root,
            )

        self.bundle_input = self.bundle_input.build()

        esbuild_args = []

        for entrypoint_name, entrypoint_path in self.entry_points.items():
            esbuild_args.append(f"{entrypoint_name}={workspace_subpath}/{entrypoint_path}")

        esbuild_args += [
            "--bundle",
            f"--outdir={workspace_subpath / 'out'}",
            f"--platform={self.platform}",
            f"--target={self.target}",
            f"--format={self.format}",
            f"--sourcemap={self.sourcemap}",
            f"--splitting={json.dumps(self.splitting)}",
            f"--metafile={workspace_subpath / 'out' / 'meta.json'}",
            "--color=true",
            "--log-level=warning",
        ]

        if self.public_path is not None:
            esbuild_args += [f"--public-path={self.public_path}"]

        if self.entry_names is not None:
            esbuild_args += [f"--entry-names={self.entry_names}"]

        if self.chunk_names is not None:
            esbuild_args += [f"--chunk-names={self.chunk_names}"]

        if self.minify:
            esbuild_args += ["--minify"]

        if self.tree_shaking is not None:
            esbuild_args += [f"--tree-shaking={json.dumps(self.tree_shaking)}"]

        if self.keep_names:
            esbuild_args += ["--keep-names"]

        for k, v in self.defines.items():
            esbuild_args.append(f"--define:{k}={v}")

        for k, v in self.loaders.items():
            esbuild_args.append(f"--loader:{k}={v}")

        for v in self.externals:
            esbuild_args.append(f"--external:{v}")

        for v in self.inject:
            inject_subpath = v.relative_to(workspace_root)
            esbuild_args.append(f"--inject:{inject_subpath}")

        for k, v in self.aliases.items():
            esbuild_args.append(f"--alias:{k}={v}")

        for k, v in self.supported.items():
            esbuild_args.append(f"--supported:{k}={json.dumps(v)}")

        esbuild_detect = await self.run(
            self.yarn_toolchain.yarn,
            "bin",
            "esbuild",
            cwd=f"%[srcdir]/{workspace_subpath}",
            input=self.yarn_workspace.files,
            append_env={"BROWSERSLIST_IGNORE_OLD_DATA": "true"},
            hide_stdout=True,
            mnemonic="EsBuildDetect",
            progress_message=str(self.package_json),
        )
        with await esbuild_detect.open_stdout(encoding="utf-8") as f:
            esbuild_path = f.read().strip()

        # FIXME: detect version from package
        esbuild_run = self.run(
            self.node_toolchain.node,
            "-r",
            "./.pnp.cjs",
            esbuild_path,
            *esbuild_args,
            input=self.bundle_input,
            mnemonic="EsBuild",
            progress_message=" ".join(
                str(workspace_root / workspace_subpath / entrypoint) for entrypoint in self.entry_points.values()
            ),
        )

        bundle = self.new_depmap("bundle")
        bundle.merge(esbuild_run.files / workspace_subpath / "out")
        bundle = bundle.build()

    async def prepare_imports(self, file_label, workspace_subpath, package_json, workspace_root, *, check_exist=True):
        if file_label in self.visited_imports:
            return
        self.visited_imports.add(file_label)

        workspace_subpath = workspace_subpath.normalize_require_descending()

        if str(file_label).endswith(".vue"):
            await self.handle_vue(file_label, workspace_subpath, package_json, workspace_root)
            return
        elif str(file_label).endswith(".svelte"):
            await self.handle_svelte(file_label, workspace_subpath, package_json, workspace_root)
            return
        elif str(file_label).endswith(".capnp"):
            await self.handle_capnp(file_label, workspace_subpath, package_json, workspace_root)
            return
        elif str(file_label).endswith(".proto"):
            await self.handle_proto(file_label, workspace_subpath, package_json, workspace_root)
            return
        elif str(file_label).endswith("/+routes"):
            await self.handle_routes(file_label, workspace_subpath, package_json, workspace_root)
            return

        for k, v in self.extra_inputs.items():
            try:
                extra_input_subpath = workspace_subpath.relative_to(LabelPath(k))
            except ValueError:
                pass
            else:
                if str(extra_input_subpath) == "." or str(extra_input_subpath) == "":
                    file_label = v
                else:
                    file_label = v / extra_input_subpath
                break

        # FIXME: hack
        if str(file_label).endswith("hs_client_web"):
            file_label = Label(f"{file_label}.js")
            check_exist = False
        elif "bindgen" in str(file_label):
            check_exist = False
        elif "bundle" in str(file_label):
            check_exist = False
        elif "dist" in str(file_label):
            return
        elif "__generated__" in str(file_label):
            return
        elif workspace_subpath.name.endswith(".css"):
            check_exist = False

        if check_exist and not (await self.is_file(file_label)):
            found = False
            for extension in [".js", ".ts"]:
                try_label = Label(f"{file_label}{extension}")
                if await self.is_file(try_label):
                    file_label = try_label
                    workspace_subpath = LabelPath(f"{workspace_subpath}{extension}")
                    found = True
                    break
            if not found:
                for index_subpath in ["index.js", "index.ts"]:
                    try_label = Label(f"{file_label}/{index_subpath}")
                    if await self.is_file(try_label):
                        file_label = try_label
                        workspace_subpath = workspace_subpath / index_subpath
                        found = True
                        break
            if not found:
                raise RuntimeError(f"couldn't resolve label {file_label} to a javascript file")

        input = self.new_depmap()
        input[workspace_subpath] = file_label
        input = input.build()

        for suffix in self.loaders:
            if str(file_label).endswith(suffix):
                return

        if str(file_label).endswith(".js") or str(file_label).endswith(".ts"):
            enumerate_imports = await self.run(
                Executable(
                    executable_path="/exec/cealn-rules-js-support",
                    context=Label("@com.cealn.js//support:crate:cealn-rules-js-support"),
                ),
                "enumerate-imports",
                str(workspace_subpath),
                input=input,
                hide_stdout=True,
            )
            with await enumerate_imports.open_stdout(encoding="utf-8") as f:
                prepare_tasks = []
                for line in f:
                    import_spec = line.strip()
                    found_dependency = next(
                        (
                            (k, v)
                            for k, v in package_json.get("dependencies", {}).items()
                            if import_spec_matches_package(import_spec, k)
                        ),
                        None,
                    )
                    if found_dependency:
                        (found_dependency, found_dependency_ref) = found_dependency
                        dependency_subpath = import_spec.removeprefix(found_dependency).removeprefix("/")
                        if m := _WORKSPACE_DEPENDENCY_REGEX.fullmatch(found_dependency_ref):
                            dep_workspace_subpath = LabelPath(m.group(1))
                            dep_package_json = json.loads(
                                await self.build_file_contents(
                                    workspace_root / dep_workspace_subpath / "package.json", encoding="utf-8"
                                )
                            )
                            if not dependency_subpath:
                                # FIXME: more to handle here
                                dependency_subpath = dep_package_json["main"]
                            else:
                                exports = dep_package_json.get("exports", {})
                                export_entry = exports.get(dependency_subpath) or exports.get(f"./{dependency_subpath}")
                                if export_entry:
                                    dependency_subpath = export_entry.get("import") or export_entry["require"]

                            prepare_tasks.append(
                                self.prepare_imports(
                                    workspace_root / dep_workspace_subpath / dependency_subpath,
                                    dep_workspace_subpath / dependency_subpath,
                                    dep_package_json,
                                    workspace_root,
                                )
                            )
                    elif import_spec.startswith("."):
                        prepare_tasks.append(
                            self.prepare_imports(
                                workspace_root / workspace_subpath.parent / import_spec,
                                workspace_subpath.parent / import_spec,
                                package_json,
                                workspace_root,
                            )
                        )

                await self.gather(prepare_tasks)

    async def handle_vue(self, file_label, workspace_subpath, package_json, workspace_root):
        input = self.new_depmap()
        input.merge(self.yarn_workspace.files)
        vue_sfc_mount = str(workspace_subpath.parent / "vue_sfc.js")
        input[vue_sfc_mount] = Label("@com.cealn.js//:support/vue_sfc.js")
        input[str(workspace_subpath)] = file_label
        # FIXME: Hard science specific hack
        input[
            "services/web/graphql/schema.graphql"
        ] = "@io.hardscience//services/web/graphql:schema:merge/schema.graphql"
        input = input.build()

        options = {
            "ssr": self.ssr,
            "isProd": self.build_config[CompilationMode] == Optimized,
            "runtimeModuleName": "vue",
            "relaySchema": "/src/services/web/graphql/schema.graphql",
        }

        build_sfc = self.run(
            self.node_toolchain.node,
            vue_sfc_mount,
            str(workspace_subpath),
            json.dumps(options),
            input=input,
            append_env={"BROWSERSLIST_IGNORE_OLD_DATA": "true"},
            mnemonic="VueSfc",
            progress_message=str(file_label),
        )

        script_workspace_subpath = LabelPath(f"{workspace_subpath}.script.ts")
        await self.prepare_imports(
            build_sfc.files / script_workspace_subpath,
            script_workspace_subpath,
            package_json,
            workspace_root,
            check_exist=False,
        )

        self.bundle_input.merge(build_sfc.files)

    async def handle_svelte(self, file_label, workspace_subpath: LabelPath, package_json, workspace_root):
        input = self.new_depmap()
        input.merge(self.yarn_workspace.files)
        svelte_compiler_mount = str(workspace_subpath.parent / "svelte_compiler.js")
        input[svelte_compiler_mount] = Label("@com.cealn.js//:support/svelte_compiler.js")
        input[str(workspace_subpath)] = file_label
        input = input.build()

        options = {
            "compilerOptions": {
                "generate": "ssr" if self.ssr else "dom",
                "hydratable": True,
                "css": "external",
                "dev": self.build_config[CompilationMode] != Optimized,
            }
        }

        compile_svelte = self.run(
            self.node_toolchain.node,
            svelte_compiler_mount,
            str(workspace_subpath),
            json.dumps(options),
            input=input,
            mnemonic="SvelteCompile",
            progress_message=str(file_label),
        )

        script_workspace_subpath = LabelPath(f"{workspace_subpath}.js")
        await self.prepare_imports(
            compile_svelte.files / script_workspace_subpath,
            script_workspace_subpath,
            package_json,
            workspace_root,
            check_exist=False,
        )

        self.bundle_input.merge(compile_svelte.files)

    async def handle_capnp(self, file_label, workspace_subpath, package_json, workspace_root):
        capnp_exec = await self.resolve_executable("@io.hardscience//vendor/capnproto:sdk", "capnp")
        capnpc_hs_js = await self.resolve_executable(
            "@io.hardscience//game/ingame/public/capnp-js/generator:crate",
            "capnpc-hs-js",
        )

        capnp_compile_input = self.new_depmap()
        capnp_compile_input[workspace_subpath.parent] = file_label.parent
        capnp_compile_input = capnp_compile_input.build()

        capnp_compile = self.run(
            capnp_exec.add_dependency_executable(self, capnpc_hs_js),
            "compile",
            "-ohs-js",
            f"--src-prefix={workspace_subpath.parent}",
            capnp_compile_input,
            input=capnp_compile_input,
        )

        self.bundle_input[str(workspace_subpath) + ".js"] = capnp_compile.files / (
            str(workspace_subpath.name).removesuffix(".capnp") + ".js"
        )
        self.bundle_input[workspace_subpath.parent] = capnp_compile.files

    async def handle_proto(self, file_label, workspace_subpath, package_json, workspace_root):
        protoc_exec = await self.resolve_executable("@io.hardscience//toolchains/proto:downloaded", "protoc")

        proto_compile_input = self.new_depmap()
        proto_compile_input[workspace_subpath.parent] = file_label.parent
        proto_compile_input = proto_compile_input.build()

        proto_compile = self.run(
            protoc_exec,
            f"-I={workspace_subpath.parent}",
            "--js_out=import_style=commonjs:%[srcdir]",
            "--grpc-web_out=import_style=commonjs,mode=grpcwebtext:%[srcdir]",
            str(workspace_subpath),
            input=proto_compile_input,
            mnemonic="ProtocGrpcWeb",
            progress_message=str(file_label),
        )

        file_stem = str(workspace_subpath.name).removesuffix(".proto")
        self.bundle_input[str(workspace_subpath) + ".js"] = DepmapBuilder.file(
            f"export * from './{file_stem}_grpc_web_pb'"
        )
        self.bundle_input[workspace_subpath.parent] = proto_compile.files

    async def handle_routes(self, file_label, workspace_subpath, package_json, workspace_root):
        # FIXME: hard science specific
        from workspaces.com_cealn_python.providers import PythonToolchain

        python = await self.resolve_global_provider(PythonToolchain, host=True)

        real_routes_folder_workspace_subpath = workspace_subpath.parent / "routes"

        routes_index_input = self.new_depmap()
        routes_index_input[real_routes_folder_workspace_subpath] = file_label.parent / "routes"
        routes_index_input["build_routes.py"] = "@io.hardscience//services/web/common:tools/build_routes.py"
        routes_index_input = routes_index_input.build()

        routes_index = self.run(
            python.python,
            "build_routes.py",
            str(real_routes_folder_workspace_subpath),
            input=routes_index_input,
            mnemonic="WebRoutesIndex",
            progress_message=str(file_label),
        )

        generated_workspace_subpath = LabelPath(str(workspace_subpath) + ".js")
        self.bundle_input[generated_workspace_subpath] = routes_index.files / "index.js"
        await self.prepare_imports(
            routes_index.files / "index.js",
            generated_workspace_subpath,
            package_json,
            workspace_root,
            check_exist=False,
        )


def import_spec_matches_package(importspec: str, package: str):
    if not importspec.startswith(package):
        return False
    subpath = importspec.removeprefix(package)
    return subpath == "" or subpath.startswith("/")


_WORKSPACE_DEPENDENCY_REGEX = re.compile(r"workspace:(.+)")
