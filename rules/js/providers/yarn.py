from typing import Dict, List

from cealn.exec import Executable
from cealn.provider import Provider, Field
from cealn.label import Label


class YarnToolchain(Provider):
    yarn = Field(Executable)


class YarnWorkspace(Provider):
    files = Field(Label)
