from pathlib import Path
import inspect
from typing import Generic, Optional, TypeVar

from cealn.label import Label
from cealn._json import _reference_label

_JSON_PROVIDER_SENTINEL = "$cealn_provider"

T = TypeVar("T")


class Field(Generic[T]):
    name: str
    has_default: bool
    default: Optional[T]
    ty: type

    def __init__(self, ty: type, **kwargs):
        self.ty = ty
        self.name = None
        if "default" in kwargs:
            self.has_default = True
            self.default = kwargs.pop("default")
        else:
            self.has_default = False
            self.default = None

        if kwargs:
            raise TypeError(f"unexpected arguments {', '.join(kwargs.keys())}")

    def __get__(self, obj, objtype=None):
        return obj._data[self.name]

    # FIXME: validation based on type


class ProviderMeta(type):
    def __init__(cls, name, bases, dct):
        super().__init__(name, bases, dct)

        # Gather fields and provide names
        cls.fields = {}
        for k, v in dct.items():
            if not isinstance(v, Field):
                continue
            v.name = k
            cls.fields[k] = v

    @property
    def label(cls) -> str:
        return _reference_label(cls)


class Provider(metaclass=ProviderMeta):
    def __init__(self, **kwargs):
        fields = self.__class__.fields
        self._data = kwargs

        for k in self._data:
            if k not in fields:
                raise TypeError(f"provider {self.__class__.__name__} has no field {k!r}")
        for k, field in fields.items():
            if k not in self._data:
                if field.has_default:
                    self._data[k] = field.default

    def to_json(self):
        return {
            _JSON_PROVIDER_SENTINEL: {
                "source_label": self.__class__.label,
                "qualname": self.__class__.__qualname__,
                "data": self._data,
            }
        }

    @classmethod
    def from_json(cls, data):
        import importlib.util
        import sys

        contents = data[_JSON_PROVIDER_SENTINEL]

        # Load containing module
        import_filename = contents["source_label"].to_source_file_path()
        module_name = contents["source_label"].to_python_module_name()
        if module_name in sys.modules:
            module = sys.modules[module_name]
        else:
            spec = importlib.util.spec_from_file_location(module_name, import_filename)
            module = importlib.util.module_from_spec(spec)
            sys.modules[module_name] = module
            spec.loader.exec_module(module)

        qualname = contents["qualname"]
        if "." in qualname:
            # FIXME
            raise RuntimeError("not implemented")
        clazz = getattr(module, qualname)
        return clazz(**contents["data"])

    def __repr__(self) -> str:
        s = f"{self.__class__.__qualname__}(\n"
        for k, v in self._data.items():
            s += f"  {k}={repr(v)},\n"
        s += ")"
        return s
