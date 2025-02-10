from typing import Dict, List, Optional
from cealn.label import Label
from cealn.provider import Field, Provider


class DockerLayer(Provider):
    files = Field(Optional[Label], default=None)
    blob = Field(Optional[Label], default=None)
    digest = Field(Optional[str], default=None)
    digest_source_tag = Field(Optional[str], default=None)
    diff_id = Field(Optional[str], default=None)
    media_type = Field(Optional[str], default=None)


class DockerImage(Provider):
    tag = Field(str)
    layers = Field(List[DockerLayer])
    run_config = Field(Optional[Dict[str, object]], default=None)
