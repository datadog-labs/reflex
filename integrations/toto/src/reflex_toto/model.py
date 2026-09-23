# Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-present Datadog, Inc.

import os
from importlib.metadata import version

os.environ.setdefault("HF_HUB_DISABLE_TELEMETRY", "1")

DEFAULT_MODEL = "Datadog/Toto-2.0-22m"
DEFAULT_REVISION = "685e4ae3e2be8d8998025e53dd98e7fdcb296a89"


class Toto:
    def __init__(self, model_id, revision, device="cpu", threads=4):
        import torch
        from huggingface_hub import snapshot_download
        from toto2 import Toto2Model

        torch.set_num_threads(threads)
        # Resolve once to a concrete local snapshot. Do not execute remote model code.
        path = snapshot_download(
            model_id, revision=revision, token=False,
            allow_patterns=["*.json", "*.safetensors"],
        )
        self.torch = torch
        self.device = torch.device(device)
        self.model = Toto2Model.from_pretrained(path).to(self.device).eval()
        resolved = path.rsplit("/", 1)[-1]
        self.provenance = f"{model_id}@{resolved}; toto-2={version('toto-2')}; device={device}"

    def predict(self, values, horizon):
        torch = self.torch
        with torch.inference_mode():
            target = torch.tensor(values, dtype=torch.float32, device=self.device).T.unsqueeze(0)
            if len(values) < 32:
                raise ValueError("Toto requires at least 32 observed samples")
            # Preserve the forecast origin. Left padding is missing, never observed zero.
            padding = (-len(values)) % self.model.config.patch_size
            mask = torch.ones_like(target, dtype=torch.bool)
            if padding:
                target = torch.nn.functional.pad(target, (padding, 0))
                mask = torch.nn.functional.pad(mask, (padding, 0), value=False)
            result = self.model.forecast(
                {
                    "target": target,
                    "target_mask": mask,
                    "series_ids": torch.zeros((1, 3), dtype=torch.long, device=self.device),
                },
                horizon=horizon,
                decode_block_size=None,
                has_missing_values=bool(padding),
            )
            if tuple(result.shape) != (9, 1, 3, horizon):
                raise ValueError("Unexpected Toto output dimensions")
            result = result.detach().float().cpu()
            return [
                {"lower": result[0, 0, i].tolist(), "median": result[4, 0, i].tolist(),
                 "upper": result[8, 0, i].tolist()}
                for i in range(3)
            ]
