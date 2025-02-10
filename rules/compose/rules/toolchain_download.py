from cealn.exec import Executable
from cealn.rule import Rule
from cealn.attribute import Attribute

from ..providers import KustomizeToolchain


class DownloadKustomizeToolchain(Rule):
    version = Attribute[str]()

    async def analyze(self):
        # FIXME: handle platform
        archive_filename = "kustomize.tar.gz"
        archive_dl = self.download(
            f"https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize%2Fv{self.version}/kustomize_v{self.version}_linux_amd64.tar.gz",
            filename=archive_filename,
            id="archive-dl",
        )
        archive_extract = self.extract(archive_dl.files / archive_filename, id="archive-extract")

        context = self.new_depmap("context")
        context["bin"] = archive_extract.files
        context = context.build()

        kustomize = Executable(
            name="kustomize",
            executable_path="%[execdir]/bin/kustomize",
            context=context,
        )

        return [KustomizeToolchain(kustomize=kustomize), kustomize]
