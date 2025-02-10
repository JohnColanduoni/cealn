from ._json import _reference_object


class OptionMeta(type):
    def to_json(cls):
        return {
            _JSON_OPTION_SENTINEL: _reference_object(cls),
        }


class Option(metaclass=OptionMeta):
    @classmethod
    def get_or_default(cls, build_config):
        return build_config.get(cls) or cls.default

    @classmethod
    def from_json(cls, data):
        import importlib.util
        import sys

        contents = data[_JSON_OPTION_SENTINEL]

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
        return clazz


_JSON_OPTION_SENTINEL = "$cealn_option"


class Selection:
    def __init__(self, mapping, default):
        self.mapping = mapping
        self.default = default

    async def resolve(self, rule):
        for k, v in self.mapping.items():
            # FIXME: handle more levels of inheritance
            if rule.build_config[k.__bases__[0]] == k:
                return v
        return self.default

    def to_json(self):
        return {
            _JSON_SELECTION_SENTINEL: {
                "mapping": list(self.mapping.items()),
                "default": self.default,
            }
        }

    @classmethod
    def from_json(cls, data):
        import importlib.util
        import sys

        contents = data[_JSON_SELECTION_SENTINEL]

        return Selection(dict(contents["mapping"]), contents["default"])


_JSON_SELECTION_SENTINEL = "$cealn_selection"


def select(mapping, default):
    return Selection(mapping, default)


class CompilationMode(Option):
    pass


class Fastbuild(CompilationMode):
    pass


class Optimized(CompilationMode):
    pass


class Debug(CompilationMode):
    pass
