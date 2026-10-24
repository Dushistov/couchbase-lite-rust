#!/usr/bin/env python3

import argparse
import time, sys, os, shutil
from pathlib import Path
from subprocess import check_call, Popen
from typing import List

def show_timing(function):
    def _wrapper(*args, **kwargs):
        start = time.time()
        ret = function(*args, **kwargs)
        elapsed = (time.time() - start)
        print("%s elapsed time: %f" % (function.__name__, elapsed))
        return ret
    return _wrapper

def get_src_root_path(my_path: str) -> str:
    my_path = os.path.dirname(os.path.realpath(my_path))
    return my_path

def path_to_docker_config(src_root: str) -> str:
    # TODO: use os.environ["CORE_SRC"]/Replicator/tests/data/docker,
    # when we switch to new version of couchbase-lite-core
    return os.path.join(src_root, "ci", "docker")

def parse_args(argv: List[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Manage the Couchbase Server and Sync Gateway test containers.")
    parser.add_argument("action", choices=("up", "down"), help="Start or stop the test containers")
    parser.add_argument("--engine", choices=("docker", "podman"), default="docker",
                        help="Container engine (default: docker)")
    sudo = parser.add_mutually_exclusive_group()
    sudo.add_argument("--sudo", dest="use_sudo", action="store_true", default=True,
                      help="Run Compose with sudo (default)")
    sudo.add_argument("--no-sudo", dest="use_sudo", action="store_false",
                      help="Run Compose without sudo")
    return parser.parse_args(argv)

def compose_command(engine: str, use_sudo: bool) -> List[str]:
    if not shutil.which(engine):
        raise RuntimeError(f"Container engine not found: {engine}")
    command = [engine, "compose"]
    # Set SSL inside sudo because sudo may discard the caller's environment.
    return ["sudo", "env", "SSL=false", *command] if use_sudo else command

def wait_success_process(cmd: List[str]) -> bool:
    for i in range(0, 120):
        process = Popen(cmd)
        process.wait()
        if process.returncode == 0:
            return True
        print("%d wait command 5 seconds more" % i)
        time.sleep(5)
    return False

@show_timing
def wait_couchbase_and_syncway_in_docker_up() -> None:
    if not wait_success_process(["curl", "-sIkL", "-H", "Authorization: Basic YWRtaW46cGFzc3dvcmQ=",
                         "--fail", "http://localhost:4985/scratch"]):
        raise Exception("Wating of SG up and running FAILED")
    print("SG is up")
    if not wait_success_process(["curl", "-sIkL", "-H", "Authorization: Basic YWRtaW46cGFzc3dvcmQ=", "--fail", "http://localhost:4885/scratch-30"]):
        raise Exception("Wating of SG Legacy up and running FAILED")
    print("SG Legacy is up")
    


@show_timing
def run_couchbase_and_syncway_in_docker(src_root: str, engine: str, use_sudo: bool) -> None:
    docker_path = path_to_docker_config(src_root)
    my_env = os.environ.copy()
    my_env["SSL"] = "false"
    check_call([*compose_command(engine, use_sudo), "up", "--build", "-d"], cwd = docker_path, env = my_env)
    wait_couchbase_and_syncway_in_docker_up()

@show_timing
def setup_users_in_sg() -> None:
    check_call(["curl", "--fail",  "-k", "--location", "--request", "POST", "http://localhost:4985/scratch/_user/",
                "--header", "Content-Type: application/json",
                "--header", "Authorization: Basic QWRtaW5pc3RyYXRvcjpwYXNzd29yZA==",
                "--data-raw", '{"name": "sguser", "password": "password", "collection_access": {"flowers": {"roses": {"admin_channels": ["*"]}, "tulips": {"admin_channels": ["*"]}, "lavenders": {"admin_channels": ["*"]}}}}'])
    check_call(["curl", "--fail",  "-k", "--location", "--request", "POST", "http://localhost:4885/scratch-30/_user/",
                "--header", "Content-Type: application/json",
                "--header", "Authorization: Basic QWRtaW5pc3RyYXRvcjpwYXNzd29yZA==",
                "--data-raw", '{"name": "sguser", "password": "password", "collection_access": {"flowers": {"roses": {"admin_channels": ["*"]}, "tulips": {"admin_channels": ["*"]}, "lavenders": {"admin_channels": ["*"]}}}}'])
    
@show_timing
def stop_couchbase_and_syncway_in_docker(src_root: str, engine: str, use_sudo: bool) -> None:
    docker_path = path_to_docker_config(src_root)
    check_call([*compose_command(engine, use_sudo), "down"], cwd = docker_path)


def main() -> None:
    args = parse_args(sys.argv[1:])
    ci_dir = Path(get_src_root_path(sys.argv[0]))
    src_root = ci_dir.parent
    if args.action == "up":
        run_couchbase_and_syncway_in_docker(src_root, args.engine, args.use_sudo)
        setup_users_in_sg()        
    elif args.action == "down":
        stop_couchbase_and_syncway_in_docker(src_root, args.engine, args.use_sudo)

if __name__ == "__main__":
    main()
