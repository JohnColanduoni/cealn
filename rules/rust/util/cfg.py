from __future__ import annotations
from typing import List, Optional
import re
import json

from cealn.rule import Rule

from ..providers.rust_toolchain import RustToolchain


class CfgSet:
    def __init__(self):
        self._multimap = {}

    @classmethod
    async def enumerate(cls, rule: Rule, toolchain: RustToolchain, rustc_args: List[str]) -> CfgSet:
        cfgset = CfgSet()

        rustc_run = await rule.run(toolchain.rustc.executable, "--print", "cfg", *rustc_args, hide_stdout=True)
        with await rustc_run.open_stdout(encoding="utf-8") as f:
            for line in f:
                m = _CFG_ATOM_REGEX.fullmatch(line.strip())
                if not m:
                    raise RuntimeError(f"failed to parse rustc --print cfg output: {line!r}")
                value = m.group("value")
                if value is not None:
                    value = json.loads(value)
                cfgset.add(m.group("key"), value)

        return cfgset

    def evaluate(self, expr: str) -> bool:
        m = _CFG_REGEX.fullmatch(expr)
        if not m:
            raise RuntimeError(f"invalid cfg expression: {expr!r}")
        return self._evaluate_inner(m.group(1))

    def _evaluate_inner(self, expr: str) -> bool:
        if m := _CFG_ATOM_REGEX.fullmatch(expr):
            k = m.group("key")
            v = m.group("value")
            if v is not None:
                v = json.loads(v)
            return v in self._multimap.get(k, set())
        elif m := _CFG_NOT_REGEX.fullmatch(expr):
            return not self._evaluate_inner(m.group(1))
        elif m := _CFG_AGG_REGEX.fullmatch(expr):
            op = m.group("op")
            args_raw = m.group("args")
            args = []
            current_arg_start = 0
            paren_depth = 0
            for i in range(len(args_raw)):
                c = args_raw[i]
                if c == ",":
                    if paren_depth == 0:
                        args.append(args_raw[current_arg_start:i])
                        current_arg_start = i + 1
                elif c == "(":
                    paren_depth += 1
                elif c == ")":
                    paren_depth -= 1
            if current_arg_start < len(args_raw):
                args.append(args_raw[current_arg_start:])
            args = list(self._evaluate_inner(arg) for arg in args)

            if op == "all":
                return all(args)
            elif op == "any":
                return any(args)
            else:
                raise RuntimeError("unreachable")
        else:
            raise RuntimeError(f"TODO: cfg expression {expr!r}")

    def add(self, k: str, v: Optional[str] = None):
        if k in self._multimap:
            self._multimap[k].add(v)
        else:
            self._multimap[k] = set((v,))

    def as_env_vars(self):
        env_vars = {}
        for k, values in self._multimap.items():
            env_vars["CARGO_CFG_" + k.upper().replace("-", "_")] = ",".join(filter(lambda x: x is not None, values))
        return env_vars


_CFG_ATOM_REGEX = re.compile(r'^\s*(?P<key>[A-Za-z_][A-Za-z0-9-_]*)(\s*=\s*(?P<value>".*"))?\s*$')

_CFG_REGEX = re.compile(r"^cfg\((.+)\)$")

_CFG_AGG_REGEX = re.compile(r"^\s*(?P<op>all|any)\((?P<args>.*)\)\s*$")

_CFG_NOT_REGEX = re.compile(r"^\s*not\((.+)\)\s*$")
