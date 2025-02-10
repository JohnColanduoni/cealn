from __future__ import annotations

from typing import TYPE_CHECKING, Any, Dict, Generic, List, Optional, Type, TypeVar, Union, get_type_hints
import types
from cealn.glob import GlobSet

from cealn.label import Label
from cealn.provider import Provider

if TYPE_CHECKING:
    from cealn.rule import Rule

T = TypeVar("T")


class Attribute(Generic[T]):
    name: str
    has_default: bool
    default: Optional[T]

    NotSet = object()

    def __init__(self, **kwargs):
        self.name = None
        if "default" in kwargs:
            self.has_default = True
            self.default = kwargs.pop("default")
        else:
            self.has_default = False
            self.default = None

        if kwargs:
            raise TypeError(f"unexpected arguments {', '.join(kwargs.keys())}")

    def coerce_source(self, source, *, package=None):
        """
        Validates the value for the attribute passed in at load time and performs any necessary conversion
        """

        if Attribute.is_unresolved_value(source):
            # Validation is generally not possible for unresolved values
            return source

        # By default, perform value coercion eagerly to provide move proximate error messages
        return self.coerce_value(source)

    def coerce_value(self, value):
        """
        Validates the value for the attribute that has been resolved and performs any necessary conversion
        """
        return value

    async def resolve_from_source(self, source: Union[T, Attribute.NotSet], *, rule: Rule):
        from .config import Selection

        if Attribute.is_unresolved_value(source):
            if isinstance(source, Selection):
                source = await source.resolve(rule)
            elif isinstance(source, dict):
                new_value = {}
                for k, v in source.items():
                    k = await self.resolve_from_source(k, rule=rule)
                    v = await self.resolve_from_source(v, rule=rule)
                    new_value[k] = v
                source = new_value
            else:
                raise RuntimeError("unknown unresolved value type")

        if source is Attribute.NotSet:
            # This should already have been checked
            assert self.is_optional
            if self.has_default:
                source = self.default

        return await self.resolve_value(source, rule=rule)

    async def resolve_value(self, value: Union[T, Attribute.NotSet], *, rule: Rule):
        return self.coerce_value(value)

    def __get__(self, obj, objtype=None):
        return obj.attributes_resolved[self.name]

    @property
    def is_optional(self) -> bool:
        return self.has_default

    @classmethod
    def is_unresolved_value(self, value):
        from .config import Selection

        # FIXME: once we add generated values, check those here
        if isinstance(value, Selection):
            return True
        elif isinstance(value, dict):
            for k, v in value.items():
                if self.is_unresolved_value(k):
                    return True
                if self.is_unresolved_value(v):
                    return True
        return False


class LabelAttribute(Attribute[Label]):
    def coerce_source(self, source, *, package=None):
        if Attribute.is_unresolved_value(source):
            return source

        label = self._coerce_label(source)
        # Note that we only resolve labels relative to the package when they are resolved
        # at the time of the call. Resolving labels relative to the rule package when they are
        # generated values is likely to be confusing.
        if package:
            if label.is_package_relative and ":" not in str(label):
                # Since this is explicitly a file attribute, we default paths to point to files within the current package if not specified
                label = Label(":" + str(label))
            return package / label
        else:
            return label

    def _coerce_label(self, source):
        if isinstance(source, Label):
            return source
        elif isinstance(source, str):
            return Label(source)
        else:
            raise TypeError("expected label")


class FileAttribute(LabelAttribute):
    pass


class LabelListAttribute(Attribute[List[Label]]):
    def coerce_source(self, source, *, package=None):
        if Attribute.is_unresolved_value(source):
            return source

        coerced = []
        for v in source:
            label = self._coerce_label(v)
            # Note that we only resolve labels relative to the package when they are resolved
            # at the time of the call. Resolving labels relative to the rule package when they are
            # generated values is likely to be confusing.
            if package:
                if label.is_package_relative and ":" not in str(label):
                    # Since this is explicitly a file attribute, we default paths to point to files within the current package if not specified
                    label = Label(":" + str(label))
                label = package / label
            coerced.append(label)

        return coerced

    def _coerce_label(self, source):
        if isinstance(source, Label):
            return source
        elif isinstance(source, str):
            return Label(source)
        else:
            raise TypeError("expected label")


class LabelMapAttribute(Attribute[Dict[str, Label]]):
    def coerce_source(self, source, *, package=None):
        if Attribute.is_unresolved_value(source):
            return source

        coerced = {}
        for k, v in source.items():
            if isinstance(v, list):
                coerced[k] = [self._coerce_label(item, package) for item in v]
            else:
                coerced[k] = self._coerce_label(v, package)

        return coerced

    def _coerce_label(self, source, package=None):
        if isinstance(source, Label):
            label = source
        elif isinstance(source, str):
            label = Label(source)
        else:
            raise TypeError(f"expected label, but got {source!r}")
        if package:
            if label.is_package_relative and ":" not in str(label):
                # Since this is explicitly a file attribute, we default paths to point to files within the current package if not specified
                label = Label(":" + str(label))
            label = package / label
        # Note that we only resolve labels relative to the package when they are resolved
        # at the time of the call. Resolving labels relative to the rule package when they are
        # generated values is likely to be confusing.
        return label


class ProviderAttribute(LabelAttribute):
    def __init__(self, provider_type, *, host=False, **kwargs):
        super().__init__(**kwargs)

        self.provider_type = provider_type
        self.host = host

    async def resolve_from_source(self, source, *, rule: Rule):
        if Attribute.is_unresolved_value(source):
            # FIXME
            raise RuntimeError("TODO")

        if source is Attribute.NotSet:
            # This should already have been checked
            assert self.is_optional
            if self.has_default:
                source = self._coerce_label(self.default)

        return await rule.resolve_provider(self.provider_type, source, host=self.host)


class GlobalProviderAttribute(Attribute[Label]):
    """
    A provider which defaults to a globally registered default
    """

    def __init__(self, provider_type: Type[Provider], *, host=False, **kwargs):
        super().__init__(**kwargs)
        self.provider_type = provider_type
        self.host = host

    @property
    def is_optional(self) -> bool:
        return True

    async def resolve_value(self, value: Union[Label, Attribute.NotSet], *, rule: Rule):
        if not (value is Attribute.NotSet):
            return value

        return await rule.resolve_global_provider(self.provider_type, host=self.host)


class GlobSetAttribute(Attribute[GlobSet]):
    def coerce_value(self, value):
        if isinstance(value, GlobSet):
            return value
        if isinstance(value, list):
            return GlobSet(*value)
        else:
            raise TypeError("expected GlobSet or corresponding list")
