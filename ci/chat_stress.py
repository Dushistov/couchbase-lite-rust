"""Drive ordinary chat-demo clients against the real Sync Gateway fixture.

Run through ``ci/build_and_run_tests.py --chat-stress``.
"""

import argparse
import base64
import hashlib
import json
import os
import random
import secrets
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from stress_proxy import TcpRelay

PROFILES = {"smoke": (4, 1000, 300), "stress": (8, 5000, 600)}
PAGE_SIZE = 17  # The small page size also exercises cursor handling.
COMMAND_TIMEOUT = 45
POLL_INTERVAL = 1.0
MAX_LOG_BYTES = 8 * 1024 * 1024
MAX_EVENTS = 50_000
MAX_FAILURE_DB_BYTES = 200 * 1024**2
TRACE_PATTERNS = {
    "ws_open": "open was called with uri",
    "connect_finished": "connect_async finished",
    "write": "write allocated_data.size",
    "completed_receive": "completedReceive, byte_count",
    "request_close": "requestClose status",
    "dispose": ": dispose",
    "close_sent": "local WebSocket close sent",
    "close_received": "close frame was received",
}


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def command_output(*command: str) -> str:
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout.strip()


def payload(seed: int, writer: int, sequence: int) -> str:
    """Stable payloads; changing the schedule never changes document contents."""
    if sequence == 0:
        return ""
    key = f"payload-v2:{seed}:{writer}:{sequence}".encode()
    if sequence % 211 == 0:
        size = 256 * 1024
    elif sequence % 89 == 0:
        size = 64 * 1024
    elif sequence % 31 == 0:
        size = 16 * 1024
    elif sequence % 13 == 0:
        return "repeated:" + "chat " * 200
    elif sequence % 17 == 0:
        return f"Unicode chat 🌍 こんにちは {writer}/{sequence}"
    else:
        digest = hashlib.sha256(key).hexdigest()
        return f"chat {writer}/{sequence} {digest[:(sequence % 7 + 1) * 8]}"
    data = hashlib.shake_256(key).digest(size)
    return f"random:{writer}/{sequence}:" + base64.b64encode(data).decode()


def compare(expected: dict[str, dict[str, Any]],
            actual: dict[str, dict[str, Any]]) -> dict[str, Any]:
    missing = sorted(expected.keys() - actual.keys())
    extra = sorted(actual.keys() - expected.keys())
    wrong = {
        doc_id: {"expected": expected[doc_id], "actual": actual[doc_id]}
        for doc_id in expected.keys() & actual.keys()
        if expected[doc_id] != actual[doc_id]
    }
    return {"missing": missing, "extra": extra, "wrong": wrong}


def equal_inventory(expected: dict[str, dict[str, Any]],
                    actual: dict[str, dict[str, Any]]) -> bool:
    result = compare(expected, actual)
    return not any(result.values())


class Gateway:
    def __init__(self, public_url: str, admin_url: str, username: str, password: str):
        self.public_url = public_url.rstrip("/") + "/"
        self.admin_url = admin_url.rstrip("/") + "/"
        self.username = username
        self.password = password

    @staticmethod
    def request(base: str, path: str, username: str, password: str,
                *, method: str = "GET", body: Any = None) -> dict[str, Any]:
        headers = {
            "Authorization": "Basic " + base64.b64encode(
                f"{username}:{password}".encode()).decode(),
        }
        data = None
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(base + path, data=data, headers=headers, method=method)
        with urllib.request.urlopen(request, timeout=15) as response:
            raw = response.read()
        return json.loads(raw) if raw else {}

    def provision(self, channel: str, user: str, password: str) -> None:
        self.request(self.admin_url, "_user/", "Administrator", "password",
                     method="POST", body={"name": user, "password": password,
                                          "admin_channels": [channel]})

    def inventory(self, id_prefix: str) -> dict[str, dict[str, Any]]:
        """Page through the public changes feed using its opaque sequence token."""
        result: dict[str, dict[str, Any]] = {}
        since: str | int = 0
        for _ in range(10000):
            query = urllib.parse.urlencode({"since": since, "limit": PAGE_SIZE,
                                            "style": "all_docs", "include_docs": "true"})
            page = self.request(self.public_url, "_changes?" + query,
                                self.username, self.password)
            rows = page["results"]
            if not rows:
                return result
            next_since = page["last_seq"]
            if next_since == since:
                raise RuntimeError("Sync Gateway changes cursor did not advance")
            for row in rows:
                doc_id = row["id"]
                if not doc_id.startswith(id_prefix):
                    continue
                if doc_id in result:
                    raise RuntimeError(f"duplicate server document in changes pages: {doc_id}")
                if row.get("deleted") or len(row.get("changes", [])) != 1:
                    result[doc_id] = {"deleted": bool(row.get("deleted")),
                                      "revisions": len(row.get("changes", []))}
                    continue
                doc = row.get("doc")
                if not isinstance(doc, dict):
                    raise TypeError(f"server omitted body for {doc_id}")
                result[doc_id] = {"type": doc.get("type"), "msg": doc.get("msg"),
                                  "channels": doc.get("channels", [])}
            since = next_since
        raise RuntimeError("Sync Gateway changes feed exceeded 10000 pages")


class Client:
    def __init__(self, index: int, binary: Path, db_path: Path, url: str,
                 user: str, password: str, channel: str, artifacts: Path,
                 *, offline: bool = False, workers: int = 2):
        self.index = index
        self.db_path = db_path
        self.artifacts = artifacts
        self.events: list[dict[str, Any]] = []
        self.failure_events: list[dict[str, Any]] = []
        self.condition = threading.Condition()
        self.command_number = 0
        self.reader_errors: list[str] = []
        self.stdout_done = False
        self.stderr_done = threading.Event()
        self.trace_counts = dict.fromkeys(TRACE_PATTERNS, 0)
        self._log_bytes = 0
        command = [str(binary), "--channel", channel, "--output", "jsonl",
                   "--conflicts", "report"]
        if offline:
            command.append("--offline")
        command.extend(["--", str(db_path), url, user, password])
        env = os.environ.copy()
        env["TOKIO_WORKER_THREADS"] = str(workers)
        env["RUST_LOG"] = "couchbase_lite::replicator::tokio_socket=trace"
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, bufsize=1, env=env)
        self.artifacts.mkdir(parents=True, exist_ok=True)
        threading.Thread(target=self._read_stdout, daemon=True).start()
        threading.Thread(target=self._read_stderr, args=(password,), daemon=True).start()

    def _read_stdout(self) -> None:
        assert self.process.stdout is not None
        with (self.artifacts / "events.jsonl").open("w", encoding="utf-8") as log:
            for line in self.process.stdout:
                try:
                    event = json.loads(line)
                    if not isinstance(event, dict) or "type" not in event:
                        raise ValueError("output is not a typed event")
                except (json.JSONDecodeError, ValueError) as error:
                    with self.condition:
                        self.reader_errors.append(f"invalid JSONL: {error}: {line[:200]}")
                        self.condition.notify_all()
                    continue
                with self.condition:
                    self.events.append(event)
                    if event["type"] in {"conflict", "document_error",
                                         "validation_rejected", "error"}:
                        self.failure_events.append(event)
                    if len(self.events) > MAX_EVENTS:
                        del self.events[:1000]
                    self.condition.notify_all()
                if self._log_bytes < MAX_LOG_BYTES:
                    entry = json.dumps({"time": time.time(), "event": event}) + "\n"
                    log.write(entry)
                    self._log_bytes += len(entry)
        with self.condition:
            self.stdout_done = True
            self.condition.notify_all()

    def _read_stderr(self, password: str) -> None:
        assert self.process.stderr is not None
        count = 0
        with (self.artifacts / "stderr.log").open("w", encoding="utf-8") as log:
            for line in self.process.stderr:
                for name, pattern in TRACE_PATTERNS.items():
                    self.trace_counts[name] += line.count(pattern)
                if count < MAX_LOG_BYTES:
                    clean = line.replace(password, "[REDACTED]")
                    log.write(clean)
                    count += len(clean)
        self.stderr_done.set()

    def wait(self, event_type: str, command: int, *, timeout: float = COMMAND_TIMEOUT
             ) -> tuple[dict[str, Any], list[dict[str, Any]]]:
        deadline = time.monotonic() + timeout
        with self.condition:
            while True:
                matches = [event for event in self.events
                           if event.get("command") == command]
                for event in matches:
                    if event["type"] == "error":
                        raise RuntimeError(f"client {self.index} command {command}: {event['message']}")
                    if event["type"] == event_type:
                        return event, matches
                if self.reader_errors:
                    raise RuntimeError(f"client {self.index}: {self.reader_errors[0]}")
                if self.process.poll() is not None and self.stdout_done:
                    raise RuntimeError(f"client {self.index} exited {self.process.returncode} "
                                       f"while waiting for {event_type}")
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"client {self.index} timed out waiting for {event_type} "
                                       f"(command {command})")
                self.condition.wait(min(remaining, 0.5))

    def send(self, text: str) -> int:
        self.command_number += 1
        if self.process.stdin is None:
            raise RuntimeError("client stdin is closed")
        self.process.stdin.write(text + "\n")
        self.process.stdin.flush()
        return self.command_number

    def list_docs(self) -> dict[str, dict[str, Any]]:
        command = self.send("!list")
        _, events = self.wait("list_end", command)
        if not any(event["type"] == "list_begin" for event in events):
            raise RuntimeError(f"client {self.index} exported without list_begin")
        docs: dict[str, dict[str, Any]] = {}
        for event in events:
            if event["type"] != "document":
                continue
            doc_id = event["id"]
            if doc_id in docs:
                raise RuntimeError(f"client {self.index} exported duplicate {doc_id}")
            if event["deleted"] or event["conflicted"]:
                docs[doc_id] = {"deleted": event["deleted"],
                                "conflicted": event["conflicted"]}
            else:
                docs[doc_id] = {"type": "Message", "msg": event["msg"],
                                "channels": event["channels"]}
        return docs

    def quit(self) -> None:
        if self.process.poll() is not None:
            raise RuntimeError(f"client {self.index} exited before !quit")
        command = self.send("!quit")
        event, _ = self.wait("quit", command, timeout=60)
        if event["result"] != "stopped":
            raise RuntimeError(f"client {self.index} did not stop cleanly")
        self.process.wait(timeout=10)
        if self.process.returncode != 0:
            raise RuntimeError(f"client {self.index} exited {self.process.returncode}")
        if not self.stderr_done.wait(5):
            raise TimeoutError(f"client {self.index} stderr did not drain after exit")

    def kill(self) -> None:
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=10)


@dataclass
class Settings:
    profile: str
    seed: int
    clients: int
    messages: int
    converge_seconds: int
    artifacts: Path
    binary: Path
    public_url: str
    admin_url: str
    websocket_url: str
    workers: int
    replay_schedule: list[dict[str, Any]] | None
    replay_of: str | None


def settings_from_args(argv: list[str], src_root: Path) -> Settings:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=PROFILES)
    parser.add_argument("--seed", type=int)
    parser.add_argument("--clients", type=int)
    parser.add_argument("--messages", type=int)
    parser.add_argument("--converge-seconds", type=int)
    parser.add_argument("--artifacts", type=Path, default=Path("/tmp/chat-stress-artifacts"))
    parser.add_argument("--binary", type=Path, default=src_root / "target/debug/chat-demo")
    parser.add_argument("--public-url", default="http://localhost:4884/scratch-30/")
    parser.add_argument("--admin-url", default="http://localhost:4885/scratch-30/")
    parser.add_argument("--websocket-url", default="ws://localhost:4884/scratch-30/")
    parser.add_argument("--workers", type=int, choices=(1, 2, 4))
    parser.add_argument("--replay", type=Path, help="Replay a prior manifest in a new namespace")
    args = parser.parse_args(argv)
    previous = json.loads(args.replay.read_text(encoding="utf-8")) if args.replay else None
    if previous and previous.get("generator") != "payload-v2":
        parser.error("replay manifest uses an incompatible payload generator")
    profile = args.profile or (previous["profile"] if previous else "smoke")
    seed = args.seed if args.seed is not None else (previous["seed"] if previous else 12345)
    default_clients, default_messages, default_seconds = PROFILES[profile]
    clients = args.clients if args.clients is not None else (
        previous["clients"] if previous else default_clients)
    messages = args.messages if args.messages is not None else (
        previous["messages"] if previous else default_messages)
    if not (2 <= clients <= 64 and clients <= messages <= 10000):
        parser.error("require 2..64 clients and clients <= messages <= 10000")
    workers = args.workers if args.workers is not None else (
        previous["workers"] if previous else 2)
    exact_replay = bool(previous and profile == previous["profile"]
                        and clients == previous["clients"]
                        and messages == previous["messages"]
                        and seed == previous["seed"])
    return Settings(profile, seed, clients, messages,
                    args.converge_seconds or default_seconds, args.artifacts,
                    args.binary, args.public_url, args.admin_url,
                    args.websocket_url, workers,
                    previous["planned_schedule"] if exact_replay and previous is not None else None,
                    previous["run_id"] if previous else None)


def make_schedule(settings: Settings, run_id: str, channel: str
                  ) -> tuple[dict[str, dict[str, Any]], list[list[dict[str, Any]]]]:
    expected: dict[str, dict[str, Any]] = {}
    per_writer: list[list[dict[str, Any]]] = [[] for _ in range(settings.clients)]
    for sequence in range(settings.messages):
        writer = sequence % settings.clients
        local_sequence = sequence // settings.clients
        doc_id = f"stress:{run_id}:{writer}:{local_sequence}"
        message = payload(settings.seed, writer, local_sequence)
        expected[doc_id] = {"type": "Message", "msg": message,
                            "channels": [channel]}
        per_writer[writer].append({"id": doc_id, "msg": message})
    return expected, per_writer


def plan_waves(seed: int, profile: str, lengths: list[int]) -> list[dict[str, int]]:
    remaining = lengths.copy()
    remaining[0] -= 1  # The first document is the channel-access probe.
    rng = random.Random(seed ^ 0x51C2)
    batch_size = 8 if profile == "smoke" else 24
    plan: list[dict[str, int]] = []
    wave = 0
    while any(remaining):
        order = list(range(len(remaining)))
        rng.shuffle(order)
        for writer in order:
            size = min(batch_size, remaining[writer])
            if size:
                plan.append({"wave": wave, "writer": writer, "size": size})
                remaining[writer] -= size
        wave += 1
    return plan


def wait_inventory(get_inventory: Any, expected: dict[str, dict[str, Any]],
                   deadline: float, label: str) -> dict[str, dict[str, Any]]:
    last: dict[str, dict[str, Any]] = {}
    while time.monotonic() < deadline:
        last = get_inventory()
        if equal_inventory(expected, last):
            return last
        time.sleep(POLL_INTERVAL)
    raise TimeoutError(f"{label} did not converge: {json.dumps(compare(expected, last))[:4000]}")


def resource_sample(clients: list[Client], workspace: Path) -> dict[str, Any]:
    disk = shutil.disk_usage(workspace)
    sample: dict[str, Any] = {"time": time.time(), "disk_free": disk.free,
                              "load_average": os.getloadavg(), "clients": []}
    memory = Path("/proc/meminfo")
    if memory.exists():
        values = {}
        for line in memory.read_text(encoding="utf-8").splitlines():
            key, _, amount = line.partition(":")
            if key in {"MemTotal", "MemAvailable"}:
                values[key] = int(amount.split()[0]) * 1024
        sample["memory_total"] = values["MemTotal"]
        sample["memory_available"] = values["MemAvailable"]
    for client in clients:
        process_dir = Path(f"/proc/{client.process.pid}")
        status = process_dir / "status"
        rss = threads = None
        if status.exists():
            for line in status.read_text(encoding="utf-8").splitlines():
                if line.startswith("VmRSS:"):
                    rss = int(line.split()[1]) * 1024
                elif line.startswith("Threads:"):
                    threads = int(line.split()[1])
        stat = process_dir / "stat"
        cpu_ticks = None
        if stat.exists():
            # The comm field is parenthesized and may contain spaces.
            fields = stat.read_text(encoding="utf-8").rsplit(")", 1)[1].split()
            cpu_ticks = int(fields[11]) + int(fields[12])
        fd_dir = process_dir / "fd"
        file_descriptors = len(list(fd_dir.iterdir())) if fd_dir.exists() else None
        db_bytes = sum(path.stat().st_size for path in client.db_path.rglob("*")
                       if path.is_file())
        sample["clients"].append({"index": client.index, "pid": client.process.pid,
                                  "exit": client.process.poll(), "rss": rss,
                                  "threads": threads, "cpu_ticks": cpu_ticks,
                                  "file_descriptors": file_descriptors,
                                  "database_bytes": db_bytes})
    return sample


def socket_trace_counts(clients: list[Client]) -> dict[str, int]:
    counts = dict.fromkeys(TRACE_PATTERNS, 0)
    for client in clients:
        for name, count in client.trace_counts.items():
            counts[name] += count
    return counts


def run(settings: Settings, src_root: Path) -> None:
    if not settings.binary.is_file():
        raise FileNotFoundError(settings.binary)
    if shutil.disk_usage(src_root).free < 3 * 1024**3:
        raise RuntimeError("fewer than 3 GiB free before the stress run")
    initial_resources = resource_sample([], src_root)
    if initial_resources.get("memory_available", 0) < \
            initial_resources.get("memory_total", 0) // 4:
        raise RuntimeError("less than 25% memory headroom before the stress run")
    settings.artifacts.mkdir(parents=True, exist_ok=True)
    run_id = secrets.token_hex(6)
    channel = f"stress-{run_id}"
    user = f"stress_{run_id}"
    password = secrets.token_urlsafe(24)
    id_prefix = f"stress:{run_id}:"
    expected, per_writer = make_schedule(settings, run_id, channel)
    estimated_delivery_bytes = sum(len(item["msg"].encode()) for items in per_writer
                                   for item in items) * (settings.clients + 1)
    if estimated_delivery_bytes > 512 * 1024**2:
        raise RuntimeError("capacity: estimated replicated payload exceeds 512 MiB")
    work_dir = Path(tempfile.mkdtemp(prefix=f"chat-stress-{run_id}-"))
    failed = True
    planned_schedule = settings.replay_schedule or plan_waves(
        settings.seed, settings.profile, [len(items) for items in per_writer])
    manifest: dict[str, Any] = {"run_id": run_id, "channel": channel, "profile": settings.profile,
                "seed": settings.seed, "generator": "payload-v2", "clients": settings.clients,
                "messages": settings.messages, "workers": settings.workers,
                "chat_demo_features": ["build-cpp", "use-couchbase-lite-sqlite",
                                       "use-tokio-websocket"],
                "expected": expected, "acknowledged": [], "schedule": [],
                "planned_schedule": planned_schedule,
                "binary": str(settings.binary), "websocket_url": settings.websocket_url,
                "replay_of": settings.replay_of}
    manifest_path = settings.artifacts / "manifest.json"
    write_json(manifest_path, manifest)
    environment = {"git_commit": command_output("git", "-C", str(src_root), "rev-parse", "HEAD"),
                   "rustc": command_output("rustc", "--version"),
                   "cargo": command_output("cargo", "--version"),
                   "compose_sha256": hashlib.sha256(
                       (src_root / "ci/docker/docker-compose.yml").read_bytes()).hexdigest()}
    core_src = os.environ.get("COUCHBASE_LITE_CORE_SRC_DIR")
    if core_src:
        environment["core_commit"] = command_output("git", "-C", core_src, "rev-parse", "HEAD")
    write_json(settings.artifacts / "environment.json", environment)
    gateway = Gateway(settings.public_url, settings.admin_url, user, password)
    clients: list[Client] = []
    active: dict[int, Client] = {}
    relays: dict[int, TcpRelay] = {}
    reader: Client | None = None
    started = time.monotonic()

    def trace(action: str, **fields: Any) -> None:
        with (settings.artifacts / "actions.jsonl").open("a", encoding="utf-8") as log:
            log.write(json.dumps({"time": time.time(), "action": action, **fields}) + "\n")

    def launch(index: int, *, offline: bool = False) -> Client:
        websocket_url = settings.websocket_url
        if index in relays:
            parsed = urllib.parse.urlsplit(websocket_url)
            websocket_url = urllib.parse.urlunsplit(
                (parsed.scheme, f"127.0.0.1:{relays[index].port}", parsed.path,
                 parsed.query, ""))
        client = Client(index, settings.binary,
                        work_dir / f"client-{index}.cblite2",
                        websocket_url, user, password, channel,
                        settings.artifacts / f"client-{index}-{len(clients)}",
                        offline=offline, workers=settings.workers)
        clients.append(client)
        active[index] = client
        trace("launch", index=index, offline=offline, pid=client.process.pid)
        client.list_docs()  # Ready barrier; also checks JSON output.
        return client

    try:
        if settings.profile == "stress":
            parsed = urllib.parse.urlsplit(settings.websocket_url)
            if parsed.scheme != "ws" or parsed.hostname is None:
                raise ValueError("TCP fault relay requires a plaintext ws URL")
            for index in range(settings.clients):
                relays[index] = TcpRelay(parsed.hostname, parsed.port or 80)
        gateway.provision(channel, user, password)
        for index in range(settings.clients):
            launch(index)
        # A real channel-access check before loading the shared database.
        first = per_writer[0].pop(0)
        command = active[0].send("!send " + json.dumps([first]))
        receipt, _ = active[0].wait("saved", command)
        if receipt["ids"] != [first["id"]]:
            raise RuntimeError("initial commit receipt did not match the submitted ID")
        manifest["acknowledged"].append(first["id"])
        trace("acknowledged", index=0, ids=[first["id"]])
        write_json(manifest_path, manifest)
        known = {first["id"]: expected[first["id"]]}
        deadline = time.monotonic() + settings.converge_seconds
        wait_inventory(lambda: gateway.inventory(id_prefix), known, deadline, "channel probe on server")
        wait_inventory(lambda: {key: value for key, value in active[1].list_docs().items()
                                if key.startswith(id_prefix)}, known, deadline,
                       "channel probe on second client")
        # A late reader is useful for smoke, but a 5,000-document catch-up
        # overwhelms chat-demo's bounded callback output queue. The stress
        # reader starts with a clean database and follows the entire workload.
        if settings.profile == "stress":
            reader = launch(settings.clients)

        wave = 0
        while any(per_writer):
            if settings.profile == "stress" and wave == 0:
                relays[0].pause_responses()
                trace("response_pause", index=0)
            entries = [entry for entry in planned_schedule if entry["wave"] == wave]
            if not entries:
                raise RuntimeError(f"schedule ended before wave {wave}")
            pending: list[tuple[int, int, list[dict[str, Any]]]] = []
            for entry in entries:
                index = entry["writer"]
                size = int(entry["size"])
                batch = per_writer[index][:size]
                del per_writer[index][:len(batch)]
                if len(batch) != size:
                    raise RuntimeError(f"schedule requested {size} unavailable messages from {index}")
                command = active[index].send("!send " + json.dumps(batch))
                pending.append((index, command, batch))
                manifest["schedule"].append({"wave": wave, "writer": index,
                                              "command": command,
                                              "ids": [item["id"] for item in batch]})
            write_json(manifest_path, manifest)
            for index, command, batch in pending:
                receipt, _ = active[index].wait("saved", command)
                if receipt["ids"] != [item["id"] for item in batch]:
                    raise RuntimeError(f"client {index} committed a different batch")
                manifest["acknowledged"].extend(receipt["ids"])
                trace("acknowledged", index=index, command=command, ids=receipt["ids"])
            write_json(manifest_path, manifest)
            if settings.profile == "stress" and wave == 0:
                if not relays[0].response_blocked.wait(COMMAND_TIMEOUT):
                    raise TimeoutError("response pause did not block server data")
                relays[0].resume_responses()
                trace("response_resumed", index=0)
            if settings.profile == "stress" and wave == 1:
                trace("offline", index=0)
                command = active[0].send("!offline")
                active[0].wait("offline", command)
                trace("online", index=0)
                command = active[0].send("!online")
                active[0].wait("online", command)
                command = active[0].send("!retry")
                active[0].wait("retry", command)
                trace("reopen", index=1)
                active[1].quit()
                launch(1)
            if settings.profile == "stress" and wave == 2:
                previous_events = len(active[0].events)
                reset_count = relays[0].reset_active()
                trace("tcp_reset", index=0, connections=reset_count)
                if reset_count == 0:
                    raise RuntimeError("TCP reset had no active connection to affect")
                reset_deadline = time.monotonic() + COMMAND_TIMEOUT
                while not any(event.get("type") == "state" and
                              event.get("state") in {"offline", "connecting"}
                              for event in active[0].events[previous_events:]):
                    if time.monotonic() >= reset_deadline:
                        raise TimeoutError("TCP reset did not produce a replicator state transition")
                    time.sleep(0.1)
                trace("crash", index=2, acknowledged=len(manifest["acknowledged"]))
                active[2].kill()  # Only acknowledged transactions are in the manifest.
                launch(2, offline=True)
                local = active[2].list_docs()
                assigned = {key: value for key, value in expected.items()
                            if key in manifest["acknowledged"] and key.startswith(f"{id_prefix}2:")}
                if not equal_inventory(assigned, {key: value for key, value in local.items()
                                                  if key.startswith(f"{id_prefix}2:")}):
                    raise RuntimeError("crash recovery lost an acknowledged local transaction")
                relays[2].hold_next_handshake()
                command = active[2].send("!online")
                active[2].wait("online", command)
                if not relays[2].handshake_seen.wait(COMMAND_TIMEOUT):
                    raise TimeoutError("handshake pause did not observe a client request")
                trace("handshake_paused", index=2)
                command = active[2].send("!offline")
                active[2].wait("offline", command)
                relays[2].release_handshake()
                trace("handshake_released", index=2)
                command = active[2].send("!online")
                active[2].wait("online", command)
            wave += 1
            if shutil.disk_usage(src_root).free < 3 * 1024**3:
                raise RuntimeError("capacity: disk dropped below 3 GiB during the run")
            resources = resource_sample(list(active.values()), src_root)
            if resources.get("memory_available", 0) < resources.get("memory_total", 0) // 4:
                raise RuntimeError("capacity: memory headroom dropped below 25%")
            with (settings.artifacts / "resources.jsonl").open("a", encoding="utf-8") as log:
                log.write(json.dumps(resources) + "\n")
        if set(manifest["acknowledged"]) != expected.keys():
            raise RuntimeError("freeze failed: incomplete acknowledged message range")
        server = wait_inventory(lambda: gateway.inventory(id_prefix), expected,
                                time.monotonic() + settings.converge_seconds, "server")
        write_json(settings.artifacts / "server.json", server)
        for index, client in active.items():
            local = wait_inventory(lambda client=client: {
                key: value for key, value in client.list_docs().items()
                if key.startswith(id_prefix)}, expected,
                time.monotonic() + settings.converge_seconds, f"client {index}")
            write_json(settings.artifacts / f"client-{index}-inventory.json", local)
        if reader is None:
            reader = launch(settings.clients)
        fresh = wait_inventory(lambda: {key: value for key, value in reader.list_docs().items()
                                        if key.startswith(id_prefix)}, expected,
                               time.monotonic() + settings.converge_seconds,
                               "fresh reader")
        write_json(settings.artifacts / "fresh-reader.json", fresh)
        close_wait: dict[str, Any] | None = None
        if settings.profile == "stress":
            relay = relays[0]
            sent_before = active[0].trace_counts["close_sent"]
            received_before = active[0].trace_counts["close_received"]
            relay.pause_responses()
            trace("close_response_pause", index=0)
            close_started = time.monotonic()
            try:
                active[0].quit()
            finally:
                elapsed = time.monotonic() - close_started
                active[0].stderr_done.wait(5)
                close_wait = {"elapsed_seconds": elapsed,
                              "server_bytes_blocked": relay.response_blocked.is_set(),
                              "local_close_sent": active[0].trace_counts["close_sent"] > sent_before,
                              "reply_observed": active[0].trace_counts["close_received"] > received_before,
                              "process_exit": active[0].process.poll()}
                write_json(settings.artifacts / "close-response.json", close_wait)
                relay.resume_responses()
                trace("close_response_released", index=0, **close_wait)
            if not close_wait["server_bytes_blocked"] or not close_wait["local_close_sent"]:
                raise RuntimeError(f"close-response pause missed its trigger: {close_wait}")
            del active[0]
        for client in active.values():
            client.quit()
        if any(client.failure_events for client in clients):
            raise RuntimeError("an unexpected conflict or error event occurred")
        socket_counts = socket_trace_counts(clients)
        if not all(socket_counts[key] for key in ("ws_open", "write", "completed_receive",
                                                 "request_close", "dispose")):
            raise RuntimeError(f"missing baseline socket trace evidence: {socket_counts}")
        observed = ["normal_replication", "concurrent_push_pull", "graceful_shutdown"]
        if settings.profile == "stress":
            observed.extend(["clean_reader_from_probe", "offline_online", "retry", "reopen",
                             "committed_barrier_crash", "handshake_pause", "tcp_reset",
                             "server_response_pause", "response_withheld_during_close"])
        else:
            observed.append("fresh_reader_after_convergence")
        write_json(settings.artifacts / "coverage.json",
                   {"observed_application_scenarios": observed,
                    "socket_trace_counts": socket_counts,
                    "socket_callback_branches": "trace counts show calls, not complete callback ordering",
                    "close_response_pause": close_wait,
                    "remaining": ["5,000-document cold catch-up exceeds chat-demo output queue",
                                  "close timeout owner requires branch instrumentation",
                                  "server-initiated close", "TLS and auth failures",
                                  "frame mutation", "internal invariant branches"]})
        write_json(settings.artifacts / "summary.json",
                   {"result": "passed", "run_id": run_id,
                    "elapsed_seconds": time.monotonic() - started,
                    "documents": len(expected), "clients": settings.clients})
        failed = False
    except Exception as error:
        write_json(settings.artifacts / "summary.json",
                   {"result": "failed", "run_id": run_id, "error": str(error),
                    "elapsed_seconds": time.monotonic() - started})
        raise
    finally:
        write_json(manifest_path, manifest)
        for client in clients:
            if client.process.poll() is None:
                try:
                    client.quit()
                except (OSError, RuntimeError, TimeoutError, subprocess.TimeoutExpired):
                    client.kill()
        for relay in relays.values():
            relay.close()
        if failed:
            write_json(settings.artifacts / "failure-diagnostics.json", {
                "clients": [
                    {"index": client.index, "pid": client.process.pid,
                     "exit": client.process.poll(), "socket_trace_counts": client.trace_counts,
                     "last_events": [
                         {"type": event.get("type"), "command": event.get("command"),
                          "state": event.get("state"), "generation": event.get("generation"),
                          "error": event.get("error")}
                         for event in client.events[-10:]]}
                    for client in clients
                ],
                "relays": {str(index): {"response_blocked": relay.response_blocked.is_set(),
                                        "handshake_seen": relay.handshake_seen.is_set()}
                           for index, relay in relays.items()},
            })
            database_artifacts = settings.artifacts / "db"
            database_artifacts.mkdir(exist_ok=True)
            remaining_bytes = MAX_FAILURE_DB_BYTES
            for path in work_dir.glob("*.cblite2"):
                size = sum(item.stat().st_size for item in path.rglob("*") if item.is_file())
                if size <= min(100 * 1024**2, remaining_bytes):
                    shutil.copytree(path, database_artifacts / path.name, dirs_exist_ok=True)
                    remaining_bytes -= size
        shutil.rmtree(work_dir)


def main(argv: list[str], src_root: Path) -> None:
    settings = settings_from_args(argv, src_root)
    run(settings, src_root)


if __name__ == "__main__":
    main(sys.argv[1:], Path(__file__).resolve().parent.parent)
