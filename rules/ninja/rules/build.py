from __future__ import annotations
from itertools import chain
import re
from typing import List, Optional
import hashlib
import shlex
import time

from cealn.depmap import DepmapBuilder
from cealn.rule import Rule
from cealn.attribute import Attribute, GlobSetAttribute, ProviderAttribute
from cealn.exec import Executable
from cealn.label import LabelPath

from ..providers.input import NinjaInput


class NinjaBuild(Rule):
    input = ProviderAttribute(NinjaInput)

    """
    A set of file glob patterns
    """
    intransitive_input_patterns = GlobSetAttribute(default=["*.o", "*.obj"])

    async def analyze(self):
        self.created_output_dirs = set()

        with await self.open_file(self.input.input / self.input.build_root / "build.ninja", encoding="utf-8") as f:
            print(f"analyzing {LabelPath(self.input.build_root) / 'build.ninja'}")
            await self.handle_file(self.input.build_root, f)

    async def handle_file(self, containing_dir, f, *, parent_context=None, include_context=None):
        if include_context is not None:
            context = include_context
        else:
            context = _Context(parent_context, containing_dir)
            context.add_rule(_NinjaRule("phony", context))
        current_rule = None
        current_build = None
        accum_line = None
        for line in f:
            line = line.rstrip()
            line = _COMMENT_REGEX.sub("", line)
            if not line:
                continue

            # Handle continuation lines
            if line.endswith("$"):
                if accum_line is None:
                    accum_line = line[:-1]
                else:
                    accum_line += line[:-1]
                continue
            elif accum_line is not None:
                # End of continuation lines
                line = accum_line + line
                accum_line = None

            if current_rule is not None:
                if m := _VARIABLE_DECLRATION_REGEX.fullmatch(line):
                    if not m.group("indent"):
                        context.add_rule(current_rule)
                        current_rule = None
                    else:
                        current_rule[m.group("name")] = m.group("value") or ""
                else:
                    context.add_rule(current_rule)
                    current_rule = None

            if current_build is not None:
                if m := _VARIABLE_DECLRATION_REGEX.fullmatch(line):
                    if not m.group("indent"):
                        current_build.emit(self)
                        current_build = None
                    else:
                        current_build[m.group("name")] = m.group("value") or ""
                else:
                    current_build.emit(self)
                    current_build = None

            if current_rule is None and current_build is None:
                if m := _VARIABLE_DECLRATION_REGEX.fullmatch(line):
                    context[m.group("name")] = m.group("value") or ""
                elif m := _BUILD_EDGE_REGEX.fullmatch(line):
                    rulename = m.group("rulename")
                    current_build = _Build(context.get_rule(rulename), context, m.group("outputs"), m.group("inputs"))
                elif m := _RULE_DECLARATION_REGEX.fullmatch(line):
                    current_rule = _NinjaRule(name=m.group("name"), context=context)
                elif m := _POOL_DECLARATION_REGEX.fullmatch(line):
                    # We don't care about ninja pools
                    pass
                elif m := _DEFAULT_DECLARATION_REGEX.fullmatch(line):
                    # We don't care about default targets
                    pass
                elif m := _SUBNINJA_REGEX.fullmatch(line):
                    filename = m.group("filename")
                    with await self.open_file(
                        self.input.input / self.input.build_root / filename, encoding="utf-8"
                    ) as f:
                        print(f"analyzing {LabelPath(self.input.build_root) / filename}")
                        await self.handle_file(
                            str(LabelPath(self.input.build_root) / LabelPath(filename).parent),
                            f,
                            parent_context=context,
                        )
                elif m := _INCLUDE_REGEX.fullmatch(line):
                    filename = m.group("filename")
                    with await self.open_file(
                        self.input.input / self.input.build_root / filename, encoding="utf-8"
                    ) as f:
                        print(f"analyzing {LabelPath(self.input.build_root) / filename}")
                        await self.handle_file(
                            str(LabelPath(self.input.build_root) / LabelPath(filename).parent),
                            f,
                            include_context=context,
                        )
                else:
                    raise RuntimeError(f"failed to parse ninja line: {line!r}")

        if current_rule is not None:
            context.add_rule(current_rule)
        elif current_build is not None:
            current_build.emit(self)


_COMMENT_REGEX = re.compile(r"#.*$")

# FIXME: none of these handle escapes properly
_VARIABLE_DECLRATION_REGEX = re.compile(r"(?P<indent>\s+)?(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*=\s*(?P<value>.+)?")
_RULE_DECLARATION_REGEX = re.compile(r"rule\s+(?P<name>[A-Za-z_][A-Za-z0-9_-]*)")
_BUILD_EDGE_REGEX = re.compile(
    r"build(?P<outputs>(\s+(\$:|[^:\s])+)+)\s*:\s*(?P<rulename>[A-Za-z_][A-Za-z0-9_-]*)(?P<inputs>(\s+[^\s]+)*)"
)
_POOL_DECLARATION_REGEX = re.compile(r"pool\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)")
_DEFAULT_DECLARATION_REGEX = re.compile(r"default\s+(.+)")
_SUBNINJA_REGEX = re.compile(r"subninja\s+(?P<filename>.+)")
_INCLUDE_REGEX = re.compile(r"include\s+(?P<filename>.+)")
_SPACE_SEPARATOR_REGEX = re.compile(r"(?<!\$)\s+")
_VAR_REFERENCE_REGEX = re.compile(
    r"((?<!\$)\$\{(?P<name1>[A-Za-z_][A-Za-z0-9_]*)\}|(?<!\$)\$(?P<name2>[A-Za-z_][A-Za-z0-9_]*))"
)
_DOLLAR_SIGN_ESCAPE_REGEX = re.compile(r"\$(\$|:)")


class _VarContext:
    def __init__(self):
        self.vars = {}

    def __setitem__(self, k: str, v: str):
        self.vars[k] = v

    def __getitem__(self, k: str):
        try:
            return self.vars[k]
        except KeyError:
            for ctx in self.parent_var_contexts:
                if v := ctx.get_var(k):
                    return v
            raise RuntimeError(f"reference to undefined variable {k!r}")

    def get_var(self, k: str):
        try:
            return self.vars[k]
        except KeyError:
            for ctx in self.parent_var_contexts:
                if v := ctx.get_var(k):
                    return v
            return None

    def substitue(self, value: str) -> str:
        variable_substituted = _VAR_REFERENCE_REGEX.sub(
            lambda m: self._get_var_substitue(m.group("name1") or m.group("name2")), value
        )
        escape_substituted = _DOLLAR_SIGN_ESCAPE_REGEX.sub(r"\1", variable_substituted)
        return escape_substituted

    def _get_var_substitue(self, key: str) -> str:
        return self.substitue(self.get_var(key) or "")

    @property
    def parent_var_contexts(self) -> List[_VarContext]:
        return []


class _Context(_VarContext):
    def __init__(self, parent: Optional[_Context], containing_dir: str):
        super().__init__()
        self.parent = parent
        self.containing_dir = containing_dir
        self._own_rules = {}

    def add_rule(self, rule: Rule):
        self._own_rules[rule.name] = rule

    def get_rule(self, name: str):
        try:
            return self._own_rules[name]
        except KeyError:
            if self.parent is not None:
                return self.parent.get_rule(name)
            else:
                raise RuntimeError(f"reference to undefined rule {name!r}")

    @property
    def parent_var_contexts(self) -> List[_VarContext]:
        if self.parent is not None:
            return [self.parent]
        else:
            return []


class _NinjaRule(_VarContext):
    def __init__(self, name: str, context: _Context):
        super().__init__()
        self.name = name
        self.context = context

    @property
    def parent_var_contexts(self) -> List[_VarContext]:
        return [self.context]


class _Build(_VarContext):
    def __init__(self, ninja_rule: _NinjaRule, context: _Context, outputs: str, inputs: str):
        super().__init__()
        self.ninja_rule = ninja_rule
        self.context = context
        self.outputs = re.split(_SPACE_SEPARATOR_REGEX, outputs.strip())

        self.inputs = []
        self.implicit_inputs = []
        self.order_only_inputs = []
        input_mode = "normal"
        all_inputs = re.split(_SPACE_SEPARATOR_REGEX, inputs.strip())
        for input in all_inputs:
            if not input:
                continue
            if input == "||":
                input_mode = "order_only"
            elif input == "|":
                input_mode = "implicit"
            elif input_mode == "order_only":
                self.order_only_inputs.append(input)
            elif input_mode == "implicit":
                self.implicit_inputs.append(input)
            else:
                self.inputs.append(input)
        # FIXME: handle shell escaping
        self["in"] = " ".join(self.inputs)
        self["in_newline"] = "\n".join(self.inputs)
        self["out"] = " ".join(self.outputs)

    def emit(self, rule: Rule):
        if self.ninja_rule.name == "phony":
            input_depmap_id = self.outputs[0]
        else:
            input_depmap_id = None

        input_depmap = rule.new_depmap(input_depmap_id)
        built_input_depmap = rule.new_depmap()
        # FIXME: filter source files
        input_depmap.merge(rule.input.input)
        for input in chain(self.inputs, self.implicit_inputs, self.order_only_inputs):
            # FIXME: be smarter about how we detect whether somethign is a source file?
            # FIXME: generalize /src and /exec here
            if not input.startswith("..") and not input.startswith("/src/") and not input.startswith("/exec/"):
                specific_input_depmap = rule.label.join_action(_output_hash(input))
                input_depmap.merge(specific_input_depmap)
                if not rule.intransitive_input_patterns.match(input):
                    built_input_depmap.merge(specific_input_depmap)
        # Create directories for output files
        for output in self.outputs:
            output_dir = None

            # FIXME: generalize /src and /exec here
            try:
                output_relative_to_src = LabelPath(output).relative_to(LabelPath("/src"))
                output_dir = output_relative_to_src.parent
                if str(output_dir) == ".":
                    continue
            except ValueError:
                pass

            try:
                output_relative_to_exec = LabelPath(output).relative_to("/exec")
                # Don't do anything, must already exist
                continue
            except ValueError:
                pass

            if output_dir is None:
                output_dir = (LabelPath(rule.input.build_root) / LabelPath(output)).parent
            try:
                output_dir_build_relative = output_dir.relative_to(LabelPath(rule.input.build_root))
                # Sometimes rules depend on implicit output directories, handle this
                if output_dir_build_relative not in rule.created_output_dirs:
                    dummy_depmap = rule.new_depmap(id=_output_hash(str(output_dir_build_relative)))
                    dummy_depmap[output_dir] = DepmapBuilder.directory()
                    dummy_depmap = dummy_depmap.build()
                    rule.created_output_dirs.add(output_dir_build_relative)
            except ValueError:
                pass

            input_depmap[output_dir] = DepmapBuilder.directory()
        # Handle rsp files
        rspfile = self.get_var("rspfile")
        if rspfile is not None:
            rspfile = self.substitue(rspfile)
            rspfile_content = self.substitue(self["rspfile_content"])
            input_depmap[str(LabelPath(rule.input.build_root) / LabelPath(rspfile))] = DepmapBuilder.file(
                rspfile_content
            )

        input_depmap = input_depmap.build()
        built_input_depmap = built_input_depmap.build()

        if self.ninja_rule.name == "phony":
            for output in self.outputs:
                select_depmap = rule.new_depmap(id=_output_hash(output))
                select_depmap.merge(built_input_depmap)
                select_depmap.build()
            return

        orig_command = self.substitue(self["command"])
        command = shlex.split(orig_command)
        # Detect if the command needs a shell
        if "&&" in command or "||" in command:
            command = ["sh", "-c", orig_command]
        command_exe = command[0]

        description = self.get_var("description")
        if description is not None:
            description = self.substitue(description)

        hide_stdout = False
        if self.get_var("deps") == "msvc":
            # Command will print all header dependencies to stdout, supress this
            hide_stdout = True

        run = rule.run(
            Executable(
                executable_path=command_exe, context=rule.input.exec_context, search_paths=rule.input.search_paths
            ),
            *command[1:],
            input=input_depmap,
            cwd=f"%[srcdir]/{rule.input.build_root}",
            append_env=rule.input.append_env,
            hide_stdout=hide_stdout,
            mnemonic=self.ninja_rule.name,
            progress_message=description or " ".join(self.outputs),
        )

        for output in self.outputs:
            select_depmap = rule.new_depmap(id=_output_hash(output))
            # TODO: be smarter about what cumulative input files we include here, as it negatively impacts cacheability
            select_depmap.merge(built_input_depmap)
            select_depmap.merge(run.files)
            select_depmap.build()

    @property
    def parent_var_contexts(self) -> List[_VarContext]:
        return [self.context, self.ninja_rule]


def _output_hash(filename: str) -> str:
    return hashlib.sha256(filename.encode("utf-8")).hexdigest()[:16]
