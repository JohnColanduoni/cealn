from __future__ import annotations

import os
from typing import Optional, Union, List
import fnmatch
import re

from cealn.label import Label, LabelPath


class DepmapBuilder:
    def __init__(self, rule, id: Optional[str]):
        self.rule = rule
        self.id = id

        self._entries = []

    def __setitem__(
        self, k: Union[LabelPath, str], value: Union[str, Label, DepmapBuilder.Directory, DepmapBuilder.FileLiteral]
    ):
        if isinstance(k, str):
            k = LabelPath(k)
        if not isinstance(k, LabelPath):
            raise TypeError("expected label path or string for key")
        k = k.normalize_require_descending()

        if isinstance(value, str):
            value = Label(value)
        if isinstance(value, Label):
            value = dict(reference=value)
        elif isinstance(value, DepmapBuilder.Directory):
            value = dict(directory={})
        elif isinstance(value, DepmapBuilder.FileLiteral):
            value = dict(file=dict(content=value.content, executable=value.executable))
        elif isinstance(value, DepmapBuilder.Symlink):
            value = dict(symlink=dict(target=value.target))
        elif isinstance(value, DepmapBuilder.Glob):
            value = dict(
                filter=dict(
                    base=value.base,
                    prefix=LabelPath(""),
                    patterns=[_translate_glob(pattern) for pattern in value.patterns],
                )
            )
        else:
            raise TypeError("invalid entry value for depmap builder")
        self._entries.append((k, value))

    def merge(self, value: Label):
        if isinstance(value, Label):
            value = dict(reference=value)
        elif isinstance(value, DepmapBuilder.Directory):
            value = dict(directory={})
        elif isinstance(value, DepmapBuilder.FileLiteral):
            value = dict(file=dict(content=value.content, executable=value.executable))
        elif isinstance(value, DepmapBuilder.Symlink):
            value = dict(symlink=dict(target=value.target))
        elif isinstance(value, DepmapBuilder.Glob):
            value = dict(
                filter=dict(
                    base=value.base,
                    prefix=LabelPath(""),
                    patterns=[_translate_glob(pattern) for pattern in value.patterns],
                )
            )
        else:
            raise TypeError("invalid entry value for depmap builder")
        self._entries.append((LabelPath(""), value))

    def build(self) -> Label:
        return self.rule._build_depmap(self).files

    @classmethod
    def directory(cls) -> Directory:
        return cls.Directory()

    @classmethod
    def file(cls, content: Union[str, bytes], *, executable: bool = False) -> FileLiteral:
        return cls.FileLiteral(content, executable=executable)

    @classmethod
    def symlink(cls, target: str) -> FileLiteral:
        return cls.Symlink(target)

    @classmethod
    def glob(cls, base: Label, *patterns) -> Glob:
        if isinstance(base, str):
            base = Label(base)
        if not isinstance(base, Label):
            raise TypeError("base must be a label")
        return cls.Glob(base, list(patterns))

    class Directory:
        pass

    class FileLiteral:
        def __init__(self, content: Union[str, bytes], *, executable: bool):
            self.content = content
            self.executable = executable

    class Symlink:
        def __init__(self, target: str):
            self.target = target

    class Glob:
        def __init__(self, base: Label, patterns: List[str]):
            self.base = base
            self.patterns = patterns


class ConcreteDepmap:
    def __init__(self, *, _ref_hash: str):
        self._ref_hash = _ref_hash

    async def get_file_contents(self, filename: str, *, encoding: Optional[str] = None):
        if not isinstance(filename, str):
            raise TypeError("filename must be a string")
        with await self.open_file(filename, encoding=encoding) as f:
            return f.read()

    async def open_file(self, filename: Union[str, LabelPath], *, encoding: Optional[str] = None):
        if isinstance(filename, str):
            filename = LabelPath(filename)
        if not isinstance(filename, LabelPath):
            raise TypeError("filename must be a LabelPath or string")

        from .rule import AsyncRequestAwaiter

        result = await AsyncRequestAwaiter(
            dict(type="concrete_depmap_file_open", depmap=self._ref_hash, filename=filename)
        )
        if result["type"] == "none":
            raise FileNotFoundError(f"depmap did not have file with name {filename!r}")
        return open(result["fileno"], "r", encoding=encoding, buffering=128 * 1024)

    async def iterdir(self, filename: Union[str, LabelPath]) -> List[LabelPath]:
        if isinstance(filename, str):
            filename = LabelPath(filename)
        if not isinstance(filename, LabelPath):
            raise TypeError("filename must be a LabelPath or string")

        from .rule import AsyncRequestAwaiter

        result = await AsyncRequestAwaiter(
            dict(type="concrete_depmap_directory_list", depmap=self._ref_hash, filename=filename)
        )
        if result["type"] == "none":
            raise FileNotFoundError(f"depmap did not have directory with name {filename!r}")
        return result["filenames"]


def _translate_glob(glob: str) -> str:
    if isinstance(glob, re.Pattern):
        return glob.pattern
    if m := re.fullmatch(r"\*([^\*]+)", glob):
        return f"{re.escape(m.group(1))}$"
    elif m := re.fullmatch(r"([^\*]+)\*", glob):
        return f"^{re.escape(m.group(1))}"
    else:
        # FIXME: support more kinds of patterns
        raise RuntimeError(f"TODO: glob {glob!r}")
