from typing import Dict, List

from cealn.exec import Executable
from cealn.provider import Provider, Field


class GoToolchain(Provider):
    go = Field(Executable)
