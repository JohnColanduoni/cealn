import json
import re
import os
from typing import Optional, List, Dict, Tuple
from collections.abc import Iterable
import urllib.parse
import hashlib
from ._json import encode_json

from cealn.label import Label

from .depmap import ConcreteDepmap
from .exec import Executable, LinuxExecutePlatform
from .config import Option

_JSON_ACTION_SENTINEL = "$cealn_action"

_CAMEL_TO_SNAKE_REGEX = re.compile(r"(?<!^)(?=[A-Z])")

_ACTION_DISCRIM_MAP = {}


class ActionMeta(type):
    def __init__(cls, name, bases, dct):
        super().__init__(name, bases, dct)

        # Default discriminator based on class name
        cls._json_discrim = _CAMEL_TO_SNAKE_REGEX.sub("_", name).lower()

        _ACTION_DISCRIM_MAP[cls._json_discrim] = cls


class Action(metaclass=ActionMeta):
    def __init__(self, *, id: Optional[str], mnemonic: str, progress_message: str, **kwargs):
        self._id = id
        self.mnemonic = mnemonic
        self.progress_message = progress_message

        self._files = None

        for var_name in self.__class__.__annotations__:
            setattr(self, var_name, kwargs[var_name])

    @property
    def id(self):
        if self._id is None:
            # We need to set this to handle recursive call in `_common_json`
            self._id = ""
            self._id = hashlib.sha256(encode_json(self.to_json()).encode("utf-8")).hexdigest()
        return self._id

    @property
    def files(self) -> Label:
        if self._files is None:
            self._files = self.rule.label.join_action(self.id)
        return self._files

    async def prepare(self, rule):
        pass

    def to_json(self):
        data = {
            _JSON_ACTION_SENTINEL: self.__class__._json_discrim,
            "id": self.id,
            "mnemonic": self.mnemonic,
            "progress_message": self.progress_message,
        }
        for var_name in self.__class__.__annotations__:
            data[var_name] = getattr(self, var_name)
        return data

    def __await__(self):
        yield from self.prepare(self.rule).__await__()
        for action in self.rule.actions:
            yield from action.prepare(self.rule).__await__()
        response = yield [dict(type="action_output", action=self, partial_actions=self.rule.actions)]
        return ActionOutput.from_json(response)

    @classmethod
    def from_json(cls, data):
        sentinel = data.pop(_JSON_ACTION_SENTINEL)
        target_class = _ACTION_DISCRIM_MAP[sentinel]
        return target_class(**data)


class ActionOutput:
    files: ConcreteDepmap

    def __init__(
        self, *, _files_depmap_ref: str, _stdout_content_ref: Optional[str], _stderr_content_ref: Optional[str]
    ):
        self.files = ConcreteDepmap(_ref_hash=_files_depmap_ref)
        self._stdout_content_ref = _stdout_content_ref
        self._stderr_content_ref = _stderr_content_ref

    async def open_stdout(self, *, encoding: Optional[str] = None):
        from .rule import AsyncRequestAwaiter

        result = await AsyncRequestAwaiter(dict(type="content_ref_open", hash=self._stdout_content_ref))
        return open(result["fileno"], "r", encoding=encoding, buffering=128 * 1024)

    @classmethod
    def from_json(cls, data):
        return ActionOutput(
            _files_depmap_ref=data["files"], _stdout_content_ref=data["stdout"], _stderr_content_ref=data["stderr"]
        )


class StructuredMessageConfig:
    def __init__(self):
        self.level_map = {}
        self.human_messages = []

    def to_json(self):
        return {
            "level_map": self.level_map,
            "human_messages": self.human_messages,
        }


class Run(Action):
    executable: Executable
    args: List[str]
    input: Label
    cwd: Optional[str]
    append_env: Dict[str, str]
    append_env_files: List[Label]
    platform: LinuxExecutePlatform
    hide_stdout: bool
    hide_stderr: bool
    structured_messages: Optional[StructuredMessageConfig]

    async def prepare(self, rule):
        from .platform import Os

        if self.platform is None:
            self.platform = await rule.resolve_global_provider(
                rule.host_build_config[Os].platform_provider_type, host=True
            )


class Download(Action):
    urls: List[str]
    hash: Optional[str]
    filename: Optional[str]
    executable: bool
    user_agent: str


class GitClone(Action):
    url: str
    revision: str


class DockerDownload(Action):
    image: str
    architecture: str


class Extract(Action):
    archive: Label
    strip_prefix: Optional[str]


class BuildDepmap(Action):
    entries: Dict[str, Label]


class Transition(Action):
    label: Label
    changed_options: List[Tuple[Option, Option]]

    def to_json(self):
        from ._json import _reference_object

        data = {
            _JSON_ACTION_SENTINEL: self.__class__._json_discrim,
            "id": self.id,
            "mnemonic": self.mnemonic,
            "progress_message": self.progress_message,
            "label": self.label,
            "changed_options": [(_reference_object(a), _reference_object(b)) for a, b in self.changed_options],
        }
        return data


class TemplateArgument:
    def __init__(self, template: str, source: Label):
        self.template = template
        self.source = source

    def to_json(self):
        return {
            _TEMPLATE_ARGUMENT_SENTINEL: self.template,
            "source": self.source,
        }


_TEMPLATE_ARGUMENT_SENTINEL = "$cealn_argument_source_templated"


class RespfileArgument:
    def __init__(self, template: str, source: Label):
        self.template = template
        self.source = source

    def to_json(self):
        return {
            _RESPFILE_ARGUMENT_SENTINEL: self.template,
            "source": self.source,
        }


_RESPFILE_ARGUMENT_SENTINEL = "$cealn_argument_source_respfile"
