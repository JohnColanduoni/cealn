from cealn.attribute import Attribute
from cealn.exec import Executable
from cealn.rule import Rule


class DownloadProtoc(Rule):
    version = Attribute[str]()

    async def analyze(self):
        archive_filename = "protoc.zip"
        # FIXME: handle platform/arch
        archive_dl = self.download(
            f"https://github.com/protocolbuffers/protobuf/releases/download/v{self.version}/protoc-{self.version}-linux-x86_64.zip",
            filename=archive_filename,
            id="download",
        )
        archive_extract = self.extract(archive_dl.files / archive_filename, id="extract")

        # FIXME: handle platform/arch
        javascript_archive_filename = "protobuf-javascript.zip"
        javascript_archive_dl = self.download(
            "https://github.com/protocolbuffers/protobuf-javascript/releases/download/v3.21.2/protobuf-javascript-3.21.2-linux-x86_64.zip",
            filename=javascript_archive_filename,
        )
        javascript_extract = self.extract(javascript_archive_dl.files / javascript_archive_filename)

        # FIXME: handle platform/arch
        grpc_web_dl = self.download(
            "https://github.com/grpc/grpc-web/releases/download/1.4.2/protoc-gen-grpc-web-1.4.2-linux-x86_64",
            filename="protoc-gen-grpc-web",
            executable=True,
        )

        context = self.new_depmap("context")
        context.merge(archive_extract.files)
        context.merge(javascript_extract.files)
        context["bin/protoc-gen-grpc-web"] = grpc_web_dl.files / "protoc-gen-grpc-web"
        context = context.build()

        protoc = Executable(
            name="protoc", executable_path="%[execdir]/bin/protoc", context=context, search_paths=["bin"]
        )

        return [protoc]
