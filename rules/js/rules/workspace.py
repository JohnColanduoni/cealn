import json
from cealn.action import StructuredMessageConfig

from cealn.rule import Rule
from cealn.attribute import FileAttribute, GlobalProviderAttribute
from cealn.label import LabelPath
from ..providers import YarnToolchain, NodeToolchain, YarnWorkspace as YarnWorkspaceProvider


class YarnWorkspace(Rule):
    package_json = FileAttribute()

    yarn_toolchain = GlobalProviderAttribute(YarnToolchain, host=True)
    node_toolchain = GlobalProviderAttribute(NodeToolchain, host=True)

    async def analyze(self):
        root_package_json_contents = await self.build_file_contents(self.package_json, encoding="utf-8")

        root_package_json = json.loads(root_package_json_contents)

        workspace_root = self.package_json.parent
        package_skeleton = self.new_depmap()
        package_skeleton["package.json"] = self.package_json
        package_skeleton["yarn.lock"] = workspace_root / "yarn.lock"
        package_skeleton[".yarnrc.yml"] = workspace_root / ".yarnrc.yml"

        packages_to_process = list(LabelPath(member) for member in root_package_json["workspaces"])
        while packages_to_process:
            package_path = packages_to_process.pop()

            package_json_label = workspace_root / package_path / "package.json"
            package_skeleton[package_path / "package.json"] = package_json_label

            package_json = json.loads(await self.build_file_contents(package_json_label, encoding="utf-8"))
            if "workspaces" in package_json:
                packages_to_process.extend(package_path / member for member in package_json["workspaces"])

        package_skeleton = package_skeleton.build()

        install = self.run(
            self.yarn_toolchain.yarn,
            "install",
            "--immutable",
            "--json",
            input=package_skeleton,
            id="install",
            mnemonic="YarnInstall",
            progress_message=str(self.package_json),
            structured_messages=_YARN_INSTALL_STRUCTURED_MESSAGE_CONFIG,
        )

        files = self.new_depmap()
        files.merge(package_skeleton)
        files.merge(install.files)
        files = files.build()

        return [YarnWorkspaceProvider(files=files)]


_YARN_INSTALL_STRUCTURED_MESSAGE_CONFIG = StructuredMessageConfig()
_YARN_INSTALL_STRUCTURED_MESSAGE_CONFIG.level_map["$.[?($.type=='error')]"] = "error"
_YARN_INSTALL_STRUCTURED_MESSAGE_CONFIG.level_map["$.[?($.type=='warning')]"] = "warn"
