import sys
from pathlib import Path

from cealn.rule import Rule
from cealn.attribute import Attribute, GlobalProviderAttribute, FileAttribute
from cealn.exec import Executable
from cealn.depmap import DepmapBuilder

from ..providers import PythonToolchain, PythonVirtualEnv


class PoetryProject(Rule):
    pyproject_toml = FileAttribute(default="pyproject.toml")
    poetry_lock = FileAttribute(default="poetry.lock")
    poetry_toml = FileAttribute(default="poetry.toml")

    python_toolchain = GlobalProviderAttribute(PythonToolchain, host=True)

    async def analyze(self):
        # FIXME: lock all dependencies
        poetry_sdk = self.run(self.python_toolchain.python, "-m", "pip", "install", "poetry", "--target=%[srcdir]")

        install_input = self.new_depmap()
        install_input[".poetry-sdk"] = poetry_sdk.files
        install_input["pyproject.toml"] = self.pyproject_toml
        install_input["poetry.lock"] = self.poetry_lock
        install_input["poetry.toml"] = self.poetry_toml
        install_input = install_input.build()

        venv_install = self.run(
            self.python_toolchain.python,
            "-m",
            "poetry",
            "install",
            "--sync",
            append_env={"PYTHONPATH": "%[srcdir]/.poetry-sdk"},
            input=install_input,
        )

        # FIXME: this is only necessary because depmap merge is broken, fix it and get rid of this
        venv_install_filtered = self.run("/bin/bash", "-c", "cp -r .venv/* .", input=venv_install.files)

        venv_project = self.new_depmap(id="venv")
        venv_project.merge(venv_install_filtered.files)
        venv_project.merge(self.python_toolchain.python.context)
        venv_project["bin/python"] = DepmapBuilder.symlink("./python3")
        # FIXME: don't use host
        venv_project["pyvenv.cfg"] = DepmapBuilder.file(
            "home = /usr/bin\n"
            "implementation = CPython\n"
            "include-system-site-packages=false\n"
            "base-prefix = /usr\n"
            "base-exec-prefix = /usr\n"
            "base-executable = /usr/bin/python3\n"
        )
        venv_project = venv_project.build()

        return [PythonVirtualEnv(files=venv_project)]
