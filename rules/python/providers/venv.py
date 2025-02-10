from typing import Dict, List

from cealn.exec import Executable
from cealn.provider import Provider, Field
from cealn.label import Label

class PythonVirtualEnv(Provider):
    files = Field(Label)
