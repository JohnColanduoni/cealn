from cealn.rule import Rule
from cealn.attribute import ProviderAttribute, GlobalProviderAttribute, FileAttribute
from cealn.label import Label, LabelPath

from ..providers import YarnToolchain, NodeToolchain, YarnWorkspace


class Tsc(Rule):
    package_json = FileAttribute(default="package.json")

    # TODO: autodetect instead of assuming location
    yarn_workspace = ProviderAttribute(YarnWorkspace, default="//:yarn_workspace", host=True)

    yarn_toolchain = GlobalProviderAttribute(YarnToolchain, host=True)
    node_toolchain = GlobalProviderAttribute(NodeToolchain, host=True)

    def analyze(self):
        # FIXME: detect
        workspace_root = Label("@io.hardscience//")
        workspace_subpath = LabelPath(str(self.package_json.parent.relative_to(workspace_root)))

        tsc_input = self.new_depmap("tsc-input")
        tsc_input.merge(self.yarn_workspace.files)
        tsc_input[workspace_subpath] = self.package_json.parent
        tsc_input = tsc_input.build()

        tsc = self.run(
            self.yarn_toolchain.yarn,
            "tsc",
            cwd=f"%[srcdir]/{workspace_subpath}",
            input=tsc_input,
            id="tsc-internal",
        )

        tsc_projected = self.new_depmap("tsc")
        tsc_projected.merge(tsc.files / workspace_subpath)
        tsc_projected.build()
