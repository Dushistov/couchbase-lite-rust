#!/usr/bin/env python3

import argparse
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

DOCKER_DIR = Path(__file__).resolve().parent / "docker"
COLLECTIONS_ADMIN = "http://localhost:4985/scratch"
LEGACY_ADMIN = "http://localhost:4885/scratch-30"
USER_BODY = (
    '{"name":"sguser","password":"password","collection_access":'
    '{"flowers":{"roses":{"admin_channels":["*"]},'
    '"tulips":{"admin_channels":["*"]},'
    '"lavenders":{"admin_channels":["*"]}}}}'
)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("up", "down"))
    parser.add_argument("--engine", choices=("docker", "podman"), default="docker")
    sudo = parser.add_mutually_exclusive_group()
    sudo.add_argument("--sudo", dest="use_sudo", action="store_true", default=True)
    sudo.add_argument("--no-sudo", dest="use_sudo", action="store_false")
    parser.add_argument("--project-name", help="Isolated Compose project name")
    parser.add_argument("--stress-only", action="store_true",
                        help="Start only Couchbase Server and legacy Sync Gateway")
    return parser.parse_args(argv)


def compose_command(args: argparse.Namespace) -> list[str]:
    if not shutil.which(args.engine):
        raise RuntimeError(f"Container engine not found: {args.engine}")
    command = [args.engine, "compose"]
    if args.project_name:
        command.extend(["--project-name", args.project_name])
    return ["sudo", "env", "SSL=false", *command] if args.use_sudo else command


def wait_ready(url: str, *, timeout: int = 600) -> None:
    deadline = time.monotonic() + timeout
    consecutive = 0
    while time.monotonic() < deadline:
        result = subprocess.run(
            ["curl", "--silent", "--fail", "--max-time", "5",
             "--user", "Administrator:password", "--output", "/dev/null", url],
            check=False, capture_output=True,
        )
        if result.returncode == 0:
            consecutive += 1
            if consecutive == 2:
                print(f"Ready: {url}", flush=True)
                return
        else:
            consecutive = 0
        time.sleep(5)
    raise TimeoutError(f"Sync Gateway did not become ready: {url}")


def setup_user(admin_url: str) -> None:
    subprocess.run(
        ["curl", "--silent", "--show-error", "--fail", "--request", "POST",
         "--user", "Administrator:password", "--header", "Content-Type: application/json",
         "--data-raw", USER_BODY, f"{admin_url}/_user/"],
        check=True, stdout=subprocess.DEVNULL,
    )


def main(argv: list[str] | None = None) -> None:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    compose = compose_command(args)
    env = os.environ.copy()
    env["SSL"] = "false"
    if args.action == "up":
        services = ["lite-core-cbs", "lite-core-sg-legacy"] if args.stress_only else []
        subprocess.run([*compose, "up", "--build", "-d", *services],
                       check=True, cwd=DOCKER_DIR, env=env)
        if not args.stress_only:
            wait_ready(COLLECTIONS_ADMIN + "/_user/")
        wait_ready(LEGACY_ADMIN + "/_user/")
        if not args.stress_only:
            setup_user(COLLECTIONS_ADMIN)
            setup_user(LEGACY_ADMIN)
    else:
        subprocess.run([*compose, "down"], check=True, cwd=DOCKER_DIR, env=env)


if __name__ == "__main__":
    main()
