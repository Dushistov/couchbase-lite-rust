#!/usr/bin/env python3
# For simplify migration between different CI

import os, time, sys
from pathlib import Path
from subprocess import check_call
from multiprocessing import cpu_count

def show_timing(function):
    def _wrapper(*args, **kwargs):
        start = time.time()
        ret = function(*args, **kwargs)
        elapsed = (time.time() - start)
        print("%s elapsed time: %f" % (function.__name__, elapsed))
        return ret
    return _wrapper

def mkdir_if_not_exists(dir_path: str) -> None:
    if not os.path.exists(dir_path):
        os.makedirs(dir_path)

def get_src_root_path(my_path: str) -> str:
    my_path = os.path.dirname(os.path.realpath(my_path))
    return my_path

@show_timing
def build_and_test_cpp_part(src_root: str) -> None:
    cmake_build_dir = os.path.join(src_root, "build-cmake")
    cmake_src_dir = os.path.join(src_root, "couchbase-lite-core-sys",
                                 "couchbase-lite-core")
    mkdir_if_not_exists(cmake_build_dir)
    print("Current path: %s" % os.environ["PATH"])
    check_call(["cmake", "-DCMAKE_BUILD_TYPE=RelWithDebInfo", cmake_src_dir],
               cwd = cmake_build_dir)
    check_call(["ls"], cwd = cmake_build_dir)
    check_call(["cmake", "--build", ".", "--", "-j%d" % (cpu_count() + 1)],
               cwd = cmake_build_dir)
    os.environ["LiteCoreTestsQuiet"] = "1"
    check_call(["./CppTests", "-r", "list"], cwd = os.path.join(cmake_build_dir, "LiteCore", "tests"))
    check_call(["./C4Tests", "-r", "list"], cwd = os.path.join(cmake_build_dir, "C", "tests"))


@show_timing
def build_and_test_rust_part(src_root: str) -> None:
    print("running tests in debug mode")
    check_call(["cargo", "test", "--all", "-vv"], cwd = src_root)
    print("running tests in release mode")
    check_call(["cargo", "test", "--all", "--release", "-vv"], cwd = src_root)
    check_call(["cargo", "build", "-p", "chat-demo"], cwd = src_root)

@show_timing
def build_and_test_rust_part_for_ios(src_root: str) -> None:
    print("build for iOS")
    check_call(["cargo", "build", "--target=aarch64-apple-ios", "-p", "chat-demo"], cwd = src_root)

@show_timing
def main() -> None:
    ci_dir = Path(get_src_root_path(sys.argv[0]))
    src_root = ci_dir.parent
    CPP_TESTS = "cpp"
    RUST_TESTS = "rust"
    RUST_IOS_TESTS = "rust-ios"
    tests = set([CPP_TESTS, RUST_TESTS])
    if len(sys.argv) >= 2:
        if sys.argv[1] == "--rust-only":
            tests = set([RUST_TESTS])
        elif sys.argv[1] == "--cpp-only":
            tests = set([CPP_TESTS])
        elif sys.argv[1] == "--rust-ios-only":
            tests = set([RUST_IOS_TESTS])

    if CPP_TESTS in tests:
        build_and_test_cpp_part(src_root)
    if RUST_TESTS in tests:
        build_and_test_rust_part(src_root)
    if RUST_IOS_TESTS in tests:
        build_and_test_rust_part_for_ios(src_root)

if __name__ == "__main__":
    main()
