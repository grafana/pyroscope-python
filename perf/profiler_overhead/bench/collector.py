from __future__ import annotations

import argparse
import gzip
import json
import threading
import time
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

from .workloads import STACK_TRACE_DEPTH


def _read_varint(data: bytes, offset: int) -> tuple[int, int]:
    value = 0
    shift = 0
    while offset < len(data) and shift < 70:
        byte = data[offset]
        offset += 1
        value |= (byte & 0x7F) << shift
        if byte < 0x80:
            return value, offset
        shift += 7
    raise ValueError("invalid protobuf varint")


def _protobuf_fields(data: bytes) -> list[tuple[int, int, int | bytes]]:
    fields: list[tuple[int, int, int | bytes]] = []
    offset = 0
    while offset < len(data):
        key, offset = _read_varint(data, offset)
        field_number, wire_type = key >> 3, key & 7
        if field_number == 0:
            raise ValueError("invalid protobuf field number")
        if wire_type == 0:
            value, offset = _read_varint(data, offset)
        elif wire_type == 1:
            value = data[offset : offset + 8]
            offset += 8
        elif wire_type == 2:
            size, offset = _read_varint(data, offset)
            value = data[offset : offset + size]
            if len(value) != size:
                raise ValueError("truncated protobuf field")
            offset += size
        elif wire_type == 5:
            value = data[offset : offset + 4]
            offset += 4
        else:
            raise ValueError(f"unsupported protobuf wire type {wire_type}")
        fields.append((field_number, wire_type, value))
    return fields


def _sample_stack_depth(sample: bytes) -> int:
    depth = 0
    for field, wire_type, value in _protobuf_fields(sample):
        if field != 1:
            continue
        if wire_type == 0:
            depth += 1
        elif wire_type == 2 and isinstance(value, bytes):
            offset = 0
            while offset < len(value):
                _, offset = _read_varint(value, offset)
                depth += 1
    return depth


def max_pprof_stack_depth(compressed_push_request: bytes) -> int:
    push_request = gzip.decompress(compressed_push_request)
    maximum = 0
    for field, wire_type, series in _protobuf_fields(push_request):
        if field != 1 or wire_type != 2 or not isinstance(series, bytes):
            continue
        for series_field, series_wire, raw_sample in _protobuf_fields(series):
            if series_field != 2 or series_wire != 2 or not isinstance(raw_sample, bytes):
                continue
            for sample_field, sample_wire, profile in _protobuf_fields(raw_sample):
                if sample_field != 1 or sample_wire != 2 or not isinstance(profile, bytes):
                    continue
                for profile_field, profile_wire, sample in _protobuf_fields(profile):
                    if (
                        profile_field == 2
                        and profile_wire == 2
                        and isinstance(sample, bytes)
                    ):
                        maximum = max(maximum, _sample_stack_depth(sample))
    return maximum


class Counters:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self.profile_requests = 0
        self.profile_bytes = 0
        self.incomplete_profiles = 0
        self.decoded_profiles = 0
        self.profile_decode_errors = 0
        self.deep_profile_requests = 0
        self.max_stack_depth = 0
        self.io_requests = 0

    def add_profile(
        self,
        size: int,
        incomplete: bool,
        stack_depth: int | None,
    ) -> None:
        with self._lock:
            self.profile_requests += 1
            self.profile_bytes += size
            self.incomplete_profiles += int(incomplete)
            self.profile_decode_errors += int(stack_depth is None)
            if stack_depth is not None:
                self.decoded_profiles += 1
                self.max_stack_depth = max(self.max_stack_depth, stack_depth)
                self.deep_profile_requests += int(stack_depth >= STACK_TRACE_DEPTH)

    def add_io(self) -> None:
        with self._lock:
            self.io_requests += 1

    def snapshot(self) -> dict[str, int]:
        with self._lock:
            return {
                "profile_requests": self.profile_requests,
                "profile_bytes": self.profile_bytes,
                "incomplete_profiles": self.incomplete_profiles,
                "decoded_profiles": self.decoded_profiles,
                "profile_decode_errors": self.profile_decode_errors,
                "deep_profile_requests": self.deep_profile_requests,
                "max_stack_depth": self.max_stack_depth,
                "io_requests": self.io_requests,
            }


COUNTERS = Counters()


class CollectorServer(ThreadingHTTPServer):
    daemon_threads = True


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self) -> None:
        size = int(self.headers.get("Content-Length", "0"))
        remaining = size
        body = bytearray()
        while remaining:
            chunk = self.rfile.read(min(remaining, 64 * 1024))
            if not chunk:
                break
            body.extend(chunk)
            remaining -= len(chunk)
        stack_depth: int | None = None
        if remaining == 0:
            try:
                stack_depth = max_pprof_stack_depth(bytes(body))
            except (OSError, ValueError):
                pass
        COUNTERS.add_profile(
            size - remaining,
            incomplete=remaining != 0,
            stack_depth=stack_depth,
        )
        self._reply(HTTPStatus.OK, b"")

    def do_GET(self) -> None:
        parsed = urlsplit(self.path)
        if parsed.path == "/io":
            params = parse_qs(parsed.query)
            delay_ms = float(params.get("delay_ms", ["1"])[0])
            time.sleep(max(0.0, delay_ms) / 1_000)
            COUNTERS.add_io()
            self._reply(HTTPStatus.OK, b"ok")
            return
        if parsed.path == "/stats":
            body = json.dumps(COUNTERS.snapshot(), sort_keys=True).encode()
            self._reply(HTTPStatus.OK, body, "application/json")
            return
        if parsed.path == "/health":
            self._reply(HTTPStatus.OK, b"ok")
            return
        self._reply(HTTPStatus.NOT_FOUND, b"not found")

    def _reply(self, status: HTTPStatus, body: bytes, content_type: str = "text/plain") -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format: str, *_args: object) -> None:
        return


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=4040)
    args = parser.parse_args()
    CollectorServer((args.host, args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()

