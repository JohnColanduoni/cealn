from __future__ import annotations
from pathlib import Path, PurePosixPath
from typing import Optional, Union
import re
import site
import os


class Label:
    _repr: str

    def __init__(self, repr: str):
        if not isinstance(repr, str):
            raise TypeError("labels must be strings")
        # FIXME: check format
        self._repr = repr

    @property
    def is_package_relative(self) -> bool:
        return not self.is_workspace_relative and not self.is_workspace_absolute

    @property
    def is_workspace_relative(self) -> bool:
        return self._repr.startswith("//")

    @property
    def is_workspace_absolute(self) -> bool:
        return self._repr.startswith("@")

    @property
    def workspace_name(self) -> Optional[str]:
        match = _ABSOLUTE_WORKSPACE_REGEX.match(self._repr)
        if match is None:
            return None
        return match.group(1)

    @property
    def file_name(self) -> str:
        return re.split(r":|/", self._repr)[-1]

    def join(self, rhs: Union[str, Label, LabelPath]) -> Label:
        if isinstance(rhs, LabelPath):
            rhs = str(rhs)
        if isinstance(rhs, str):
            rhs = Label(rhs)
        if not isinstance(rhs, Label):
            raise TypeError("expected string or label")

        if rhs.is_workspace_absolute:
            return rhs
        elif rhs.is_workspace_relative:
            workspace_name = self.workspace_name
            if workspace_name is not None:
                return Label(f"@{workspace_name}{rhs}")
            else:
                return rhs
        else:
            # Package relative
            # FIXME: not right for paths with colon
            # Note that we don't allow leading slashes in relative paths, so no need to check cases here
            if not self._repr.endswith("/"):
                return Label(self._repr + "/" + rhs._repr)
            else:
                return Label(self._repr + rhs._repr)

    def join_action(self, rhs: str) -> Label:
        # FIXME: this won't always work
        if self._repr.endswith("/") and not self._repr.endswith("//"):
            return Label(self._repr[:-1] + ":" + rhs)
        else:
            return Label(self._repr + ":" + rhs)

    @property
    def parent(self) -> Optional[Label]:
        # FIXME: handle more cases, particularly around colons
        last_segment = re.split(r"\:|/", self._repr)[-1]
        return Label(self._repr[: -(len(last_segment) + 1)])

    @property
    def package(self) -> Optional[Label]:
        # FIXME: handle more cases, particularly around colons
        last_segment = re.split(r"\:", self._repr, maxsplit=1)[-1]
        return Label(self._repr[: -(len(last_segment) + 1)])

    def to_source_file_path(self) -> Path:
        """
        Produces a readable path assuming this label refers to a source file
        """
        match = _ABSOLUTE_WORKSPACE_REGEX.match(self._repr)
        if match is None:
            raise ValueError("label must be workspace absolute to be converted to a path")

        workspace_name = match.group(1)
        relative_path = self._repr[len(match.group(0)) :].replace(":", "/").lstrip("/")

        if workspace_name == "com.cealn.builtin":
            return Path(site.getsitepackages()[0]) / "cealn" / relative_path

        return Path("/workspaces") / match.group(1) / relative_path

    def relative_to(self, ancestor: Label) -> Label:
        if isinstance(ancestor, str):
            ancestor = Label(ancestor)
        # FIXME: this is a hack
        return Label(str(self.to_source_file_path().relative_to(ancestor.to_source_file_path())))

    def to_python_module_name(self) -> Path:
        match = _ABSOLUTE_WORKSPACE_REGEX.match(self._repr)
        if match is None:
            raise ValueError("label must be workspace absolute to be converted to a path")

        workspace_name = match.group(1)
        workspace_segment = workspace_name.replace(".", "_")
        relative_path = self._repr[len(match.group(0)) :].replace(":", ".").replace("/", ".").lstrip(".")
        if relative_path.endswith(".py"):
            relative_path = relative_path[:-3]

        if workspace_name == "com.cealn.builtin":
            return f"cealn.{relative_path}"

        return f"workspaces.{workspace_segment}.{relative_path}"

    def __str__(self):
        return self._repr

    def __repr__(self):
        return f"Label({self._repr!r})"

    def __eq__(self, obj):
        return isinstance(obj, Label) and self._repr == obj._repr

    def __hash__(self):
        return self._repr.__hash__()

    def __truediv__(self, obj: Union[str, Label]) -> Label:
        return self.join(obj)


class LabelPath:
    _repr: str

    def __init__(self, repr: str):
        if not isinstance(repr, str):
            raise TypeError("labels must be strings")
        if repr == ".":
            repr = ""
        # FIXME: check format
        self._repr = repr

    @property
    def name(self) -> str:
        # TODO: do this ourselves, super lazy
        return PurePosixPath(self._repr).name

    @property
    def parent(self) -> LabelPath:
        # TODO: do this ourselves, super lazy
        parent_str = str(PurePosixPath(self._repr).parent)
        if parent_str == ".":
            return LabelPath("")
        else:
            return LabelPath(parent_str)

    def join(self, rhs: Union[str, LabelPath]) -> LabelPath:
        if isinstance(rhs, str):
            rhs = LabelPath(rhs)
        if not isinstance(rhs, LabelPath):
            raise TypeError("expected string or label path")
        # TODO: do this ourselves, super lazy
        return LabelPath(str(PurePosixPath(self._repr) / PurePosixPath(rhs._repr)))

    def relative_to(self, base: Union[str, LabelPath]) -> LabelPath:
        if isinstance(base, str):
            base = LabelPath(base)
        if not isinstance(base, LabelPath):
            raise TypeError("expected string or label path")
        # TODO: do this ourselves, super lazy
        return LabelPath(str(PurePosixPath(self._repr).relative_to(PurePosixPath(base._repr))))

    def normalize(self) -> LabelPath:
        # TODO: do this ourselves, super lazy
        normalized = PurePosixPath(os.path.normpath(self._repr))
        return LabelPath(str(normalized))

    def normalize_require_descending(self) -> LabelPath:
        # TODO: do this ourselves, super lazy
        normalized = PurePosixPath(os.path.normpath(self._repr))
        if ".." in normalized.parts:
            raise ValueError(f"provided LabelPath {self} escapes the root")
        return LabelPath(str(normalized))

    def __str__(self):
        return self._repr

    def __repr__(self):
        return f"LabelPath({self._repr!r})"

    def __eq__(self, obj):
        return isinstance(obj, LabelPath) and self._repr == obj._repr

    def __hash__(self):
        return self._repr.__hash__()

    def __truediv__(self, obj: Union[str, LabelPath]) -> LabelPath:
        return self.join(obj)


_ABSOLUTE_WORKSPACE_REGEX = re.compile(r"^@([^/]+)//")
