"""Small bounded TCP relay for real Sync Gateway transport fault tests.

Each direction forwards at most one read buffer at a time. A pause therefore
applies backpressure instead of accumulating an unbounded queue.
"""

import socket
import struct
import threading

BUFFER_SIZE = 16 * 1024


class TcpRelay:
    def __init__(self, upstream_host: str, upstream_port: int):
        self.upstream = (upstream_host, upstream_port)
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.listener.settimeout(0.5)
        self.port = self.listener.getsockname()[1]
        self.running = threading.Event()
        self.running.set()
        self.response_gate = threading.Event()
        self.response_gate.set()
        self.response_blocked = threading.Event()
        self.handshake_gate = threading.Event()
        self.handshake_gate.set()
        self.handshake_seen = threading.Event()
        self.connected = threading.Event()
        self.lock = threading.Lock()
        self.pairs: set[tuple[socket.socket, socket.socket]] = set()
        self.accept_thread = threading.Thread(target=self._accept, daemon=True)
        self.accept_thread.start()

    def _accept(self) -> None:
        while self.running.is_set():
            try:
                client, _ = self.listener.accept()
            except TimeoutError:
                continue
            except OSError:
                break
            try:
                server = socket.create_connection(self.upstream, timeout=10)
                server.settimeout(None)
            except OSError:
                client.close()
                continue
            pair = (client, server)
            with self.lock:
                self.pairs.add(pair)
            self.connected.set()
            threading.Thread(target=self._forward, args=(pair, client, server, True),
                             daemon=True).start()
            threading.Thread(target=self._forward, args=(pair, server, client, False),
                             daemon=True).start()

    def _forward(self, pair: tuple[socket.socket, socket.socket],
                 source: socket.socket, destination: socket.socket,
                 client_to_server: bool) -> None:
        first = True
        try:
            while self.running.is_set():
                data = source.recv(BUFFER_SIZE)
                if not data:
                    break
                if first and client_to_server and not self.handshake_gate.is_set():
                    self.handshake_seen.set()
                first = False
                gate = self.handshake_gate if client_to_server and self.handshake_seen.is_set() \
                    else self.response_gate if not client_to_server else None
                if gate is not None:
                    if not client_to_server and not gate.is_set():
                        self.response_blocked.set()
                    while self.running.is_set() and not gate.wait(0.1):
                        pass
                if not self.running.is_set():
                    break
                destination.sendall(data)
        except OSError:
            pass
        finally:
            self._close_pair(pair)

    def _close_pair(self, pair: tuple[socket.socket, socket.socket], *, reset: bool = False) -> bool:
        with self.lock:
            if pair not in self.pairs:
                return False
            self.pairs.remove(pair)
        for connection in pair:
            if reset:
                try:
                    connection.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER,
                                          struct.pack("ii", 1, 0))
                except OSError:
                    pass
            # close() alone can leave a forwarding thread blocked in recv(),
            # delaying the reset until that thread wakes up.
            try:
                connection.shutdown(socket.SHUT_RD if reset else socket.SHUT_RDWR)
            except OSError:
                pass
            connection.close()
        return True

    def hold_next_handshake(self) -> None:
        self.handshake_seen.clear()
        self.handshake_gate.clear()

    def release_handshake(self) -> None:
        self.handshake_gate.set()

    def pause_responses(self) -> None:
        self.response_blocked.clear()
        self.response_gate.clear()

    def resume_responses(self) -> None:
        self.response_gate.set()

    def reset_active(self) -> int:
        with self.lock:
            pairs = list(self.pairs)
        return sum(self._close_pair(pair, reset=True) for pair in pairs)

    def close(self) -> None:
        self.running.clear()
        self.release_handshake()
        self.resume_responses()
        self.listener.close()
        self.reset_active()
        self.accept_thread.join(timeout=2)
