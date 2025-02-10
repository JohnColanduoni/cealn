from typing import Dict, List
from cealn.label import Label
from cealn.provider import Provider, Field


class NinjaInput(Provider):
    build_root = Field(str)
    input = Field(Label)
    exec_context = Field(Label)
    append_env = Field(Dict[str, str], default={})
    search_paths = Field(List[str], default=[])
