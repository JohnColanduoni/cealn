from typing import Union
from ._json import encode_json
from .label import Label

_IS_WORKSPACE = False
_CONFIGURED_INFO = None


def configure(
    name: str,
):
    global _IS_WORKSPACE
    global _CONFIGURED_INFO

    if not _IS_WORKSPACE:
        raise RuntimeError("this is not a workspace file")
    if _CONFIGURED_INFO is not None:
        raise RuntimeError("workspace.config should only be called once")

    # Validate parameters so we provide useful errors
    if not isinstance(name, str):
        raise TypeError("name must be a str")

    _CONFIGURED_INFO = dict(
        name=name,
        local_workspaces=[],
        global_default_providers=[],
    )


def local_workspace(path: str):
    global _IS_WORKSPACE
    global _CONFIGURED_INFO
    if not _IS_WORKSPACE:
        raise RuntimeError("`local_workspace` can only be called from inside a workspace file")
    if _CONFIGURED_INFO is None:
        raise RuntimeError("`local_workspace` must be called after `config`")
    _CONFIGURED_INFO["local_workspaces"].append(dict(path=Label(path)))


def global_default_provider(provider_file: Union[str, Label], provider_qualname: str, target_label: Union[str, Label]):
    global _IS_WORKSPACE
    global _CONFIGURED_INFO
    if not _IS_WORKSPACE:
        raise RuntimeError("`global_default_provider` can only be called from inside a workspace file")
    if _CONFIGURED_INFO is None:
        raise RuntimeError("`global_default_provider` must be called after `config`")
    if isinstance(provider_file, str):
        provider_file = Label(provider_file)
    if isinstance(target_label, str):
        target_label = Label(target_label)
    if not isinstance(provider_file, Label):
        raise TypeError("`provider_file` must be a Label or string")
    if not isinstance(provider_qualname, str):
        raise TypeError("`provider_qualname` must be a string")
    if not isinstance(target_label, Label):
        raise TypeError("`target_label` must be a Label or string")

    # FIXME: handle workspace relative labels here?

    _CONFIGURED_INFO["global_default_providers"].append(
        dict(
            type="static",
            provider_type=dict(source_label=provider_file, qualname=provider_qualname),
            providing_target=target_label,
        )
    )


def _set_is_workspace():
    global _IS_WORKSPACE
    _IS_WORKSPACE = True


def _get_configured_info():
    global _CONFIGURED_INFO

    import json

    if _CONFIGURED_INFO is None:
        raise RuntimeError("the workspace file must call `workspace.configure`")

    return encode_json(_CONFIGURED_INFO)
