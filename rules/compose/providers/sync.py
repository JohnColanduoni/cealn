from typing import Optional
from cealn.label import Label, LabelPath
from cealn.provider import Provider, Field
from cealn.depmap import DepmapBuilder


class ComposeVolume(Provider):
    files = Field(Label)

    persistent_volume_claim = Field(str)
    namespace = Field(str)

    sync_pod = Field(str)
    sync_pod_container = Field(str)
    sync_pod_module = Field(str)
