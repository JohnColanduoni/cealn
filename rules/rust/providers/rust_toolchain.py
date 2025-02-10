from typing import Dict, List, Optional

from cealn.exec import Executable
from cealn.provider import Provider, Field
from cealn.label import Label


class Rustc(Provider):
    host = Field(str)
    supported_targets = Field(List[str])
    executable = Field(Executable)


class RustStd(Provider):
    target = Field(str)
    files = Field(Label)
    stdlib_filenames = Field(List[str])
    testlib_filenames = Field(List[str])
    proc_macro_path = Field(str)


class Cargo(Provider):
    host = Field(str)
    executable = Field(Executable)


class RustToolchain(Provider):
    rustc = Field(Optional[Rustc])
    rust_std = Field(Optional[RustStd])
    cargo = Field(Optional[Cargo])
    rust_analyzer = Field(Optional[Executable])
    rust_src = Field(Optional[Label])
