#!/usr/bin/env python3
"""Makes the network behind Sing: Open-Unmix UMX-HQ's vocals model without its weights, which the app carries, and the
recipe the app follows to fill it from the authors' own checkpoint.

Open-Unmix (Stöter, Uhlich, Liutkus and Mitsufuji, JOSS 2019; https://github.com/sigsep/open-unmix-pytorch) publishes
its code and the UMX-HQ weights (Zenodo 3370489, Inria) under the MIT licence. nori ships no copy of the weights and
hosts none: with Sing on, the core fetches `vocals-b62c91ce.pth` (35.6 MB) from Zenodo, checks its SHA-256, reads it
with the restricted unpickler and writes the weights in the layout this graph expects, once
(crates/player/src/automix/checkpoint.rs, weights.rs; crates/core/src/model_download.rs).

The graph is the model's forward pass up to its mask: input `mag`, the STFT magnitudes of a stereo 44.1 kHz mix
(4096-point Hann window, hop 1024) cut to the model's 1487 bins, [frames, 2, 1487]; output `mask`, relu(...) before the
model multiplies it with the mix, [frames, 2, 2049]. That ratio is what Open-Unmix's separator applies to the mix
(one target, no EM steps), so the app computes its STFT itself and keeps only the mask. The three bidirectional LSTM
layers export as ONNX `LSTM` nodes, which tract runs (to 2e-6 of PyTorch). Then, as for Beat This!
(tools/beat-this/export.py, whose helpers this uses):

- Weights of 1024 values or more are stored as fp16 with a Cast back to fp32 (17.8 MB instead of 35.6 MB).
- Every weight leaves the file: `copy` (the "m." prefix stripped), `transpose` (a Linear folded into MatMul) or
  `lstm` (the comma-separated state_dict tensors in `nori.from`, each with its gate blocks reordered from PyTorch's
  input, forget, cell, output to ONNX's input, output, forget, cell, then joined in order: the forward and reverse
  weights of a layer, or its four biases).

The weights file's size and SHA-256 are printed: the pins in crates/automix/src/beat_model.rs. A fixed test input's output
is printed too, the reference crates/player sing::model's ignored test compares against.

    uv venv -p 3.12 /tmp/umx && uv pip install -p /tmp/umx/bin/python torch==2.8.0 torchaudio==2.8.0 onnx==1.19.0 \\
        onnxruntime numpy openunmix==1.3.0
    /tmp/umx/bin/python tools/umx/export.py --out crates/player/models/umx-hq-vocals.graph.onnx
    NORI_UMX_CKPT=vocals-b62c91ce.pth cargo test --release -p nori-player --features neural-beats umx -- --ignored --nocapture
"""
import argparse
import hashlib
import importlib.util
import sys
import tempfile
import urllib.request
from pathlib import Path

import numpy as np

CHECKPOINT_URL = "https://zenodo.org/records/3370489/files/vocals-b62c91ce.pth"
CHECKPOINT_SHA256 = "b62c91cedbc7a066f1778ead5b5cecb377aa3a46a31af1cce7c5c8769339d083"
# The name the graph gives the weights file; crates/automix/src/beat_model.rs keeps it under the same name.
WEIGHTS_FILE = "umx-hq-vocals.weights"
BINS, OUT_BINS, LAYERS = 1487, 2049, 3
# Frames per model run in the app (crates/player/src/sing/model.rs: a window and its context).
CHUNK = 640


def beat_this_tools():
    spec = importlib.util.spec_from_file_location("beat_this_export", Path(__file__).parent.parent / "beat-this" / "export.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_model(checkpoint: Path):
    import torch
    from openunmix.model import OpenUnmix

    sd = torch.load(checkpoint, map_location="cpu", weights_only=True)
    # As openunmix.umxhq_spec builds it: 16 kHz of the 4096-point spectrum go in.
    model = OpenUnmix(nb_bins=OUT_BINS, nb_channels=2, hidden_size=512, max_bin=BINS)
    model.load_state_dict(sd, strict=False)
    return model.eval()


def mask_module(model):
    import torch

    class Mask(torch.nn.Module):
        """OpenUnmix.forward up to the mask, on [frames, 2, 1487] magnitudes (one batch)."""

        def __init__(self, m):
            super().__init__()
            self.m = m

        def forward(self, x):
            m, frames = self.m, x.shape[0]
            x = (x + m.input_mean) * m.input_scale
            x = torch.tanh(m.bn1(m.fc1(x.reshape(frames, 2 * BINS))))
            y = m.lstm(x.reshape(frames, 1, 512))[0].reshape(frames, 512)
            x = torch.relu(m.bn2(m.fc2(torch.cat([x, y], -1))))
            x = m.bn3(m.fc3(x)).reshape(frames, 2, OUT_BINS)
            return torch.relu(x * m.output_scale + m.output_mean)

    return Mask(model).eval()


def test_input(frames=64):
    """Magnitudes from a formula the Rust test computes too: 0.5 + 0.4 sin(0.11 t + 0.037 k + 1.3 c) for `frames`
    frames, then silence to a chunk."""
    t, c, k = np.meshgrid(np.arange(frames), np.arange(2), np.arange(BINS), indexing="ij")
    x = (0.5 + 0.4 * np.sin(0.11 * t + 0.037 * k + 1.3 * c)).astype(np.float32)
    return np.concatenate([x, np.zeros((CHUNK - frames, 2, BINS), np.float32)])


def umx_candidates(sd):
    """The recipes that may make one of the graph's initializers: a copy, a transpose, or an LSTM layer's parts."""
    lstm = []
    for layer in range(LAYERS):
        for part in ("weight_ih", "weight_hh"):
            lstm.append(f"lstm.{part}_l{layer},lstm.{part}_l{layer}_reverse")
        lstm.append(f"lstm.bias_ih_l{layer},lstm.bias_hh_l{layer},lstm.bias_ih_l{layer}_reverse,lstm.bias_hh_l{layer}_reverse")

    def candidates(name, want):
        # to_fp16_weights renamed what it stored as fp16.
        name = name.removesuffix("__q")
        if name.startswith("m.") and name[2:] in sd:
            return [{"nori.op": "copy", "nori.from": name[2:]}]
        found = [{"nori.op": "transpose", "nori.from": k} for k, v in sd.items() if v.ndim == 2 and v.T.shape == want.shape]
        found += [{"nori.op": "lstm", "nori.from": keys} for keys in lstm if sum(sd[k].size for k in keys.split(",")) == want.size]
        return found

    return candidates


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--checkpoint", type=Path, help="vocals-b62c91ce.pth, if already downloaded")
    ap.add_argument("--out", required=True, type=Path, help="the graph, weights left out")
    args = ap.parse_args()

    import onnxruntime as ort
    import torch

    tools = beat_this_tools()
    with tempfile.TemporaryDirectory() as tmp:
        ckpt = args.checkpoint or Path(tmp) / "vocals-b62c91ce.pth"
        if not args.checkpoint:
            print("fetching", CHECKPOINT_URL)
            urllib.request.urlretrieve(CHECKPOINT_URL, ckpt)
        if tools.sha256(ckpt) != CHECKPOINT_SHA256:
            sys.exit(f"{ckpt} is not the published UMX-HQ vocals checkpoint (SHA-256 {tools.sha256(ckpt)})")
        model = load_model(ckpt)
        sd = {k: v.numpy() for k, v in model.state_dict().items() if v.dtype == torch.float32}
        fp32, fp16 = Path(tmp) / "umx-fp32.onnx", Path(tmp) / "umx-fp16.onnx"
        net = mask_module(model)
        x = test_input()
        with torch.no_grad():
            torch.onnx.export(net, torch.from_numpy(x), str(fp32), input_names=["mag"], output_names=["mask"],
                              dynamic_axes={"mag": {0: "frames"}, "mask": {0: "frames"}}, opset_version=17, dynamo=False)
            ref = net(torch.from_numpy(x)).numpy()
        tools.to_fp16_weights(fp32, fp16)
        so = ort.SessionOptions()
        so.intra_op_num_threads = 1
        got = ort.InferenceSession(str(fp16), so, providers=["CPUExecutionProvider"]).run(None, {"mag": x})[0]
        diff = float(np.abs(got - ref).max())
        print(f"fp16 export against PyTorch: largest mask difference {diff:.5f}")
        blob, differ, total = tools.split(fp16, args.out, sd, umx_candidates(sd), WEIGHTS_FILE)
    print(f"{differ} of {total} weights differ from PyTorch's")
    print(f"{args.out}: {args.out.stat().st_size} bytes, SHA-256 {tools.sha256(args.out)}")
    print(f"{WEIGHTS_FILE}: {len(blob)} bytes, SHA-256 {hashlib.sha256(blob).hexdigest()}")
    # The reference for the Rust test: the fp16 graph's mask at a few places, and its mean.
    places = [(0, 0, 0), (5, 1, 100), (17, 0, 500), (33, 1, 1000), (48, 0, 1486), (63, 1, 2048)]
    print("reference mean", float(got.mean()), "at", [(p, float(got[p])) for p in places])
    sys.exit(0 if diff < 0.02 else 1)


if __name__ == "__main__":
    main()
