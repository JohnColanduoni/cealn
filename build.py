#! /usr/bin/env python3

import platform
import subprocess
import shlex
import sys
import os
from pathlib import Path
import argparse

SRC_ROOT = Path(__file__).resolve().parent
TARGET_DIR = SRC_ROOT / "target"


def sh(*args, append_env={}):
    print(">", shlex.join(args), file=sys.stderr)
    ret = subprocess.call(args, env={**os.environ, **append_env})
    if ret != 0:
        raise RuntimeError("command failed")


def build(*, target_os=None, target_arch=None, profile="dev", locked=False):
    if target_os is None:
        py_system = platform.system()
        if py_system == "Linux":
            target_os = "linux"
        elif py_system == "Darwin":
            target_os = "macos"
        else:
            raise RuntimeError("unsupported OS")
    if target_arch is None:
        py_arch = platform.machine()
        if py_arch == "x86_64":
            target_arch = "x86_64"
        elif py_arch == "arm64":
            target_arch = "aarch64"
        else:
            raise RuntimeError("unsupported arch")

    if target_os == "linux":
        target_triple = f"{target_arch}-unknown-linux-musl"
    elif target_os == "macos":
        target_triple = f"{target_arch}-apple-darwin"
    else:
        raise RuntimeError("unsupported OS")

    profile_dirname = profile
    if profile == "dev":
        profile_dirname = "debug"

    extra_cargo_build_args = []
    if locked:
        extra_cargo_build_args += ["--locked"]

    if target_os == "macos":
        sh(
            "cargo",
            "build",
            "-Zunstable-options",
            "--profile",
            profile,
            "--target",
            f"{target_arch}-unknown-none",
            *extra_cargo_build_args,
            "-p",
            "cealn-action-executable-macos-guest",
            "--bin",
            "guest",
            append_env={"CARGO_TARGET_DIR": str(TARGET_DIR / "guest-target")},
        )
    elif target_os == "linux":
        sh(
            "cargo",
            "build",
            "-Zunstable-options",
            "--profile",
            profile,
            "--target",
            f"{target_arch}-unknown-linux-gnu",
            *extra_cargo_build_args,
            "-p",
            "cealn-action-executable-linux-interceptor",
            "--lib",
            append_env={"CARGO_TARGET_DIR": str(TARGET_DIR / "interceptor-target")},
        )

    sh(
        "cargo",
        "build",
        "-Zunstable-options",
        "--profile",
        profile,
        "--target",
        "wasm32-wasip1",
        *extra_cargo_build_args,
        "-p",
        "cealn-runtime-python",
        "--bin",
        "runtime-python",
        append_env={"CARGO_TARGET_DIR": str(TARGET_DIR / "runtime-target")},
    )

    output_target_args = []
    if target_os is not None or target_arch is not None:
        output_target_args += ["--target", target_triple]

    sh(
        "cargo",
        "build",
        "-Zunstable-options",
        *output_target_args,
        "--profile",
        profile,
        *extra_cargo_build_args,
        "-p",
        "cealn-driver",
        "--bin",
        "cealn",
        append_env={
            "CEALN_RUNTIME_PYTHON_PREBUILT": str(
                SRC_ROOT
                / "target"
                / "runtime-target"
                / "wasm32-wasi"
                / profile_dirname
                / "runtime-python.wasm"
            ),
            "CEALN_RUNTIME_PYTHON_STDLIB": str(
                SRC_ROOT
                / "target"
                / "runtime-target"
                / "wasm32-wasi"
                / profile_dirname
                / "python_libs"
            ),
            "CEALN_EXECUTE_GUEST": str(
                SRC_ROOT
                / "target"
                / "guest-target"
                / f"{target_arch}-unknown-none"
                / profile_dirname
                / "guest"
            ),
            "CEALN_EXECUTE_INTERCEPTOR": str(
                SRC_ROOT
                / "target"
                / "interceptor-target"
                / f"{target_arch}-unknown-linux-gnu"
                / profile_dirname
                / "libcealn_interceptor.so"
            ),
        },
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--locked", action=argparse.BooleanOptionalAction)

    args = parser.parse_args()

    try:
        build(locked=args.locked)
    except RuntimeError as ex:
        print(str(ex), file=sys.stderr)
        sys.exit(1)
