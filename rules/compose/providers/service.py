from typing import Optional
from cealn.label import Label, LabelPath
from cealn.provider import Provider, Field
from cealn.depmap import DepmapBuilder

from workspaces.com_cealn_cue.providers import CueToolchain

from .kustomize import KustomizeToolchain


class K8Service(Provider):
    name = Field(str)


class KustomizeService(K8Service):
    name = Field(str)
    kustomize_files = Field(Label)
    kustomize_subpath = Field(Optional[str], default=None)

    async def build_manifests(self, *, rule, images, default_repo=None, global_labels={}):
        kustomize_toolchain = await rule.resolve_global_provider(KustomizeToolchain, host=True)

        support_exe = await rule.resolve_executable("@com.cealn.compose//support:crate", "cealn-rules-compose-support")

        prepare_entrypoint_input = rule.new_depmap()
        for image_name, image_data in images.items():
            escaped_image_name = image_name.replace("/", "_")
            prepare_entrypoint_input[f"images/{escaped_image_name}/name.txt"] = DepmapBuilder.file(image_name)
            prepare_entrypoint_input[f"images/{escaped_image_name}"] = image_data["tagger"]
        prepare_entrypoint_input = prepare_entrypoint_input.build()

        if self.kustomize_subpath is None:
            main_path = "__base"
        else:
            main_path = str(LabelPath("__base") / self.kustomize_subpath)

        entrypoint_opts = []

        if default_repo is not None:
            entrypoint_opts += ["--default-repo", default_repo]

        for k, v in global_labels.items():
            entrypoint_opts += [f"--label={k}={v}"]

        prepare_entrypoint = rule.run(
            support_exe,
            "kustomize-entrypoint",
            *entrypoint_opts,
            main_path,
            input=prepare_entrypoint_input,
            mnemonic="KustomizePrepare",
            progress_message=f"{self.kustomize_files}",
        )

        kustomize_input = rule.new_depmap()
        kustomize_input.merge(prepare_entrypoint.files)
        kustomize_input["__base"] = self.kustomize_files
        kustomize_input = kustomize_input.build()

        kustomize_run = rule.run(
            kustomize_toolchain.kustomize,
            "build",
            "--output",
            "%[srcdir]",
            input=kustomize_input,
            mnemonic="Kustomize",
            progress_message=f"{self.kustomize_files}",
        )
        return [kustomize_run.files]


class CueService(K8Service):
    name = Field(str)
    files = Field(Label)
    package = Field(str, default="k8s")

    async def build_manifests(self, *, rule, images, default_repo=None, global_labels={}):
        cue_toolchain = await rule.resolve_global_provider(CueToolchain, host=True)

        support_exe = await rule.resolve_executable("@com.cealn.compose//support:crate", "cealn-rules-compose-support")

        prepare_entrypoint_input = rule.new_depmap()
        for image_name, image_data in images.items():
            escaped_image_name = image_name.replace("/", "_")
            prepare_entrypoint_input[f"images/{escaped_image_name}/name.txt"] = DepmapBuilder.file(image_name)
            prepare_entrypoint_input[f"images/{escaped_image_name}"] = image_data["tagger"]
        prepare_entrypoint_input = prepare_entrypoint_input.build()

        entrypoint_opts = []

        if default_repo is not None:
            entrypoint_opts += ["--default-repo", default_repo]

        for k, v in global_labels.items():
            entrypoint_opts += [f"--label={k}={v}"]

        prepare_entrypoint = rule.run(
            support_exe,
            "cue-entrypoint",
            *entrypoint_opts,
            input=prepare_entrypoint_input,
            mnemonic="CueK8sPrepare",
            progress_message=f"{self.files}",
        )

        cue_input = rule.new_depmap()
        cue_input.merge(self.files)
        cue_input.merge(prepare_entrypoint.files)
        cue_input = cue_input.build()

        cue_run = rule.run(
            cue_toolchain.cue,
            "eval",
            f"./{self.package}",
            "--out=text",
            "--outfile=%[srcdir]/objects.yaml",
            "--expression",
            # TODO: get rid of the need for the intermediate exports object
            """yaml.MarshalStream([for k, v in exports { v }])""",
            input=cue_input,
            mnemonic="CueK8s",
            progress_message=f"{self.files}",
        )

        return [cue_run.files]
