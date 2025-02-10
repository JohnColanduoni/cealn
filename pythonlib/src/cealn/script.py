from typing import Dict, List, Optional
from cealn.exec import Executable
from cealn.label import Label
from cealn.rule import Rule


class Script:
    def __init__(self, *, rule: Rule, input: Optional[Label], append_env: Dict[str, str]):
        self.rule = rule
        self.input = input
        self.append_env = append_env

        self._current_input = self.input

    def run(
        self,
        executable: Executable,
        *args: List[str],
        append_env: Dict[str, str] = {},
        hide_stdout: bool = False,
        hide_stderr: bool = False,
        id: Optional[str] = None,
        mnemonic: Optional[str] = None,
        progress_message: Optional[str] = None,
    ):
        sub_action = self.rule.run(
            executable,
            *args,
            input=self._current_input,
            append_env={**self.append_env, **append_env},
            hide_stdout=hide_stdout,
            hide_stderr=hide_stderr,
            id=id,
            mnemonic=mnemonic,
            progress_message=progress_message,
        )

        new_input = self.rule.new_depmap()
        if self._current_input:
            new_input.merge(self._current_input)
        new_input.merge(sub_action.files)
        self._current_input = new_input.build()

        return sub_action

    @property
    def files(self) -> Label:
        return self._current_input
