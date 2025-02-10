from typing import Dict, Any, List, Optional
import json

from cealn.label import Label, LabelPath
from cealn.provider import Field, Provider


class Workspace(Provider):
    root = Field(Label)
    workspace_label = Field(Label)
    metadata_file = Field(Label)
    registry = Field(Label)

    global_rustflags = Field(List[str])

    async def metadata_for_package(self, package_id, *, rule):
        # TODO: reduce metadata to only what package needs to read to speed up analysis
        with await rule.open_file(self.metadata_file, encoding="utf-8") as f:
            return json.load(f)
