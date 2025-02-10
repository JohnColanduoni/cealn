import json
import sys
import os.path
from pathlib import Path
import re

from cealn.rule import Rule
from cealn.attribute import FileAttribute, GlobalProviderAttribute, Attribute, LabelAttribute, LabelMapAttribute
from cealn.exec import Executable
from cealn.depmap import DepmapBuilder
from cealn.label import LabelPath, Label

from workspaces.com_cealn_cc.config import llvm_target_triple

from ..providers.rust_toolchain import RustToolchain
from ..providers.workspace import Workspace

# FIXME: pull this in more gracefully, maybe don't vendor
sys.path.insert(0, str(Path(__file__).parents[1] / "vendor" / "toml"))
import toml


class CargoWorkspace(Rule):
    workspace_toml = FileAttribute(default=":Cargo.toml")
    source_root = LabelAttribute(default="")

    features = Attribute(default=[])
    global_rustflags = Attribute(default=[])

    rust_toolchain = GlobalProviderAttribute(RustToolchain, host=True)

    no_lock = Attribute(default=False)
    no_std_targets = Attribute(default=[])
    extra_inputs = LabelMapAttribute(default={})
    metadata_hash_extra = Attribute(default={})

    async def analyze(self):
        cargo = self.rust_toolchain.cargo

        # FIXME: fix label handling so this is not necessary
        self.source_root = Label(str(self.source_root).removesuffix(":"))

        workspace_toml_contents = await self.build_file_contents(self.workspace_toml, encoding="utf-8")

        workspace_toml = toml.loads(workspace_toml_contents)

        try:
            if workspace_toml["workspace"].get("resolver") != "2":
                raise RuntimeError("only cargo resolver v2 is currently supported")
        except KeyError:
            raise RuntimeError("missing `workspace` in workspace Cargo.toml")

        try:
            members = workspace_toml["workspace"]["members"]
        except KeyError:
            raise RuntimeError("missing `workspace.members` in workspace Cargo.toml")

        workspace_root = self.workspace_toml.parent
        workspace_mount = LabelPath(str(workspace_root.relative_to(self.source_root)))
        package_skeleton = self.new_depmap()
        package_skeleton[workspace_mount / "Cargo.toml"] = self.workspace_toml
        if not self.no_lock:
            package_skeleton[workspace_mount / "Cargo.lock"] = workspace_root / "Cargo.lock"

        workspaces_to_process = list()
        packages_to_process = list(LabelPath(member) for member in members)
        detected_packages = set(packages_to_process)
        detected_workspaces = set(workspaces_to_process)

        if "package" in workspace_toml:
            packages_to_process.append(LabelPath(""))

        # Add paths from patches
        for patch_source in workspace_toml.get("patch", {}).values():
            for patch in patch_source.values():
                if "path" in patch:
                    path = LabelPath(patch["path"])
                    packages_to_process.append(path)
                    detected_packages.add(path)

        while packages_to_process or workspaces_to_process:
            while workspaces_to_process:
                workspace_path = workspaces_to_process.pop()
                package_skeleton[workspace_mount / workspace_path / "Cargo.toml"] = (
                    workspace_root / workspace_path / "Cargo.toml"
                )
                subworkspace_toml_contents = await self.build_file_contents(
                    workspace_root / workspace_path / "Cargo.toml", encoding="utf-8"
                )
                subworkspace_toml = toml.loads(subworkspace_toml_contents)
                for dependency in subworkspace_toml.get("workspace", {}).get("dependencies", {}).values():
                    if "path" in dependency:
                        dependency_path = (workspace_path / dependency["path"]).normalize()
                        if dependency_path not in detected_packages:
                            packages_to_process.append(dependency_path)
                            detected_packages.add(dependency_path)

            if not packages_to_process:
                continue
            package_path = packages_to_process.pop()

            package_toml_label = workspace_root / package_path / "Cargo.toml"
            package_skeleton[workspace_mount / package_path / "Cargo.toml"] = package_toml_label

            package_toml_contents = await self.build_file_contents(package_toml_label, encoding="utf-8")
            package_toml = toml.loads(package_toml_contents)

            # Find path depdnencies
            dependencies = list(package_toml.get("dependencies", {}).values())
            dependencies.extend(package_toml.get("build-dependencies", {}).values())
            dependencies.extend(package_toml.get("dev-dependencies", {}).values())
            for target_stanza in package_toml.get("target", {}).values():
                dependencies.extend(target_stanza.get("dependencies", {}).values())
                dependencies.extend(target_stanza.get("dev-dependencies", {}).values())
                dependencies.extend(target_stanza.get("build-dependencies", {}).values())
            has_workspace_dependency = False
            for dependency in dependencies:
                if "path" in dependency:
                    dependency_path = (package_path / dependency["path"]).normalize()
                    if dependency_path not in detected_packages:
                        packages_to_process.append(dependency_path)
                        detected_packages.add(dependency_path)
                if "workspace" in dependency:
                    has_workspace_dependency = True

            if has_workspace_dependency:
                # This package uses a workspace dependency of its parent workspace, so cargo needs to see the workspace
                # manifest
                # FIXME: do real detection here
                current_parent = package_path.parent
                found_workspace = None
                while True:
                    if await self.file_exists(workspace_root / current_parent / "Cargo.toml"):
                        found_workspace = current_parent
                        break
                    new_parent = current_parent.parent
                    if new_parent == current_parent:
                        break
                    else:
                        current_parent = new_parent
                if not found_workspace:
                    raise RuntimeError("failed to find workspace")
                if found_workspace not in detected_workspaces:
                    workspaces_to_process.append(found_workspace)
                    detected_workspaces.add(found_workspace)

            # Insert empty files for `src/lib.rs` etc. as needed to ensure cargo autodetects correctly
            if not ("lib" in package_toml and "path" in package_toml["lib"]):
                lib_rs_source = workspace_root / package_path / "src" / "lib.rs"
                if await self.file_exists(lib_rs_source):
                    package_skeleton[workspace_mount / package_path / "src" / "lib.rs"] = DepmapBuilder.file("")
            main_rs_source = workspace_root / package_path / "src" / "main.rs"
            if await self.file_exists(main_rs_source):
                package_skeleton[workspace_mount / package_path / "src" / "main.rs"] = DepmapBuilder.file("")
            bin_dir_source = workspace_root / package_path / "src" / "bin"
            if await self.file_exists(bin_dir_source):
                # FIXME: use empty files here too
                package_skeleton[workspace_mount / package_path / "src" / "bin"] = DepmapBuilder.glob(
                    bin_dir_source, "*.rs"
                )
            build_rs_source = workspace_root / package_path / "build.rs"
            if await self.file_exists(build_rs_source):
                package_skeleton[workspace_mount / package_path / "build.rs"] = DepmapBuilder.file("")

        package_skeleton = package_skeleton.build()

        extra_metadata_args = []
        if self.features:
            extra_metadata_args += ["--features", ",".join(self.features)]

        cargo_metadata_args = [
            "metadata",
            "--manifest-path",
            str(workspace_mount / "Cargo.toml"),
            # FIXME: get this working
            # "--filter-platform",
            # llvm_target_triple(self.build_config),
            # "--filter-platform",
            # llvm_target_triple(self.host_build_config),
            *extra_metadata_args,
            "--format-version=1",
            "--quiet",
            "--color=always",
        ]
        if not self.no_lock:
            cargo_metadata_args += ["--locked"]

        cargo_metadata_run = self.run(
            cargo.executable,
            *cargo_metadata_args,
            input=package_skeleton,
            append_env={"CARGO_HOME": "%[srcdir]/.cargo"},
            hide_stdout=True,
            id="metadata",
            mnemonic="CargoMetadata",
            progress_message=str(self.workspace_toml),
        )
        cargo_metadata_out = await cargo_metadata_run
        with await cargo_metadata_out.open_stdout(encoding="utf-8") as f:
            cargo_metadata = json.load(f)
        registry = cargo_metadata_run.files

        check_depmap = self.new_depmap("check")

        packages = {}
        package_tasks = []
        for package_data in cargo_metadata["packages"]:
            package_task = self.prepare_package(
                package_data,
                registry=registry,
                src_root_path=cargo_metadata_run.platform.execution_sysroot_input_dest,
                src_root_label=self.source_root,
            )
            package_tasks.append(package_task)
        for package in await self.gather(package_tasks):
            if package["source"] is None:
                check_depmap.merge(Label(package["cealn_target"]).join_action("check-all"))
            packages[package["id"]] = package
        cargo_metadata["packages"] = packages

        cargo_metadata["resolve"]["nodes"] = {node["id"]: node for node in cargo_metadata["resolve"]["nodes"]}

        check_depmap = check_depmap.build()

        metadata_depmap = self.new_depmap("updated-metadata")
        metadata_depmap["metadata.json"] = DepmapBuilder.file(json.dumps(cargo_metadata))
        metadata_depmap = metadata_depmap.build()

        return [
            Workspace(
                root=self.source_root,
                workspace_label=self.label,
                metadata_file=metadata_depmap / "metadata.json",
                registry=registry,
                global_rustflags=self.global_rustflags,
            )
        ]

    async def prepare_package(self, package_data, *, registry, src_root_path, src_root_label):
        from .package import CargoPackage

        for target_data in package_data["targets"]:
            src_path = LabelPath(target_data["src_path"])
            src_path = str(src_path.relative_to(src_root_path))
            target_data["src_path"] = src_path

        # FIXME: detect source root dynamically
        manifest_path = LabelPath(package_data["manifest_path"]).relative_to(LabelPath("/src"))
        package_data["manifest_path"] = str(manifest_path)

        if package_data["source"]:
            if package_data["source"] == "registry+https://github.com/rust-lang/crates.io-index":
                synthetic_target_name = f"{package_data['name']}-{package_data['version']}"
            else:
                synthetic_target_name = re.sub(_NON_ALPHA_CHARACTER_REGEX, "_", package_data["id"])
            cealn_target = self.synthetic_target(
                CargoPackage,
                name=synthetic_target_name,
                cargo_toml=registry / manifest_path,
                workspace=self.label,
                package_id=package_data["id"],
                # Don't report warnings in dependencies
                extra_rustflags=["--cap-lints=allow"],
                extra_inputs=self.extra_inputs,
                no_std_targets=self.no_std_targets,
                metadata_hash_extra=self.metadata_hash_extra,
            )
        else:
            cealn_target = (src_root_label / manifest_path.parent).join_action("crate")
            if not await self.target_exists(cealn_target):
                cealn_target = self.synthetic_target(
                    CargoPackage,
                    name=package_data["name"],
                    cargo_toml=src_root_label / manifest_path,
                    workspace=self.label,
                    package_id=package_data["id"],
                    extra_inputs=self.extra_inputs,
                    no_std_targets=self.no_std_targets,
                    metadata_hash_extra=self.metadata_hash_extra,
                )

        package_data["cealn_target"] = str(cealn_target)
        return package_data


_NON_ALPHA_CHARACTER_REGEX = re.compile(r"[^A-Za-z0-9-_]")
