# Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-present Datadog, Inc.

"""Loopback-only HTTP API. No telemetry or credentials leave this process."""
import argparse
import json
import logging
import math
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

EPOCH = 1_780_000_000
MAX_BODY = 2_000_000
LOG = logging.getLogger("reflex_toto")


def integer(value):
    return type(value) is int


def finite(value):
    return type(value) in (int, float) and math.isfinite(value) and abs(value) <= 3.4028234e38


def validate_input(data):
    if not isinstance(data, dict):
        raise ValueError("Expected a JSON object")
    origin, interval = data.get("origin_ms"), data.get("interval_ms", 1000)
    horizon, timestamps, values = (data.get(k) for k in ("prediction_length", "timestamps", "values"))
    if not integer(origin) or not 0 <= origin <= 2**63 - 1 or origin % 1000:
        raise ValueError("origin_ms must be nonnegative and aligned to seconds")
    if not integer(interval) or interval not in (1000, 10000):
        raise ValueError("interval_ms must be 1000 or 10000")
    if not integer(horizon) or not 1 <= horizon <= 120:
        raise ValueError("prediction_length must be between 1 and 120")
    if not isinstance(values, list) or not (64 if interval == 1000 else 32) <= len(values) <= 8192:
        raise ValueError("Invalid observation history length")
    if any(not isinstance(row, list) or len(row) != 3 or not all(map(finite, row)) for row in values):
        raise ValueError("Expected three finite, aligned observation series")
    if not isinstance(timestamps, list) or len(timestamps) != len(values) or not all(map(integer, timestamps)):
        raise ValueError("Invalid timestamps")
    if timestamps[-1] != EPOCH + origin // 1000 or any(
        b - a != interval // 1000 for a, b in zip(timestamps, timestamps[1:])
    ):
        raise ValueError("History must end at its origin on the declared interval grid")
    return origin, interval, horizon, values


def validate_series(series, horizon):
    if len(series) != 3:
        raise ValueError("Expected three forecast series")
    for s in series:
        if any(len(s[k]) != horizon for k in ("lower", "median", "upper")):
            raise ValueError("Incomplete forecast arrays")
        for lo, mid, hi in zip(s["lower"], s["median"], s["upper"]):
            if not all(map(finite, (lo, mid, hi))) or not lo <= mid <= hi:
                raise ValueError("Non-finite or crossing quantiles")


class Service(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address, model, queue_timeout=8):
        self.model = model
        self.queue_timeout = queue_timeout
        self.slots = threading.BoundedSemaphore(5)  # One running request, at most four waiting.
        self.inference = threading.Lock()
        super().__init__(address, Handler)


class Handler(BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(10)

    def log_message(self, *_):
        pass  # Do not log observation bodies or query strings.

    def reply(self, status, body):
        encoded = json.dumps(body, allow_nan=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        try:
            self.wfile.write(encoded)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_GET(self):
        if self.path != "/health":
            return self.reply(404, {"error": "Not found"})
        self.reply(200, {"status": "ready", "model_provenance": self.server.model.provenance,
                         "busy": self.server.inference.locked()})

    def do_POST(self):
        if self.headers.get("Origin"):
            return self.reply(403, {"error": "Use the simulator server to request forecasts"})
        if self.path != "/forecast":
            return self.reply(404, {"error": "Not found"})
        if self.headers.get_content_type() != "application/json":
            return self.reply(415, {"error": "Expected application/json"})
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= MAX_BODY or self.headers.get("Transfer-Encoding"):
                raise ValueError()
        except ValueError:
            return self.reply(413, {"error": "Expected a bounded Content-Length"})
        if not self.server.slots.acquire(blocking=False):
            return self.reply(429, {"error": "Forecast queue is full"})
        started = time.perf_counter()
        request_id = str(uuid.uuid4())
        try:
            try:
                data = json.loads(self.rfile.read(length))
                origin, interval, horizon, values = validate_input(data)
            except (ValueError, OverflowError, RecursionError):
                return self.reply(422, {"error": "Invalid forecast input"})
            if not self.server.inference.acquire(timeout=self.server.queue_timeout):
                return self.reply(503, {"error": "Forecast queue deadline exceeded"})
            try:
                series = self.server.model.predict(values, horizon)
                validate_series(series, horizon)
            except Exception:
                LOG.exception("Forecast failed: request_id=%s", request_id)
                return self.reply(500, {"error": "Toto inference failed", "request_id": request_id})
            finally:
                self.server.inference.release()
            latency_ms = (time.perf_counter() - started) * 1000
            self.reply(200, {"origin_ms": origin, "interval_ms": interval,
                             "request_id": request_id, "source": "local_toto",
                             "model_provenance": self.server.model.provenance,
                             "quantiles": [0.1, 0.5, 0.9], "series": series,
                             "latency_ms": latency_ms})
            LOG.info("Forecast complete: request_id=%s samples=%d horizon=%d latency_ms=%.1f",
                     request_id, len(values), horizon, latency_ms)
        finally:
            self.server.slots.release()


def main():
    from reflex_toto.model import DEFAULT_MODEL, DEFAULT_REVISION, Toto

    parser = argparse.ArgumentParser(description="Run Toto locally for the Reflex playground")
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument("--revision", help="Hugging Face revision; defaults to a pinned commit for the default model")
    parser.add_argument("--device", choices=["cpu", "cuda"], default="cpu")
    parser.add_argument("--threads", type=int, default=4)
    args = parser.parse_args()
    if not 1 <= args.threads <= 64 or not 0 <= args.port <= 65535:
        parser.error("Invalid thread count or port")
    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(message)s")
    revision = args.revision or (DEFAULT_REVISION if args.model == DEFAULT_MODEL else "main")
    LOG.info("Loading %s at %s (first start downloads public model weights)", args.model, revision)
    model = Toto(args.model, revision, args.device, args.threads)
    # Readiness means both loading and a real forward pass succeeded.
    validate_series(model.predict([[1., 2., 3.]] * 64, 12), 12)
    with Service(("127.0.0.1", args.port), model) as server:
        LOG.info("Ready: http://127.0.0.1:%d — %s", server.server_port, model.provenance)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
