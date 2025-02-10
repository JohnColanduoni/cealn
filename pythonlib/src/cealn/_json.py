import inspect
import json
from pathlib import Path
from typing import Any
import site

from .label import Label, LabelPath

LABEL_SENTINEL = "$cealn_label"
LABEL_PATH_SENTINEL = "$cealn_label_path"
RULE_SENTINEL = "$cealn_target"


def encode_json(obj) -> str:
    return json.dumps(
        obj,
        sort_keys=True,
        # No extra spaces around separators
        separators=(",", ":"),
        cls=_CealnJSONEncoder,
    )


def decode_json(data: str) -> Any:
    return json.loads(data, object_hook=_decode_json_object)


class _CealnJSONEncoder(json.JSONEncoder):
    def default(self, obj):
        from .action import Action, TemplateArgument, RespfileArgument, StructuredMessageConfig
        from .provider import Provider
        from .glob import GlobSet
        from .config import Option, Selection

        if isinstance(obj, Label):
            return {LABEL_SENTINEL: str(obj)}
        elif isinstance(obj, LabelPath):
            return {LABEL_PATH_SENTINEL: str(obj)}
        elif isinstance(obj, Action):
            return self.default(obj.to_json())
        elif isinstance(obj, Provider):
            return self.default(obj.to_json())
        elif isinstance(obj, GlobSet):
            return obj.to_json()
        elif isinstance(obj, type) and issubclass(obj, Option):
            return obj.to_json()
        elif isinstance(obj, Selection):
            return obj.to_json()
        elif isinstance(obj, TemplateArgument):
            return self.default(obj.to_json())
        elif isinstance(obj, RespfileArgument):
            return self.default(obj.to_json())
        elif isinstance(obj, StructuredMessageConfig):
            return self.default(obj.to_json())
        elif isinstance(obj, dict):
            newobj = {}
            for k, v in obj.items():
                newobj[k] = self.default(v)
            return newobj
        elif isinstance(obj, (list, tuple)):
            return [self.default(item) for item in obj]
        elif isinstance(obj, (str, bool, int, float, type(None))):
            return obj
        else:
            json.JSONEncoder.default(self, obj)


def _decode_json_object(obj):
    from .action import _JSON_ACTION_SENTINEL, Action
    from .provider import _JSON_PROVIDER_SENTINEL, Provider
    from .glob import _JSON_GLOBSET_SENTINEL, GlobSet
    from .config import _JSON_OPTION_SENTINEL, _JSON_SELECTION_SENTINEL, Option, Selection

    if LABEL_SENTINEL in obj:
        return Label(obj[LABEL_SENTINEL])
    elif LABEL_PATH_SENTINEL in obj:
        return LabelPath(obj[LABEL_PATH_SENTINEL])
    elif _JSON_ACTION_SENTINEL in obj:
        return Action.from_json(obj)
    elif _JSON_PROVIDER_SENTINEL in obj:
        return Provider.from_json(obj)
    elif _JSON_GLOBSET_SENTINEL in obj:
        return GlobSet.from_json(obj)
    elif _JSON_OPTION_SENTINEL in obj:
        return Option.from_json(obj)
    elif _JSON_SELECTION_SENTINEL in obj:
        return Selection.from_json(obj)
    else:
        return obj


def _reference_label(obj):
    file = Path(inspect.getabsfile(obj))
    # FIXME: with probably breaks on a bunch of stuff
    try:
        workspaces_dir_relative = file.relative_to("/workspaces")
    except ValueError:
        try:
            builtin_dir_relative = file.relative_to(Path(site.getsitepackages()[0]) / "cealn")
            return Label(f"@com.cealn.builtin//:{builtin_dir_relative}")
        except ValueError:
            raise RuntimeError(f"rule defined outside of workspace: {file!r}")
    workspace_name = workspaces_dir_relative.parts[0]
    workspace_path = workspaces_dir_relative.relative_to(workspace_name)
    # Find containing package
    package_subpath = Path(".")
    for subpath in list(file.parents)[:-1]:
        if (subpath / "build.cealn").exists():
            package_subpath = subpath.relative_to(Path("/workspaces") / workspace_name)
            break
    if str(package_subpath) == ".":
        package_subpath = ""
    return Label(f"@{workspace_name}//") / f"{package_subpath}:{workspace_path.relative_to(package_subpath)}"


def _reference_object(obj):
    return {"source_label": _reference_label(obj), "qualname": obj.__qualname__}
