from cealn.exec import LinuxExecutePlatform
from .config import Option


class Os(Option):
    platform_provider_type = None


class Linux(Os):
    platform_provider_type = LinuxExecutePlatform


class Windows(Os):
    pass


class Wasi(Os):
    pass


class UnknownOs(Os):
    pass


class Arch(Option):
    pass


class X86_64(Arch):
    pass


class Aarch64(Arch):
    pass


class Wasm32(Arch):
    pass


class Cuda(Os):
    pass


class NvPtx(Arch):
    pass


class Vendor(Option):
    pass


class UnknownVendor(Vendor):
    pass


class Pc(Vendor):
    pass


class Uwp(Vendor):
    pass
