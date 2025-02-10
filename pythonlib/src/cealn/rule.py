from __future__ import annotations

from inspect import Traceback
import inspect
from pathlib import Path
import shlex
from types import coroutine
from typing import Any, Awaitable, Counter, Dict, Optional, Type, TypeVar, Union, List
import os
from cealn.config import _JSON_OPTION_SENTINEL, Option
from cealn.platform import Os

from cealn.provider import Provider
from cealn.depmap import DepmapBuilder

from .label import Label, LabelPath
from ._reflect import get_caller_location
from .attribute import Attribute
from .action import Action, BuildDepmap, Download, DockerDownload, Extract, Run, GitClone, Transition
from ._json import _reference_object
from .exec import Executable, LinuxExecutePlatform, MacOSExecutePlatform


class RuleMeta(type):
    attributes: Dict[str, Attribute]

    def __init__(cls, name, bases, dct):
        super().__init__(name, bases, dct)

        cls.attributes = {}
        for k, v in dct.items():
            if not isinstance(v, Attribute):
                continue
            v.name = k
            cls.attributes[k] = v

    def __call__(cls, **kwds) -> RuleInvocation:
        from cealn.package import _add_rule_invocation_to_package, _get_package_optional

        name = kwds.pop("name", None)
        if not isinstance(name, str):
            raise TypeError('expected string value for "name"')

        output_mounts = kwds.pop("output_mounts", {})

        package = _get_package_optional()

        invocation_data = {}
        for k, v in kwds.items():
            try:
                attribute = cls.attributes[k]
            except KeyError:
                raise ValueError(f"unexpected attribute with name {k!r}")
            invocation_data[k] = attribute.coerce_source(v, package=package)
        for k, attribute in cls.attributes.items():
            if k not in invocation_data:
                if not attribute.is_optional:
                    raise ValueError(f"expected a value for attribute {k!r}")
                if attribute.has_default:
                    invocation_data[k] = attribute.coerce_source(attribute.default, package=package)

        # Get caller location
        instantiation_location = get_caller_location()

        invocation = RuleInvocation(
            cls,
            name=name,
            output_mounts=output_mounts,
            invocation_data=invocation_data,
            instantiation_location=instantiation_location,
        )

        _add_rule_invocation_to_package(invocation)

        return invocation


class Rule(metaclass=RuleMeta):
    name: str
    actions: list[Action]

    def __init__(self, name, label, attributes_input, build_config):
        self.name = name
        self.label = label
        self.attributes_input = attributes_input
        self.build_config = {}
        self.host_build_config = {}
        for [k, v] in build_config["options"]:
            self.build_config[Option.from_json({_JSON_OPTION_SENTINEL: k})] = Option.from_json(
                {_JSON_OPTION_SENTINEL: v}
            )
        for [k, v] in build_config["host_options"]:
            self.host_build_config[Option.from_json({_JSON_OPTION_SENTINEL: k})] = Option.from_json(
                {_JSON_OPTION_SENTINEL: v}
            )
        self.actions = []
        self.synthetic_targets = []

    def analyze(self):
        raise NotImplementedError('rules must implement an "analyze" method')

    async def build_file_contents(self, label: Label, *, encoding=None) -> Union[bytes, str]:
        """
        Execute the steps necessary to build the referenced file and return its contents
        """
        with await self.open_file(label, encoding=encoding) as f:
            return f.read()

    async def open_file(self, label: Label, *, encoding=None) -> Union[bytes, str]:
        if isinstance(label, str):
            label = Label(label)
        if not isinstance(label, Label):
            raise TypeError(f"invalid file reference {label!r} provided")

        # Ensure label is absolute
        label = self.label.package / label

        response = await AsyncRequestAwaiter(dict(type="label_open", label=label))
        if response["type"] == "file_handle":
            return open(response["fileno"], "r", encoding=encoding, buffering=128 * 1024)
        if response["type"] == "none":
            raise FileNotFoundError(f"no file with label {label}")
        else:
            raise RuntimeError("internal error: invalid response to async request")

    async def resolve_provider(self, provider_type: Type[Provider], target: Label, *, host=False) -> Provider:
        if isinstance(target, str):
            target = Label(target)

        providers = await self.load_providers(target, host=host)
        matching_providers = list(provider for provider in providers if isinstance(provider, provider_type))
        if not matching_providers:
            raise RuntimeError("no matching provider")
        elif len(matching_providers) > 1:
            raise RuntimeError("more than one provider matched the requested type")
        else:
            return matching_providers[0]

    async def load_providers(self, target: Label, *, host=False) -> List[Provider]:
        if isinstance(target, str):
            target = Label(target)

        if host:
            source_build_config = {
                "options": list(
                    (_reference_object(k), _reference_object(v)) for k, v in self.host_build_config.items()
                ),
                "host_options": list(
                    (_reference_object(k), _reference_object(v)) for k, v in self.host_build_config.items()
                ),
            }
        else:
            source_build_config = {
                "options": list((_reference_object(k), _reference_object(v)) for k, v in self.build_config.items()),
                "host_options": list(
                    (_reference_object(k), _reference_object(v)) for k, v in self.host_build_config.items()
                ),
            }

        response = await AsyncRequestAwaiter(
            dict(
                type="load_providers",
                target=target,
                build_config=source_build_config,
            )
        )

        if response["type"] == "providers":
            return response["providers"]
        else:
            raise RuntimeError("internal error: invalid response to async request")

    async def resolve_executable(self, target: Label, name: str, *, host=True) -> Executable:
        if isinstance(target, str):
            target = Label(target)

        providers = await self.load_providers(target, host=host)

        for provider in providers:
            if isinstance(provider, Executable) and provider.name == name:
                return provider
        raise RuntimeError("no matching executable")

    async def resolve_global_provider(self, provider: Type[Provider], host=False) -> Provider:
        if host:
            source_build_config = {
                "options": list(
                    (_reference_object(k), _reference_object(v)) for k, v in self.host_build_config.items()
                ),
                "host_options": list(
                    (_reference_object(k), _reference_object(v)) for k, v in self.host_build_config.items()
                ),
            }
        else:
            source_build_config = {
                "options": list((_reference_object(k), _reference_object(v)) for k, v in self.build_config.items()),
                "host_options": list(
                    (_reference_object(k), _reference_object(v)) for k, v in self.host_build_config.items()
                ),
            }

        response = await AsyncRequestAwaiter(
            dict(type="load_global_provider", provider=_reference_object(provider), build_config=source_build_config)
        )

        if response["type"] == "none":
            raise RuntimeError(f"failed to find global provider for {provider.__qualname__} in {provider.label}")
        elif response["type"] == "provider":
            return response["provider"]
        else:
            raise RuntimeError("internal error: invalid response to async request")

    async def file_exists(self, label: Label) -> bool:
        response = await AsyncRequestAwaiter(dict(type="file_exists", label=label))

        return response["value"]

    async def is_file(self, label: Label) -> bool:
        response = await AsyncRequestAwaiter(dict(type="is_file", label=label))

        return response["value"]

    async def target_exists(self, label: Label) -> bool:
        response = await AsyncRequestAwaiter(dict(type="target_exists", label=label))

        return response["value"]

    async def substitute_for_execution(self, value: str) -> str:
        # FIXME: handle other OS
        platform = await self.resolve_global_provider(LinuxExecutePlatform, host=True)
        return platform.substitute(value)

    # Action constructors
    def run(
        self,
        executable: Executable,
        *args: List[str],
        input: Optional[Label] = None,
        cwd: Optional[str] = None,
        append_env: Dict[str, str] = {},
        append_env_files: List[Label] = [],
        hide_stdout: bool = False,
        hide_stderr: bool = False,
        platform=None,
        structured_messages=None,
        id: Optional[str] = None,
        mnemonic: Optional[str] = None,
        progress_message: Optional[str] = None,
    ):
        if isinstance(executable, str):
            executable = Executable(executable_path=executable)
        if mnemonic is None:
            mnemonic = Path(executable.executable_path).name
        if progress_message is None:
            progress_message = " ".join(map(shlex.quote, map(str, args)))
        if isinstance(cwd, LabelPath):
            cwd = str(cwd)
        action = Run(
            executable=executable,
            args=list(args),
            input=input,
            cwd=cwd,
            platform=platform,
            hide_stdout=hide_stdout,
            hide_stderr=hide_stderr,
            structured_messages=structured_messages,
            append_env=append_env,
            append_env_files=append_env_files,
            id=id,
            mnemonic=mnemonic,
            progress_message=progress_message,
        )
        self._add_action(action)
        return action

    def script(self, input: Optional[Label] = None, append_env: Dict[str, str] = {}) -> Script:
        from .script import Script

        return Script(rule=self, input=input, append_env=append_env)

    def download(
        self,
        *urls: List[str],
        hash: Optional[str] = None,
        filename: Optional[Union[str, LabelPath]] = None,
        executable: bool = False,
        user_agent: str = "cealn",
        id: Optional[str] = None,
        mnemonic="Download",
        progress_message: Optional[str] = None,
    ):
        if filename is not None:
            filename = LabelPath(filename)
        if progress_message is None:
            progress_message = urls[0]
        action = Download(
            urls=urls,
            hash=hash,
            filename=filename,
            executable=executable,
            user_agent=user_agent,
            id=id,
            mnemonic=mnemonic,
            progress_message=progress_message,
        )
        self._add_action(action)
        return action

    def git_clone(
        self,
        url: str,
        revision: str,
        id: Optional[str] = None,
        mnemonic="GitClone",
        progress_message: Optional[str] = None,
    ):
        if progress_message is None:
            progress_message = f"{url}#{revision}"
        action = GitClone(url=url, revision=revision, id=id, mnemonic=mnemonic, progress_message=progress_message)
        self._add_action(action)
        return action

    def docker_download(
        self,
        image: str,
        architecture: str,
        *,
        id: Optional[str] = None,
        mnemonic: str = "DockerPull",
        progress_message: Optional[str] = None,
    ):
        if progress_message is None:
            progress_message = image
        action = DockerDownload(
            image=image, architecture=architecture, id=id, mnemonic=mnemonic, progress_message=progress_message
        )
        self._add_action(action)
        return action

    def extract(
        self,
        archive: Label,
        *,
        strip_prefix: Optional[Union[str, LabelPath]] = None,
        id: Optional[str] = None,
        mnemonic: str = "Extract",
        progress_message: Optional[str] = None,
    ):
        if isinstance(strip_prefix, str):
            strip_prefix = LabelPath(strip_prefix)
        if strip_prefix is not None and not isinstance(strip_prefix, LabelPath):
            raise TypeError("strip_prefix must be a LabelPath or string")
        if progress_message is None:
            progress_message = str(archive)
        action = Extract(
            archive=archive, strip_prefix=strip_prefix, id=id, mnemonic=mnemonic, progress_message=progress_message
        )
        self._add_action(action)
        return action

    def _add_action(self, action: Action):
        action.rule = self
        self.actions.append(action)

    def new_depmap(self, id: Optional[str] = None) -> DepmapBuilder:
        return DepmapBuilder(self, id=id)

    def _build_depmap(self, builder: DepmapBuilder):
        action = BuildDepmap(entries=builder._entries, id=builder.id, mnemonic="BuildDepmap", progress_message="")
        self._add_action(action)
        return action

    def transition(
        self,
        label: Label,
        *changed_options,
        host=False,
        id: Optional[str] = None,
        mnemonic="Transition",
        progress_message="",
    ) -> Transition:
        changed_options = list((option.__bases__[0], option) for option in changed_options)
        if host:
            changed_options = list((k, v) for k, v in self.host_build_config.items()) + changed_options
        action = Transition(
            label=label,
            # FIXME: handle nested inheritance of options
            changed_options=changed_options,
            id=id,
            mnemonic=mnemonic,
            progress_message=progress_message,
        )
        self._add_action(action)
        return action

    def synthetic_target(self, rule: Type[Rule], name: str, **kwargs) -> Label:
        self.synthetic_targets.append(rule(name=name, **kwargs))
        return self.label.join(name)

    async def gather(self, coroutines):
        if isinstance(coroutines, dict):
            results = await AsyncGroupAwaiter(list(coroutines.values()))
            output = {}
            for k, result in zip(coroutines.keys(), results):
                output[k] = result
            return output
        else:
            return await AsyncGroupAwaiter(coroutines)

    async def _run_analyze(self):
        import inspect

        attribute_tasks = {}
        for k, attribute in self.__class__.attributes.items():
            attribute_tasks[k] = self._resolve_attribute(k, attribute)
        self.attributes_resolved = await self.gather(attribute_tasks)

        result = self.analyze()

        # Allow `analyze` to be synchronous or asynchronous
        if inspect.iscoroutine(result):
            result = await result

        if result is not None:
            if not isinstance(result, list):
                raise RuntimeError("expected list of providers from analysis return value")
            for item in result:
                if not isinstance(item, Provider):
                    raise RuntimeError("expected list of providers from analysis return value")
        else:
            result = []

        for action in self.actions:
            await action.prepare(self)

        return {
            "actions": self.actions,
            "synthetic_targets": list(map(lambda x: x.to_json(), self.synthetic_targets)),
            "providers": result,
        }

    async def _resolve_attribute(self, k, attribute):
        if k in self.attributes_input:
            return await attribute.resolve_from_source(self.attributes_input[k], rule=self)
        else:
            # This should already have been checked when the rule was invoked
            assert attribute.is_optional
            return await attribute.resolve_from_source(Attribute.NotSet, rule=self)


class AsyncRequestAwaiter:
    def __init__(self, request):
        self.request = request

    def __await__(self):
        response = yield [self.request]
        return response


class AsyncGroupAwaiter:
    def __init__(self, coroutines):
        self.coroutines = coroutines

    def __await__(self):
        active_coroutines = [*self.coroutines]
        outputs = [None] * len(self.coroutines)
        response_mappings = []

        # Get initial requests
        requests = []
        for i, coroutine in enumerate(active_coroutines):
            if coroutine is None:
                continue
            try:
                coroutine_requests = coroutine.send(None)
            except StopIteration as ex:
                outputs[i] = ex.value
                active_coroutines[i] = None
            else:
                requests += coroutine_requests
                response_mappings += [i] * len(coroutine_requests)

        # Continue to solicit requests and handle responses
        while any(active_coroutines):
            response = yield requests
            requests = []
            response_dest_index = response_mappings.pop(0)
            try:
                coroutine_requests = active_coroutines[response_dest_index].send(response)
            except StopIteration as ex:
                outputs[response_dest_index] = ex.value
                active_coroutines[response_dest_index] = None
            else:
                requests += coroutine_requests
                response_mappings += [response_dest_index] * len(coroutine_requests)

        return outputs


class RuleInvocation:
    rule: RuleMeta
    name: str
    invocation_data: Dict[Any, Any]
    instantiation_location: Optional[Traceback]

    def __init__(
        self,
        rule: RuleMeta,
        *,
        name: str,
        output_mounts: Dict[str, str],
        invocation_data: Dict[Any, Any],
        instantiation_location: Optional[Traceback] = None,
    ):
        self.rule = rule
        self.name = name
        self.output_mounts = output_mounts
        self.invocation_data = invocation_data
        self.instantiation_location = instantiation_location

    def to_json(self):
        return {
            "name": self.name,
            "rule": _reference_object(self.rule),
            "attributes_input": self.invocation_data,
            "output_mounts": self.output_mounts,
        }


_CURRENT_RULE_INSTANCE = None
_CURRENT_RULE_TASK = None


def _prepare_rule(
    rule_file: str,
    class_name: str,
):
    import importlib
    from ._json import decode_json

    # FIXME: with probably breaks on a bunch of stuff
    rule_file = Path(rule_file)
    try:
        workspaces_dir_relative = rule_file.relative_to("/workspaces")
    except ValueError:
        raise RuntimeError("rule defined outside of workspace")
    workspace_name = workspaces_dir_relative.parts[0]
    workspace_path = workspaces_dir_relative.relative_to(workspace_name)
    module_path = (
        "workspaces." + workspace_name.replace(".", "_") + "." + str(workspace_path.with_suffix("")).replace("/", ".")
    )

    module = importlib.import_module(module_path)
    _rule_class = getattr(module, class_name)


def _start_rule(
    rule_file: str,
    class_name: str,
    target_name: str,
    target_label: str,
    attributes_json: str,
    build_config_json: str,
):
    global _CURRENT_RULE_INSTANCE
    global _CURRENT_RULE_TASK

    import importlib
    from ._json import decode_json

    # FIXME: with probably breaks on a bunch of stuff
    rule_file = Path(rule_file)
    try:
        workspaces_dir_relative = rule_file.relative_to("/workspaces")
    except ValueError:
        raise RuntimeError("rule defined outside of workspace")
    workspace_name = workspaces_dir_relative.parts[0]
    workspace_path = workspaces_dir_relative.relative_to(workspace_name)
    module_path = (
        "workspaces." + workspace_name.replace(".", "_") + "." + str(workspace_path.with_suffix("")).replace("/", ".")
    )

    module = importlib.import_module(module_path)
    rule_class = getattr(module, class_name)

    attributes_input = decode_json(attributes_json)
    build_config = decode_json(build_config_json)
    # We need to manually construct the rule instance becasue we overrode __call__
    rule_instance = rule_class.__new__(rule_class)
    rule_instance.__init__(target_name, Label(target_label), attributes_input, build_config)

    _CURRENT_RULE_INSTANCE = rule_instance
    _CURRENT_RULE_TASK = rule_instance._run_analyze()


def _poll_rule(event):
    global _CURRENT_RULE_TASK

    from ._json import encode_json, decode_json

    event = decode_json(event)
    if event["type"] == "first_poll":
        # First send into a coroutine must always be `None` so we can get our first request
        event = None

    try:
        requests = _CURRENT_RULE_TASK.send(event)
    except StopIteration as ex:
        return encode_json({"done": ex.value})
    else:
        return encode_json({"requests": requests})
