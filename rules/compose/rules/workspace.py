import json
from typing import Optional
from cealn.label import LabelPath
from cealn.rule import Rule
from cealn.attribute import Attribute, LabelListAttribute
from cealn.depmap import DepmapBuilder

from workspaces.com_cealn_docker.providers import DockerImage, DockerLayer

from ..providers import K8Service, ComposeVolume


class ComposeWorkspace(Rule):
    sources = LabelListAttribute()

    default_repo = Attribute[Optional[str]](default=None)

    port_forwards = Attribute(default=[])
    global_labels = Attribute(default={})

    async def analyze(self):
        self._support_exe = None

        self.resources = self.new_depmap("resources")

        self.images = {}
        self.volumes = []

        await self.gather([self.handle_source(source_label) for source_label in self.sources])

        self.resources = self.resources.build()

        manifest_opts = []

        if self.default_repo is not None:
            manifest_opts += ["--default-repo", self.default_repo]

        support_exe = await self.support_exe()
        manifest_run = self.run(
            support_exe,
            "manifest",
            *manifest_opts,
            "manifest.json",
            json.dumps({image_name: image_data["metadata"] for image_name, image_data in self.images.items()}),
            json.dumps(self.port_forwards),
            json.dumps(self.volumes),
            input=self.resources,
            id="manifest",
            mnemonic="ComposeManifest",
            progress_message=str(self.label),
        )

        output = self.new_depmap("output")
        output.merge(self.resources)
        output.merge(manifest_run.files)
        output = output.build()

    async def handle_source(self, source_label):
        providers = await self.load_providers(source_label)
        await self.gather(
            [self.handle_docker_image(provider) for provider in providers if isinstance(provider, DockerImage)]
        )
        await self.gather(
            [self.handle_service(provider) for provider in providers if isinstance(provider, K8Service)]
            + [self.handle_volume(provider) for provider in providers if isinstance(provider, ComposeVolume)]
        )

    async def handle_docker_image(self, image):
        image_output_subpath = LabelPath("images") / image.tag

        image_resources = self.new_depmap()

        layer_metadatas = []
        for layer_index, layer in enumerate(image.layers):
            layer_output_subpath = image_output_subpath / str(layer_index)
            if layer.files:
                image_resources[layer_output_subpath] = layer.files
                layer_metadatas.append({"loose": str(layer_output_subpath)})
            elif layer.blob:
                blob_filename = f"{layer_output_subpath}.tar.gz"
                image_resources[blob_filename] = layer.blob
                layer_metadatas.append(
                    {
                        "blob": {
                            "filename": str(blob_filename),
                            "digest": layer.digest,
                            "diff_id": layer.diff_id,
                            "media_type": layer.media_type,
                        }
                    }
                )
            else:
                raise RuntimeError("unsupported layer type")

        image_resources = image_resources.build()

        self.resources.merge(image_resources)

        image_metadata = {
            "layers": layer_metadatas,
            "run_config": image.run_config,
        }
        support_exe = await self.support_exe()
        image_tagger = self.run(
            support_exe,
            "image-tag",
            image.tag,
            json.dumps(image_metadata),
            input=image_resources,
            mnemonic="ComposeImageTag",
            progress_message=str(image.tag),
        )

        self.resources[image_output_subpath] = image_tagger.files

        self.images[image.tag] = {
            "metadata": image_metadata,
            "tagger": image_tagger.files,
        }

    async def handle_service(self, service):
        manifests = await service.build_manifests(
            rule=self, images=self.images, default_repo=self.default_repo, global_labels=self.global_labels
        )
        for manifest_label in manifests:
            self.resources[LabelPath("manifests") / service.name] = manifest_label

    async def handle_volume(self, volume: ComposeVolume):
        self.volumes.append(
            {
                "persistent_volume_claim": volume.persistent_volume_claim,
                "namespace": volume.namespace,
                "sync_pod": volume.sync_pod,
                "sync_pod_module": volume.sync_pod_module,
            }
        )
        self.resources[LabelPath("volumes") / volume.namespace / volume.persistent_volume_claim] = volume.files

    async def support_exe(self):
        if not self._support_exe:
            self._support_exe = await self.resolve_executable(
                "@com.cealn.compose//support:crate", "cealn-rules-compose-support"
            )
        return self._support_exe
