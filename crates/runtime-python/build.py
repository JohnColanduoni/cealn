# NOTE: supports host builds (i.e. not wasi) just to running workspace-wide check etc. works

import os
from pathlib import Path, PurePosixPath
import platform
import shutil
import tarfile
import subprocess
import shlex
import sys
import re
import multiprocessing
import time
import urllib.request
from tempfile import TemporaryFile

if platform.system() == "Windows":
    # We currently don't have a way to get this build to work on a Windows host, so we use WSL

    def wslpath(path):
        return subprocess.check_output(f"wsl.exe wslpath -a {shlex.quote(path)}").decode("utf-8").strip()

    script_path = wslpath(os.path.realpath(__file__))

    relay_env = ["OUT_DIR", "PROFILE", "TARGET", "HOST"]

    # FIXME: path escape
    linux_command = f"python3 {script_path}"
    for env_var in relay_env:
        value = os.environ[env_var]
        if re.match(r"^[A-Z]:\\", value):
            value = wslpath(value)
        # FIXME: dirty hack to keep things on Linux filesystem
        if env_var == "OUT_DIR":
            windows_out_dir_in_linux = value
            linux_out_dir = PurePosixPath("/tmp") / PurePosixPath(value).relative_to("/")
            value = str(linux_out_dir)
        # NOTE: shlex.quote will be for Windows, hopefully that doesn't break anything
        linux_command = f"{env_var}={shlex.quote(value)} {linux_command}"

    print(f"running in WSL: {linux_command}", flush=True)

    ret = subprocess.call(["bash.exe", "-c", linux_command])

    if ret != 0:
        print(f"WSL build command exited with {ret}")
        exit(1)

    # Copy output files back to windows
    try:
        shutil.rmtree(Path(os.environ["OUT_DIR"]) / "python_install")
    except FileNotFoundError:
        pass

    # NOTE: shlex.quote will be for Windows, hopefully that doesn't break anything
    linux_copy_command = f"cp -r {shlex.quote(str(linux_out_dir / 'python_install'))} {shlex.quote(str(PurePosixPath(windows_out_dir_in_linux) / 'python_install'))}"

    ret = subprocess.call(
        [
            "bash.exe",
            "-c",
            linux_copy_command,
        ]
    )

    if ret != 0:
        print(f"WSL copy command exited with {ret}")
        exit(1)

    exit(0)

SOURCE_URL = "https://www.python.org/ftp/python/3.11.3/Python-3.11.3.tar.xz"
WASI_SYSROOT_URL = "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-20/wasi-sysroot-20.0.tar.gz"


class Build:
    def __init__(self):
        self.out_dir = Path(os.environ["OUT_DIR"])

    @property
    def python_source_path(self) -> Path:
        return self.out_dir / "python_src"

    @property
    def python_source_stamp(self) -> Path:
        return self.python_source_path / "dl.stamp"

    @property
    def python_build_directory(self) -> Path:
        return self.out_dir / "python_build"

    @property
    def python_install_directory(self) -> Path:
        return self.out_dir / "python_install"

    @property
    def clang_runtime_directory(self) -> Path:
        return self.out_dir / "clang_runtime"

    @property
    def wasi_sysroot_path(self) -> Path:
        return self.out_dir / "wasi-sysroot"

    @property
    def debug_build(self) -> bool:
        return os.environ["PROFILE"] == "debug"

    @property
    def is_wasi(self) -> bool:
        return os.environ["TARGET"] == "wasm32-wasi"

    @property
    def libname(self) -> str:
        if self.debug_build:
            return "libpython3.11d.a"
        else:
            return "libpython3.11.a"

    def run(self):
        if self.is_wasi:
            self.download_wasi_sdk()
        self.download_python()

        self.configure()
        self.build()

    def build(self):
        time.sleep(1)

        # Regenerate makefile with Setup.local additions
        self.sh(
            "make",
            "-j",
            str(multiprocessing.cpu_count()),
            "CROSS_COMPILE=yes",
            "Modules/config.c",
            cwd=self.python_build_directory,
        )

        if self.is_wasi:
            # HACK: prevent libinstall from trying to build modules (no dynamic linking on wasi yet)
            with open(self.python_build_directory / "Makefile", "r", encoding="utf-8") as f:
                makefile_text = f.read()
            makefile_text = re.compile(r"^libinstall:.*?$", re.MULTILINE).sub("libinstall:", makefile_text)
            with open(self.python_build_directory / "Makefile", "w", encoding="utf-8") as f:
                f.write(makefile_text)

        self.sh(
            "make",
            "-j",
            str(multiprocessing.cpu_count()),
            "CROSS_COMPILE=yes",
            "inclinstall",
            "libinstall",
            self.libname,
            cwd=self.python_build_directory,
        )

        shutil.copyfile(
            self.python_build_directory / self.libname,
            self.python_install_directory / self.libname,
        )

    def configure(self):
        try:
            shutil.rmtree(self.python_build_directory)
        except FileNotFoundError:
            pass

        cflags = []
        extra_args = []
        append_env = {}

        if self.is_wasi:
            cflags += [
                "--target=wasm32-wasi",
                "-isystem",
                str(Path("stub_headers").resolve()),
                f"--sysroot={self.wasi_sysroot_path}",
                # Suppress an error in timemodule.c
                "-Wno-int-conversion",
                # Ensure WASI provides clock() and Python can use it
                "-D_WASI_EMULATED_PROCESS_CLOCKS",
                "-DHAVE_CLOCK",
                "-nodefaultlibs",
                "-Wl,-lc",
            ]
            extra_args += [
                "--host=wasm32-wasi",
                f"--build={os.environ['HOST']}",
            ]
            if platform.system() == "Darwin":
                append_env.update(
                    {
                        "CC": "/opt/homebrew/opt/llvm/bin/clang",
                        "AR": "/opt/homebrew/opt/llvm/bin/llvm-ar",
                        "READELF": "true",
                        "CONFIG_SITE": "./config.site",
                    }
                )
            else:
                llvm_ar = None
                for i in range(14, 20):
                    llvm_ar_candidate = f"llvm-ar-{i}"
                    if shutil.which(llvm_ar_candidate):
                        llvm_ar = llvm_ar_candidate
                        break

                if llvm_ar is None:
                    raise RuntimeError("failed to find llvm-ar")

                append_env.update(
                    {
                        "CC": "clang --target=wasm32-wasi",
                        "AR": llvm_ar,
                        "READELF": "true",
                        "CONFIG_SITE": "./config.site",
                    }
                )

        elif "apple" in os.environ["TARGET"]:
            cflags += [
                "-I",
                "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk/System/Library/Frameworks/Tk.framework/Headers",
            ]

        if self.debug_build:
            extra_args += ["--with-pydebug"]

        self.python_build_directory.mkdir(parents=True)

        if self.is_wasi:
            # Configure static modules
            print(f"cargo:rerun-if-changed=Setup.local", flush=True)
            (self.python_build_directory / "Modules").mkdir(parents=True)
            shutil.copyfile("Setup.local", self.python_build_directory / "Modules" / "Setup.local")

        print("cargo:rerun-if-changed=config.site", flush=True)
        shutil.copyfile("config.site", self.python_build_directory / "config.site")
        build_python = shutil.which("python3.11")
        self.sh(
            "../python_src/configure",
            f"CFLAGS={shlex.join(cflags)}",
            f"CPPFLAGS={shlex.join(cflags)}",
            f"--with-build-python={build_python}",
            # Not compatible with WASI
            "--without-pymalloc",
            # We don't need shared libraries
            "--disable-shared",
            # Cargo culted from pyiodide
            "--disable-ipv6",
            "--with-pkg-config=no",
            f"--prefix={self.python_install_directory}",
            *extra_args,
            append_env=append_env,
            cwd=self.python_build_directory,
        )

        if self.is_wasi:
            # Configure static modules
            print(f"cargo:rerun-if-changed=Setup.local", flush=True)
            shutil.copyfile("Setup.local", self.python_build_directory / "Modules" / "Setup.local")

    def sh(self, *args, append_env=None, cwd=None):
        env_base = {**os.environ, **(append_env or {})}

        bytes_args = []
        str_args = []
        for arg in args:
            str_args.append(str(arg))
            if isinstance(arg, Path):
                bytes_args.append(os.fsencode(arg))
            else:
                bytes_args.append(arg)
        print(shlex.join(str_args), flush=True)

        subprocess.check_call(
            bytes_args,
            env=env_base,
            cwd=cwd,
        )

    def download_python(self):
        patches_dir = Path("patches")
        print(f"cargo:rerun-if-changed={patches_dir}", flush=True)
        patch_files = list(sorted(patches_dir.glob("*.patch")))

        print(f"downloading python source from {SOURCE_URL}", flush=True)

        try:
            shutil.rmtree(self.python_source_path)
        except FileNotFoundError:
            pass
        try:
            shutil.rmtree(self.python_build_directory)
        except FileNotFoundError:
            pass

        archive_filename = self.out_dir / "python.tar.xz"
        try:
            archive_filename.unlink()
        except FileNotFoundError:
            pass
        urllib.request.urlretrieve(SOURCE_URL, archive_filename)
        with open(archive_filename, "rb") as archive_tempfile:
            with tarfile.open(fileobj=archive_tempfile, mode="r:xz") as archive:
                for member in archive:
                    # Strip containing folder
                    member.path = str(Path(*Path(member.path).parts[1:]))
                    archive.extract(member, self.python_source_path)

        if self.is_wasi:
            for patch_file in patch_files:
                print(f"cargo:rerun-if-changed={patch_file}", flush=True)
                with open(patch_file, "rb") as f:
                    patch_process = subprocess.Popen(
                        ["patch", "-p1"], cwd=self.python_source_path, stdin=subprocess.PIPE
                    )
                    patch_process.communicate(input=f.read())
                    if patch_process.wait() != 0:
                        raise RuntimeError("patch command failed")

            for file_name in ["config.sub", "config.guess"]:
                shutil.copyfile(file_name, self.python_source_path / file_name)

    def download_wasi_sdk(self):
        # FIXME: detect changes in URL
        if not self.wasi_sysroot_path.exists():
            with urllib.request.urlopen(WASI_SYSROOT_URL) as response:
                with tarfile.open(fileobj=response, mode="r|gz") as archive:
                    for member in archive:
                        # Strip containing folder
                        member.path = str(Path(*Path(member.path).parts[1:]))
                        archive.extract(member, self.wasi_sysroot_path)


if __name__ == "__main__":
    Build().run()
