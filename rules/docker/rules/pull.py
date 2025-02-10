import json

from cealn.rule import Rule
from cealn.attribute import Attribute
from cealn.platform import Os, Arch, X86_64, Linux

from ..providers import DockerLayer, DockerImage


class DockerPull(Rule):
    image = Attribute[str]()

    async def analyze(self):
        os = _OS_MAP[self.build_config[Os]]
        arch = _ARCH_MAP[self.build_config[Arch]]

        support_exe = await self.resolve_executable("@com.cealn.docker//support:crate", "cealn-rules-docker-support")
        metadata_run = await self.run(
            support_exe,
            "metadata",
            "--output",
            "metadata.json",
            "--os",
            os,
            "--architecture",
            arch,
            self.image,
            id="metadata",
            mnemonic="DockerPullManifest",
            progress_message=f"{self.image}",
        )
        with await metadata_run.files.open_file("metadata.json", encoding="utf-8") as f:
            metadata = json.load(f)

        layers = []
        for layer_metadata in metadata["layers"]:
            layer_dl = self.run(
                support_exe,
                "blob",
                "--output",
                "blob.tar.gz",
                self.image,
                layer_metadata["digest"],
                mnemonic="DockerPullBlob",
                progress_message=f"{self.image} {layer_metadata['digest']}",
            )
            layers.append(
                DockerLayer(
                    digest=layer_metadata["digest"],
                    digest_source_tag=self.image,
                    diff_id=layer_metadata["diff_id"],
                    media_type=layer_metadata["media_type"],
                    blob=layer_dl.files / "blob.tar.gz",
                )
            )

        return [DockerImage(tag=self.image, layers=layers, run_config=metadata["run_config"])]


_ARCH_MAP = {
    X86_64: "amd64",
}

_OS_MAP = {
    Linux: "linux",
}
