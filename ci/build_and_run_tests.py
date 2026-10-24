#!/usr/bin/env python3
"""To simplify migration between different CI"""

import os
import sys
import time
from collections.abc import Callable
from functools import wraps
from multiprocessing import cpu_count
from pathlib import Path
from subprocess import check_call

CPP_TESTS = "cpp"
RUST_TESTS = "rust"
RUST_IOS_TESTS = "rust-ios"
VALGRIND_TESTS = "valgrind"
WITH_SERVER_TESTS = "with-server"
REUSE_CPP_ONLY_BUILD = "reuse-cpp-only-build"

TEST_SELECTIONS = {
    "--rust-only": RUST_TESTS,
    "--cpp-only": CPP_TESTS,
    "--rust-ios-only": RUST_IOS_TESTS,
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


@show_timing
def build_and_test_rust_part(src_root: Path, *, use_valgrind: bool) -> None:
    """Run Rust tests in debug and release mode, optionally under Valgrind."""
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


@show_timing
def run_tests_that_require_server(src_root: Path, *, use_valgrind: bool) -> None:
    """Run the ignored integration tests against the local Sync Gateway."""
    my_env = os.environ.copy()
    my_env["SG_PASS"] = "password"
    my_env["SG_USER"] = "sguser"
    my_env["SG_URL"] = "ws://localhost:4884/scratch-30/"
    for test_name in (
        "test_double_replicator_restart",
        "test_wrong_sync_packets_order",
    ):
        cmd = ["cargo"]
        if use_valgrind:
            cmd.append("valgrind")
        cmd.extend(["test", "--release"])
        cmd.extend(
            ["-p", "couchbase-lite", "-vv", test_name, "--", "--ignored", "--nocapture"]
        )
        check_call(cmd, cwd=src_root, env=my_env)


def selected_tests(options: list[str]) -> set[str]:
    """Apply test selection options in command-line order."""
    tests = {CPP_TESTS, RUST_TESTS}
    for option in options:
        if option == "--with-valigrind":
            tests.add(VALGRIND_TESTS)
        elif option in TEST_SELECTIONS:
            tests = {TEST_SELECTIONS[option]}
        else:
            message = f"Unknown option {option}"
            raise SystemExit(message)
    return tests


@show_timing
def main() -> None:
    ci_dir = Path(sys.argv[0]).resolve().parent
    src_root = ci_dir.parent
    tests = selected_tests(sys.argv[1:])

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
        build_and_test_rust_part(src_root, use_valgrind=VALGRIND_TESTS in tests)
    if RUST_IOS_TESTS in tests:
        build_and_test_rust_part_for_ios(src_root)
    if WITH_SERVER_TESTS in tests:
        run_tests_that_require_server(src_root, use_valgrind=VALGRIND_TESTS in tests)
    if REUSE_CPP_ONLY_BUILD in tests:
        cargo_check_sys_with_external_couchbase_lite(src_root)


if __name__ == "__main__":
    main()
