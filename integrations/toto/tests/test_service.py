# Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-present Datadog, Inc.

from contextlib import closing
from types import SimpleNamespace
import http.client
import json
import threading
import unittest
from reflex_toto.server import EPOCH, Service, validate_input, validate_series


def history(interval=1000):
    count = 64 if interval == 1000 else 32
    origin = count * interval
    return {"origin_ms": origin, "interval_ms": interval, "prediction_length": 12,
            "timestamps": [EPOCH + i * interval // 1000 for i in range(1, count + 1)],
            "values": [[float(i), float(i * 2), float(i * 3)] for i in range(count)]}


class Model:
    provenance = "test-model@fixture"

    def predict(self, values, horizon):
        return [{"lower": [i] * horizon, "median": [i + 1] * horizon,
                 "upper": [i + 2] * horizon} for i in range(3)]


class ContractTests(unittest.TestCase):
    def test_history_grids(self):
        for interval in (1000, 10000):
            data = history(interval)
            self.assertEqual(validate_input(data)[:3], (data["origin_ms"], interval, 12))
        data = history()
        del data["interval_ms"]  # Rust omits the default interval.
        self.assertEqual(validate_input(data)[1], 1000)

    def test_invalid_input(self):
        cases = [None, [], {}, {**history(), "origin_ms": True},
                 {**history(), "prediction_length": 121}, {**history(), "interval_ms": 2000}]
        for mutation in (lambda d: d["timestamps"].__setitem__(0, EPOCH - 1),
                         lambda d: d["values"][0].pop(),
                         lambda d: d["values"][0].__setitem__(0, float("nan")),
                         lambda d: d["values"].pop(),
                         lambda d: d.__setitem__("origin_ms", 65000)):
            data = history()
            mutation(data)
            cases.append(data)
        for data in cases:
            with self.subTest(data=str(data)[:50]), self.assertRaises(ValueError):
                validate_input(data)

    def test_bad_output(self):
        for value in (float("nan"), float("inf"), 20):
            series = Model().predict([], 12)
            series[0]["lower"][0] = value
            with self.assertRaises(ValueError):
                validate_series(series, 12)

    def test_toto_tensor_axes_and_quantile_selection(self):
        import torch
        from reflex_toto.model import Toto
        test = self

        class RecordingModel:
            config = SimpleNamespace(patch_size=32)
            def forecast(self, inputs, **kwargs):
                test.assertEqual(tuple(inputs["target"].shape), (1, 3, 64))
                test.assertEqual(inputs["target"][0, 2, 63].item(), 189)
                test.assertTrue(inputs["target_mask"].all())
                test.assertEqual(tuple(inputs["series_ids"].shape), (1, 3))
                test.assertIsNone(kwargs["decode_block_size"])
                return torch.arange(9.).view(9, 1, 1, 1).expand(9, 1, 3, kwargs["horizon"])

        model = Toto.__new__(Toto)
        model.torch, model.device, model.model = torch, torch.device("cpu"), RecordingModel()
        series = model.predict(history()["values"], 12)
        self.assertEqual(series[2], {"lower": [0.] * 12, "median": [4.] * 12, "upper": [8.] * 12})

    def test_partial_patch_is_missing_and_preserves_last_observation(self):
        import torch
        from reflex_toto.model import Toto
        test = self

        class RecordingModel:
            config = SimpleNamespace(patch_size=32)

            def forecast(self, inputs, **kwargs):
                test.assertEqual(tuple(inputs["target"].shape), (1, 3, 96))
                test.assertFalse(inputs["target_mask"][..., :31].any())
                test.assertTrue(inputs["target_mask"][..., 31:].all())
                test.assertEqual(inputs["target"][0, 2, -1].item(), 192)
                test.assertTrue(kwargs["has_missing_values"])
                return torch.zeros(9, 1, 3, kwargs["horizon"])

        model = Toto.__new__(Toto)
        model.torch, model.device, model.model = torch, torch.device("cpu"), RecordingModel()
        values = history()["values"] + [[64., 128., 192.]]
        validate_series(model.predict(values, 12), 12)
        with self.assertRaises(ValueError):
            model.predict(values[:16], 12)


class HttpTests(unittest.TestCase):
    def setUp(self):
        self.server = Service(("127.0.0.1", 0), Model(), queue_timeout=0.02)
        self.worker = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.worker.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.worker.join()

    def request(self, method, path, body=None, headers=None):
        with closing(http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=2)) as conn:
            conn.request(method, path, json.dumps(body) if body is not None else None,
                         headers or {"Content-Type": "application/json"})
            result = conn.getresponse()
            return result.status, json.loads(result.read())

    def test_roundtrip_both_grids_and_health(self):
        self.assertEqual(self.request("GET", "/health")[1]["status"], "ready")
        for interval in (1000, 10000):
            status, result = self.request("POST", "/forecast", history(interval))
            self.assertEqual(status, 200)
            self.assertEqual(result["origin_ms"], history(interval)["origin_ms"])
            self.assertEqual(result["interval_ms"], interval)
            self.assertEqual(result["quantiles"], [0.1, 0.5, 0.9])
            self.assertEqual(result["source"], "local_toto")
            validate_series(result["series"], 12)

    def test_invalid_http_input(self):
        self.assertEqual(self.request("POST", "/forecast", {})[0], 422)
        self.assertEqual(self.request("POST", "/forecast", history(), {"Content-Type": "text/plain"})[0], 415)
        self.assertEqual(self.request("POST", "/forecast", history(), {"Content-Type": "application/json", "Content-Length": "2000001"})[0], 413)
        self.assertEqual(self.request("GET", "/missing")[0], 404)
        self.assertEqual(self.request("POST", "/forecast", history(),
                         {"Content-Type": "application/json", "Origin": "https://example.com"})[0], 403)

    def test_busy_service_preserves_health_and_bounds_queue(self):
        with self.server.inference:
            self.assertTrue(self.request("GET", "/health")[1]["busy"])
            self.assertEqual(self.request("POST", "/forecast", history())[0], 503)
        for _ in range(5):
            self.server.slots.acquire()
        try:
            self.assertEqual(self.request("POST", "/forecast", history())[0], 429)
        finally:
            for _ in range(5):
                self.server.slots.release()
        self.assertEqual(self.request("POST", "/forecast", history())[0], 200)

    def test_inference_failure_releases_capacity(self):
        self.server.model.predict = lambda *_: (_ for _ in ()).throw(RuntimeError("test failure"))
        with self.assertLogs("reflex_toto", level="ERROR"):
            self.assertEqual(self.request("POST", "/forecast", history())[0], 500)
        self.assertFalse(self.server.inference.locked())
        self.server.model = Model()
        self.assertEqual(self.request("POST", "/forecast", history())[0], 200)


if __name__ == "__main__":
    unittest.main()
