#!/usr/bin/env python3
"""To simplify migration between different CI"""

import json
import os
import shlex
import sys
import time
from collections.abc import Callable
from functools import wraps
from multiprocessing import cpu_count
from pathlib import Path
from subprocess import PIPE, Popen, check_call

CPP_TESTS = "cpp"
RUST_TESTS = "rust"
MIRI_TESTS = "miri"
RUST_IOS_TESTS = "rust-ios"
RUST_ANDROID_TESTS = "rust-android"
RUST_ANDROID_BUILD_ONLY = "rust-android-build-only"
VALGRIND_TESTS = "valgrind"
WITH_SERVER_TESTS = "with-server"
REUSE_CPP_ONLY_BUILD = "reuse-cpp-only-build"

TEST_SELECTIONS = {
    "--rust-only": RUST_TESTS,
    "--miri-only": MIRI_TESTS,
    "--cpp-only": CPP_TESTS,
    "--rust-ios-only": RUST_IOS_TESTS,
    "--rust-android-only": RUST_ANDROID_TESTS,
    "--rust-android-build-only": RUST_ANDROID_BUILD_ONLY,
    "--with-server-only": WITH_SERVER_TESTS,
    "--cargo-check-to-reuse-cpp-only-build": REUSE_CPP_ONLY_BUILD,
}


def show_timing[**P, R](function: Callable[P, R]) -> Callable[P, R]:
    @wraps(function)
    def _wrapper(*args: P.args, **kwargs: P.kwargs) -> R:
        start = time.time()
        ret = function(*args, **kwargs)
        elapsed = time.time() - start
        print(f"{function.__name__} elapsed time: {elapsed}")
        return ret

    return _wrapper


@show_timing
def build_and_test_cpp_part(src_root: Path) -> None:
    """Build and run the LiteCore C++ test binaries."""
    cmake_build_dir = src_root / "build-cmake"
    cmake_src_dir = os.environ["CORE_SRC"]
    cmake_build_dir.mkdir(exist_ok=True)
    print(f"Current path: {os.environ['PATH']}")
    check_call(
        [
            "cmake",
            "-DCMAKE_BUILD_TYPE=RelWithDebInfo",
            "-DCMAKE_POSITION_INDEPENDENT_CODE=On",
            cmake_src_dir,
        ],
        cwd=cmake_build_dir,
    )
    check_call(["ls"], cwd=cmake_build_dir)
    check_call(
        ["cmake", "--build", ".", "--", f"-j{cpu_count() + 1}"],
        cwd=cmake_build_dir,
    )
    os.environ["LiteCoreTestsQuiet"] = "1"
    check_call(["./CppTests", "-r", "list"], cwd=cmake_build_dir / "LiteCore" / "tests")
    check_call(["./C4Tests", "-r", "list"], cwd=cmake_build_dir / "C" / "tests")


@show_timing
def cargo_check_sys_with_external_couchbase_lite(src_root: Path) -> None:
    """Check the sys crate against the already built native library."""
    cmake_build_dir = src_root / "build-cmake"
    cmake_src_dir = os.environ["CORE_SRC"]
    my_env = os.environ.copy()
    my_env["COUCHBASE_LITE_CORE_SRC_DIR"] = cmake_src_dir
    my_env["COUCHBASE_LITE_CORE_BUILD_DIR"] = str(cmake_build_dir)
    syslib_src = src_root / "couchbase-lite-core-sys"
    check_call(
        [
            "cargo",
            "check",
            "--no-default-features",
            "--features=use-couchbase-lite-sqlite",
        ],
        cwd=syslib_src,
        env=my_env,
    )


def build_tests(cmd: list[str], src_root: Path) -> None:
    """Compile a test command without running its tests."""
    no_run_cmd = cmd.copy()
    no_run_cmd.append("--no-run")
    check_call(no_run_cmd, cwd=src_root)


def report_llvm_cov(src_root: Path) -> None:
    """Export release coverage accumulated by the Rust and server test runs"""
    check_call(
        ["cargo", "llvm-cov", "report", "--release", "--lcov", "--output-path",
         "target/llvm-cov/coverage.lcov"],
        cwd=src_root,
    )


@show_timing
def build_and_test_rust_part(
    src_root: Path, *, use_valgrind: bool, use_llvm_cov: bool
) -> None:
    """Run Rust tests in debug and release mode, optionally with coverage or Valgrind"""
    if use_llvm_cov:
        # Keep debug and release profiles separate: LLVM cannot merge coverage for
        # differently optimized versions of the same functions reliably.
        check_call(["cargo", "llvm-cov", "--no-report", "--workspace", "--tests"],
                   cwd=src_root)
        check_call(["cargo", "llvm-cov", "--workspace",
                    "--tests", "--release"], cwd=src_root)
        report_llvm_cov(src_root)
        check_call(["cargo", "build", "-p", "chat-demo"], cwd=src_root)
        return
    if not use_valgrind:
        print("running tests in debug mode")
        cmd = ["cargo", "test", "--all", "-vv"]
        build_tests(cmd, src_root)
        check_call(cmd, cwd=src_root)
    print(f"running release tests{' under Valgrind' if use_valgrind else ''}")
    cmd = ["cargo", "test", "--all", "-vv", "--release"]
    build_tests(cmd, src_root)
    if use_valgrind:
        cmd.insert(1, "valgrind")
    check_call(cmd, cwd=src_root)
    check_call(["cargo", "build", "-p", "chat-demo"], cwd=src_root)


@show_timing
def run_miri_tests(src_root: Path) -> None:
    """Interpret Rust-only FFI helper tests without building LiteCore."""
    env = os.environ.copy()
    env.setdefault("COUCHBASE_LITE_CORE_SRC_DIR", str(src_root / "couchbase-lite-core"))
    check_call(["cargo", "+nightly", "miri", "setup"], cwd=src_root, env=env)
    check_call(
        [
            "cargo",
            "+nightly",
            "miri",
            "test",
            "-p",
            "couchbase-lite-core-sys",
            "--lib",
            "--no-default-features",
        ],
        cwd=src_root,
        env=env,
    )


@show_timing
def build_and_test_rust_part_for_ios(src_root: Path) -> None:
    """Cross-compile the native libraries and chat demo for iOS."""
    print("build for iOS")
    # Because of https://github.com/alexcrichton/cmake-rs/issues/96 , cmake-rs can not
    # handle build for iOS, so time for manual build
    cpp_src = os.environ["CORE_SRC"]
    cpp_build_dir = Path(cpp_src) / "build-ios"
    cpp_build_dir.mkdir(exist_ok=True)
    cmake_conf_cmd = [
        "cmake",
        cpp_src,
        "-DCMAKE_OSX_ARCHITECTURES=arm64",
        "-DCMAKE_OSX_SYSROOT=iphoneos",
        "-DCMAKE_OSX_DEPLOYMENT_TARGET=12.0",
        "-DCMAKE_SYSTEM_NAME=iOS",
        "-DDISABLE_LTO_BUILD=True",
        "-DMAINTAINER_MODE=False",
        "-DENABLE_TESTING=False",
        "-DLITECORE_BUILD_TESTS=False",
        "-DSQLITE_ENABLE_RTREE=True",
        "-DCMAKE_C_FLAGS=-fPIC --target=aarch64-apple-ios",
        "-DCMAKE_C_COMPILER=/usr/bin/clang",
        "-DCMAKE_CXX_FLAGS=-fPIC --target=aarch64-apple-ios",
        "-DCMAKE_CXX_COMPILER=/usr/bin/clang++",
        "-DCMAKE_ASM_FLAGS=-fPIC --target=aarch64-apple-ios",
        "-DCMAKE_ASM_COMPILER=/usr/bin/clang",
        "-DCMAKE_BUILD_TYPE=Debug",
    ]
    print("configure build with cmake " + " ".join(cmake_conf_cmd))
    check_call(cmake_conf_cmd, cwd=cpp_build_dir)
    check_call(
        ["cmake", "--build", ".", "--", "LiteCoreStatic", "FleeceStatic", "BLIPStatic"],
        cwd=cpp_build_dir,
    )
    os.environ["COUCHBASE_LITE_CORE_BUILD_DIR"] = str(cpp_build_dir)
    os.environ["COUCHBASE_LITE_CORE_SRC_DIR"] = cpp_src
    check_call(
        [
            "cargo",
            "build",
            "-vv",
            "--no-default-features",
            "--features=use-couchbase-lite-sqlite,use-tokio-websocket",
            "--target=aarch64-apple-ios",
            "-p",
            "chat-demo",
        ],
        cwd=src_root,
    )


def android_test_executables(cmd: list[str], src_root: Path, env: dict[str, str]) -> list[Path]:
    """Build Rust tests and collect the binaries to run on an Android device."""
    executables = []
    with Popen(
        [*cmd, "--no-run", "--message-format=json"],
        cwd=src_root,
        env=env,
        stdout=PIPE,
        text=True,
    ) as process:
        assert process.stdout is not None
        for line in process.stdout:
            message = json.loads(line)
            if message["reason"] == "compiler-message":
                print(message["message"].get("rendered", ""), file=sys.stderr, end="")
            elif message["reason"] == "compiler-artifact":
                executable = message.get("executable")
                if executable and message["profile"]["test"]:
                    executables.append(Path(executable))
        if process.wait() != 0:
            raise RuntimeError(f"Android test build failed: {' '.join(cmd)}")
    if not executables:
        raise RuntimeError("Cargo produced no Android test executables")
    return list(dict.fromkeys(executables))


@show_timing
def build_and_test_rust_part_for_android(src_root: Path, *, run_on_device: bool) -> None:
    """Cross-compile Android native code and run x86_64 Rust tests on an emulator."""
    ndk = Path(os.environ["ANDROID_NDK_HOME"]).resolve()
    core_src = Path(os.environ["CORE_SRC"]).resolve()
    toolchain = ndk / "build/cmake/android.toolchain.cmake"
    llvm_bin = ndk / "toolchains/llvm/prebuilt/linux-x86_64/bin"
    if not toolchain.is_file():
        raise RuntimeError(f"Android NDK toolchain not found: {toolchain}")

    for target, abi in (
        ("x86_64-linux-android", "x86_64"),
        ("aarch64-linux-android", "arm64-v8a"),
    ):
        print(f"Building Android target {target}", flush=True)
        build_dir = src_root / "target/android-native" / abi
        build_dir.mkdir(parents=True, exist_ok=True)
        check_call(
            [
                "cmake", "-S", str(core_src), "-B", str(build_dir), "-G", "Ninja",
                f"-DCMAKE_TOOLCHAIN_FILE={toolchain}",
                "-DANDROID_PLATFORM=android-24", f"-DANDROID_ABI={abi}",
                "-DANDROID_STL=c++_shared", "-DCMAKE_BUILD_TYPE=Debug",
                "-DCMAKE_POLICY_VERSION_MINIMUM=3.5",
                "-DDISABLE_LTO_BUILD=True", "-DLITECORE_BUILD_TESTS=False",
                "-DENABLE_TESTING=False", "-DCOUCHBASE_LITE_RUST_WEBSOCKET=True",
                f"-DCMAKE_PROJECT_LiteCore_INCLUDE={src_root / 'couchbase-lite-core-sys/cmake/rust.cmake'}",
            ]
        )
        check_call(["cmake", "--build", str(build_dir), "--target", "CouchbaseLiteRust"])

        linker = llvm_bin / f"{target}24-clang"
        env = os.environ.copy()
        env["COUCHBASE_LITE_CORE_SRC_DIR"] = str(core_src)
        env["COUCHBASE_LITE_CORE_BUILD_DIR"] = str(build_dir)
        env[f"CARGO_TARGET_{target.upper().replace('-', '_')}_LINKER"] = str(linker)
        env[f"CC_{target.replace('-', '_')}"] = str(linker)
        env[f"CXX_{target.replace('-', '_')}"] = str(llvm_bin / f"{target}24-clang++")
        env[f"AR_{target.replace('-', '_')}"] = str(llvm_bin / "llvm-ar")

        cmd = [
            "cargo", "test", "--target", target, "--no-default-features",
            "-p", "couchbase-lite", "-p", "serde-fleece", "-p", "couchbase-lite-core-sys",
            "--features", (
                "couchbase-lite/use-couchbase-lite-sqlite,"
                "couchbase-lite/use-tokio-websocket,"
                "serde-fleece/use-couchbase-lite-sqlite,"
                "couchbase-lite-core-sys/use-couchbase-lite-sqlite"
            ),
        ]
        executables = android_test_executables(cmd, src_root, env)
        if target == "aarch64-linux-android":
            check_call(
                ["cargo", "build", "-p", "chat-demo", "--target", target,
                 "--no-default-features",
                 "--features=use-couchbase-lite-sqlite,use-couchbase-lite-websocket"],
                cwd=src_root,
                env=env,
            )
        elif run_on_device:
            device_dir = "/data/local/tmp/couchbase-lite-rust-tests"
            check_call(["adb", "shell", "mkdir", "-p", device_dir])
            cpp_shared = ndk / "toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib" \
                / target / "libc++_shared.so"
            check_call(["adb", "push", str(cpp_shared), device_dir])
            for executable in executables:
                check_call(["adb", "push", str(executable), device_dir])
                test_path = f"{device_dir}/{executable.name}"
                check_call(["adb", "shell", "chmod", "755", test_path])
                remote_cmd = (
                    f"cd {shlex.quote(device_dir)} && "
                    f"TMPDIR={shlex.quote(device_dir)} "
                    f"LD_LIBRARY_PATH={shlex.quote(device_dir)} "
                    f"./{shlex.quote(executable.name)} --test-threads=2"
                )
                check_call(["adb", "shell", remote_cmd])


@show_timing
def run_tests_that_require_server(
    src_root: Path, *, use_valgrind: bool, use_llvm_cov: bool
) -> None:
    """Run the ignored integration tests against the local Sync Gateway."""
    my_env = os.environ.copy()
    my_env["SG_PASS"] = "password"
    my_env["SG_USER"] = "sguser"
    my_env["SG_URL"] = "ws://localhost:4884/scratch-30/"
    for test_name in (
        "test_replication_round_trip",
        "test_double_replicator_restart",
        "test_wrong_sync_packets_order",
    ):
        if use_llvm_cov:
            # Match the Rust run's package selection so both runs use the same
            # instrumented binaries and LLVM can merge their profiles.
            cmd = ["cargo", "llvm-cov", "test", "--no-clean", "--release",
                   "--workspace", "--tests"]
        else:
            cmd = ["cargo"]
            if use_valgrind:
                cmd.append("valgrind")
            cmd.extend(["test", "--release"])
            cmd.extend(["-p", "couchbase-lite", "-vv"])
        cmd.extend([test_name, "--", "--ignored"])
        if not use_llvm_cov:
            cmd.append("--nocapture")
        check_call(cmd, cwd=src_root, env=my_env)
    if use_llvm_cov:
        report_llvm_cov(src_root)


def selected_tests(options: list[str]) -> tuple[set[str], bool]:
    """Apply test and coverage options in command-line order"""
    tests = {CPP_TESTS, RUST_TESTS}
    use_llvm_cov = False
    for option in options:
        if option == "--llvm-cov":
            use_llvm_cov = True
        elif option == "--no-llvm-cov":
            use_llvm_cov = False
        elif option == "--with-valigrind":
            tests.add(VALGRIND_TESTS)
        elif option in TEST_SELECTIONS:
            tests = {TEST_SELECTIONS[option]}
        else:
            message = f"Unknown option {option}"
            raise SystemExit(message)
    if use_llvm_cov and VALGRIND_TESTS in tests:
        raise SystemExit("--llvm-cov cannot be combined with --with-valigrind")
    return tests, use_llvm_cov


@show_timing
def main() -> None:
    ci_dir = Path(sys.argv[0]).resolve().parent
    src_root = ci_dir.parent
    if sys.argv[1:2] == ["--chat-stress"]:
        import chat_stress

        settings = chat_stress.settings_from_args(sys.argv[2:], src_root)
        core_src = src_root / "couchbase-lite-core"
        if core_src.is_dir():
            os.environ.setdefault("COUCHBASE_LITE_CORE_SRC_DIR", str(core_src))
        if not os.environ.get("COUCHBASE_LITE_CORE_SRC_DIR"):
            raise SystemExit("chat stress requires COUCHBASE_LITE_CORE_SRC_DIR")
        check_call(
            ["cargo", "build", "-p", "chat-demo", "--no-default-features",
             "--features=build-cpp,use-couchbase-lite-sqlite,use-tokio-websocket"],
            cwd=src_root,
        )
        chat_stress.run(settings, src_root)
        return
    tests, use_llvm_cov = selected_tests(sys.argv[1:])

    if VALGRIND_TESTS in tests:
        os.environ["VALGRINDFLAGS"] = " ".join(
            filter(
                None,
                [
                    os.environ.get("VALGRINDFLAGS", ""),
                    "--leak-check=full",
                    "--show-leak-kinds=definite,possible",
                    f"--suppressions={ci_dir / 'valgrind-suppressions.supp'}",
                ],
            )
        )

    if CPP_TESTS in tests:
        build_and_test_cpp_part(src_root)
    if RUST_TESTS in tests:
        build_and_test_rust_part(
            src_root, use_valgrind=VALGRIND_TESTS in tests, use_llvm_cov=use_llvm_cov
        )
    if MIRI_TESTS in tests:
        run_miri_tests(src_root)
    if RUST_IOS_TESTS in tests:
        build_and_test_rust_part_for_ios(src_root)
    if RUST_ANDROID_TESTS in tests or RUST_ANDROID_BUILD_ONLY in tests:
        build_and_test_rust_part_for_android(
            src_root, run_on_device=RUST_ANDROID_TESTS in tests
        )
    if WITH_SERVER_TESTS in tests:
        run_tests_that_require_server(
            src_root, use_valgrind=VALGRIND_TESTS in tests, use_llvm_cov=use_llvm_cov
        )
    if REUSE_CPP_ONLY_BUILD in tests:
        cargo_check_sys_with_external_couchbase_lite(src_root)


if __name__ == "__main__":
    main()
