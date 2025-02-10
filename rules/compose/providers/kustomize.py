from cealn.provider import Provider, Field
from cealn.exec import Executable


class KustomizeToolchain(Provider):
    kustomize = Field(Executable)
