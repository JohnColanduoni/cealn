from pathlib import Path

from cealn.rule import Rule
from cealn.attribute import Attribute, FileAttribute, GlobalProviderAttribute
from cealn.exec import Executable

from ..providers.cc_toolchain import LLVMToolchain


class CCExecutable(Rule):
    # FIXME: swap this with CCToolchain, handle inheritance properly
    toolchain = GlobalProviderAttribute(LLVMToolchain, host=True)

    sources = FileAttribute()

    async def analyze(self):
        link = self.run(self.toolchain.clang, "-o", "test", self.sources, input=self.sources)
