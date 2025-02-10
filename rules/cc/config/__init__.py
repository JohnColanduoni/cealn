from __future__ import annotations

from cealn.platform import (
    Arch,
    Aarch64,
    Os,
    Linux,
    X86_64,
    NvPtx,
    Cuda,
    Wasm32,
    UnknownOs,
    Windows,
    Vendor,
    Pc,
    Uwp,
    UnknownVendor,
    Wasi,
)
from cealn.config import Option


def llvm_target_triple(build_config):
    arch = build_config[Arch]
    try:
        arch = _ARCH_MAPPINGS[arch]
    except KeyError as ex:
        raise ValueError("unknown architecture") from ex

    os = build_config[Os]
    try:
        os = _OS_MAPPINGS[os]
    except KeyError as ex:
        raise ValueError("unknown os") from ex

    if os not in _NO_VENDOR_OS:
        vendor = build_config[Vendor]
        try:
            vendor = _VENDOR_MAPPINGS[vendor]
        except KeyError as ex:
            raise ValueError("unknown os") from ex

        return f"{arch}-{vendor}-{os}"
    else:
        return f"{arch}-{os}"


_ARCH_MAPPINGS = {
    X86_64: "x86_64",
    Aarch64: "aarch64",
    NvPtx: "nvptx64",
    Wasm32: "wasm32",
}

_VENDOR_MAPPINGS = {
    Pc: "pc",
    Uwp: "uwp",
    UnknownVendor: "unknown",
}

_OS_MAPPINGS = {
    Linux: "linux-gnu",
    Windows: "windows-msvc",
    Wasi: "wasi",
    Cuda: "cuda",
    UnknownOs: "unknown",
}

_NO_VENDOR_OS = set(["wasi"])


class CrtLinkage(Option):
    pass


class CrtDynamic(CrtLinkage):
    pass


class CrtStatic(CrtLinkage):
    pass


CrtLinkage.default = CrtDynamic
