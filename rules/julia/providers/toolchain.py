from typing import Dict, List

from cealn.exec import Executable
from cealn.provider import Provider, Field


class JuliaToolchain(Provider):
    julia = Field(Executable)
