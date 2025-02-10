from __future__ import annotations

from typing import Dict, List, Optional

from .provider import Provider, Field
from .label import Label


class Executable(Provider):
    name = Field(Optional[str])

    context = Field(Label)
    executable_path = Field(str)

    search_paths = Field(List[str], default=[])
    library_search_paths = Field(List[str], default=[])

    def add_dependency_executable(self, rule, *executables, context_id=None) -> Executable:
        search_paths = [*self.search_paths]
        library_search_paths = [*self.library_search_paths]
        new_context = rule.new_depmap(id=context_id)
        if self.context:
            new_context.merge(self.context)
        for executable in executables:
            new_context.merge(executable.context)
            search_paths += executable.search_paths
            library_search_paths += executable.library_search_paths
        new_context = new_context.build()

        return Executable(
            context=new_context,
            executable_path=self.executable_path,
            search_paths=search_paths,
            library_search_paths=library_search_paths,
        )


class LinuxExecutePlatform(Provider):
    execution_sysroot = Field(Label)
    execution_sysroot_input_dest = Field(str)
    execution_sysroot_output_dest = Field(str)
    execution_sysroot_exec_context_dest = Field(str)

    uid = Field(int)
    gid = Field(int)

    standard_environment_variables = Field(Dict[str, str])

    use_fuse = Field(bool, default=True)
    use_interceptor = Field(bool, default=True)

    def substitute(self, value: str):
        return value.replace("%[srcdir]", self.execution_sysroot_input_dest).replace(
            "%[execdir]", self.execution_sysroot_exec_context_dest
        )


class MacOSExecutePlatform(Provider):
    execution_sysroot_extra = Field(Label)
