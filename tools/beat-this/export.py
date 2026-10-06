#!/usr/bin/env python3
"""Makes the network behind "Better beat detection": Beat This!'s graph without its weights, which the app carries,
and the recipe the app follows to fill it from the authors' own checkpoint.

Beat This! (Foscarin, Schlüter and Widmer, ISMIR 2024; https://github.com/CPJKU/beat_this) publishes its code and
weights under the MIT licence. nori ships no copy of the weights and hosts none: with the switch on, the core fetches
the `small0` checkpoint (2.1 M parameters, 8.1 MB) from the authors' server, checks its SHA-256, reads it with a
restricted unpickler and writes the weights in the layout this graph expects, once (crates/player/src/automix/
checkpoint.rs, weights.rs; crates/core/src/model_download.rs). This script makes the graph that conversion fills.

The model is exported with the repository's own code for one 30 s window (1512 frames of 128 mel bands: 1500 and a
border of 6 on each side) with both outputs as logits. Then, each checked:

- Each attention the exporter spells out (q and k scaled, MatMul, Softmax, MatMul with v) becomes one ONNX
  `Attention` node, which tract runs as flash attention: the spelled-out form holds every 1512 x 1512 score matrix
  of 32 frequency rows at once, 700 MB at the peak, the fused one about 100 MB. `Attention` belongs to opset 23 while
  the rest of the file is opset 17, so onnxruntime will not open the result; tract, which reads operators by name,
  does, and the app only uses tract.
- The weights are stored as fp16 with a Cast back to fp32 in front of each: half the size, and tract still computes
  in fp32 (logits move by about 0.01; no frame changes side of zero on the check below). Dynamic int8 does not load
  in tract, and BitChord's int8 small export misbehaves; see docs/research/analysis.md.
- Every weight then leaves the file (`split`): each initializer becomes external data at its place in the weights
  file (`location`, `offset`, `length`, as ONNX has it), plus how to make it from the checkpoint's state_dict, the
  "model." prefix stripped as beat_this does: `nori.op` is `copy`, `transpose` (a Linear's weight, as the exporter
  folds it into MatMul), `conv_bn` (a Conv2d's weight with the BatchNorm2d after it folded in, as the exporter does:
  w * gamma / sqrt(var + eps) per output channel) or `bn_shift` (the bias that fold leaves: beta - mean * gamma /
  sqrt(var + eps)), `nori.from` the state_dict key (for `bn_shift` the BatchNorm's), `nori.bn` the BatchNorm and
  `nori.eps` its epsilon; the initializer's own type says fp32 or fp16. Every value name is shortened (they were
  most of the file). What remains is the graph, about 0.1 MB, with nothing of the weights in it.

The recipe is followed here in numpy, in float32 arithmetic step for step as the Rust conversion does it, and the
weights file's size and SHA-256 are printed: the pins in crates/automix/src/beat_model.rs. PyTorch folds with its own
square root, which is not always correctly rounded, so a few folded values differ from the exporter's by one unit in
the last place; the script prints how many, and the Rust test compares the logits (crates/player automix::weights).

    python3.13 -m venv /tmp/bt && /tmp/bt/bin/pip install torch==2.8.0 --index-url https://download.pytorch.org/whl/cpu
    /tmp/bt/bin/pip install onnx==1.19.0 onnxruntime numpy einops==0.8.0 rotary-embedding-torch==0.6.4
    curl -L https://github.com/CPJKU/beat_this/archive/b95c8ab0c58c.tar.gz | tar xz -C /tmp
    /tmp/bt/bin/python tools/beat-this/export.py --beat-this /tmp/beat_this-b95c8ab0c58c* \\
        --out crates/player/models/beat-this-small0.graph.onnx --full /tmp/beat-this-small0-v1.onnx

rotary-embedding-torch is the version beat_this's requirements.txt names: 0.9.1 traces a different graph (a file
1.2 MB bigger). torch 2.8.0 has no wheel for Python 3.14. The checkpoint is fetched from the authors' server and
checked against its SHA-256 (or given with --checkpoint). The fp16 export (before the attention is fused) is compared
with the PyTorch model on a fixed test input. `--full` also writes the whole fused model with PyTorch's own weights
(5,069,707 bytes, SHA-256 847b51aa...: the file the app shipped until the weights left it), which the equality test
and the evaluation read:

    NORI_BEAT_THIS_CKPT=small0.ckpt NORI_BEAT_THIS=/tmp/beat-this-small0-v1.onnx \\
        cargo test --release -p nori-player --features neural-beats official_weights -- --nocapture
    NORI_BEAT_THIS_CKPT=small0.ckpt cargo test --release -p nori-engine --features neural-beats --test core
    NORI_BEAT_THIS=/tmp/beat-this-small0-v1.onnx NORI_BEAT_THIS_REF=<the full model, final0, as ONNX> \\
        cargo test --release -p nori-player --features neural-beats neural_eval -- --ignored --nocapture
"""
import argparse
import hashlib
import sys
import tempfile
import urllib.request
from pathlib import Path

import numpy as np

CHECKPOINT_URL = "https://cloud.cp.jku.at/public.php/dav/files/7ik4RrBKTS273gp/small0.ckpt"
CHECKPOINT_SHA256 = "6074be2c4d490c5f6101fcc374a1ec72ae93456e23bb6019783b849f5dc7d47b"
# The name the graph gives the weights file; crates/automix/src/beat_model.rs keeps it under the same name.
WEIGHTS_FILE = "beat-this-small0.weights"
FRAMES, MELS = 1512, 128


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_model(repo: Path, checkpoint: Path):
    import inspect

    import torch

    sys.path.insert(0, str(repo))
    from beat_this.model.beat_tracker import BeatThis
    from beat_this.utils import replace_state_dict_key

    ckpt = torch.load(checkpoint, map_location="cpu", weights_only=True)
    # As beat_this.inference.load_model does, without its audio dependencies: the small model's own sizes.
    hparams = {k: v for k, v in ckpt["hyper_parameters"].items() if k in set(inspect.signature(BeatThis).parameters)}
    model = BeatThis(**hparams)
    model.load_state_dict(replace_state_dict_key(ckpt["state_dict"], "model.", ""))
    return model.eval(), hparams


def export(model, path: Path):
    import torch

    class Logits(torch.nn.Module):
        def __init__(self, m):
            super().__init__()
            self.m = m

        def forward(self, spect):
            out = self.m(spect)
            return out["beat"], out["downbeat"]

    with torch.no_grad():
        # The frame count stays open, so the harness can also run the file over other chunk lengths.
        frames = {"spect": {1: "frames"}, "beat": {1: "frames"}, "downbeat": {1: "frames"}}
        torch.onnx.export(
            Logits(model), torch.zeros(1, FRAMES, MELS), str(path), input_names=["spect"],
            output_names=["beat", "downbeat"], dynamic_axes=frames, opset_version=17, dynamo=False,
        )


def to_fp16_weights(src: Path, dst: Path):
    """Every float weight of 1024 values or more stored as fp16, followed by a Cast to fp32 under its old name."""
    import onnx
    from onnx import TensorProto, helper, numpy_helper

    m = onnx.load(str(src))
    g = m.graph
    inits, casts = [], []
    for t in g.initializer:
        a = numpy_helper.to_array(t)
        if t.data_type != TensorProto.FLOAT or a.size < 1024:
            inits.append(t)
            continue
        inits.append(numpy_helper.from_array(a.astype(np.float16), t.name + "__q"))
        casts.append(helper.make_node("Cast", [t.name + "__q"], [t.name], to=TensorProto.FLOAT))
    del g.initializer[:]
    g.initializer.extend(inits)
    nodes = casts + list(g.node)
    del g.node[:]
    g.node.extend(nodes)
    onnx.checker.check_model(m)
    onnx.save(m, str(dst))


def fuse_attention(src: Path, dst: Path, head_dim: int = 32):
    """Each spelled-out attention - Softmax(MatMul(q * sqrt(s), Transpose(k) * sqrt(s))) read by one MatMul with v -
    becomes Attention(q, k, v, scale = s), s = 1 / sqrt(head_dim); what only computed the scaling goes. Every
    pattern must match exactly, or nothing is written."""
    import onnx
    from onnx import helper

    m = onnx.load(str(src))
    g = m.graph
    prod = {o: n for n in g.node for o in n.output}
    readers = {}
    for n in g.node:
        for i in n.input:
            readers.setdefault(i, []).append(n)
    gone, fused = set(), {}
    softmaxes = [n for n in g.node if n.op_type == "Softmax"]
    for sm in softmaxes:
        scores = prod[sm.input[0]]
        after = readers[sm.output[0]]
        if scores.op_type != "MatMul" or len(after) != 1 or after[0].op_type != "MatMul":
            sys.exit(f"{sm.name}: not the attention pattern")
        mq, mk = prod[scores.input[0]], prod[scores.input[1]]
        tk = prod.get(mk.input[0])
        perm = [list(helper.get_attribute_value(a)) for a in tk.attribute if a.name == "perm"] if tk else []
        is_transpose = tk is not None and tk.op_type == "Transpose" and perm == [[0, 1, 3, 2]]
        if mq.op_type != "Mul" or mk.op_type != "Mul" or not is_transpose:
            sys.exit(f"{sm.name}: not the attention pattern")
        out = after[0]
        fused[out.output[0]] = helper.make_node(
            "Attention", [mq.input[0], tk.input[0], out.input[1]], [out.output[0]], name=sm.name + ".fused",
            scale=float(1.0 / np.sqrt(head_dim)),
        )
        gone.update(id(n) for n in (sm, scores, out, mq, mk, tk))
    nodes = []
    for n in g.node:
        if id(n) in gone:
            if n.output[0] in fused:
                nodes.append(fused[n.output[0]])
            continue
        nodes.append(n)
    outputs = {o.name for o in g.output}
    while True:
        read = {i for n in nodes for i in n.input} | outputs
        kept = [n for n in nodes if any(o in read for o in n.output)]
        if len(kept) == len(nodes):
            break
        nodes = kept
    del g.node[:]
    g.node.extend(nodes)
    onnx.save(m, str(dst))
    print(f"{len(softmaxes)} attentions fused")


def conv_bn_pairs(model):
    """Each Conv2d followed directly by a BatchNorm2d in a Sequential, as the exporter folds them: (conv, bn, eps)."""
    import torch

    pairs = []
    for name, mod in model.named_modules():
        if not isinstance(mod, torch.nn.Sequential):
            continue
        kids = list(mod.named_children())
        for (cn, c), (bn, b) in zip(kids, kids[1:]):
            if isinstance(c, torch.nn.Conv2d) and isinstance(b, torch.nn.BatchNorm2d):
                if c.bias is not None:
                    sys.exit(f"{name}.{cn} has a bias; the recipe folds only bias-free convolutions")
                pre = f"{name}." if name else ""
                pairs.append((pre + cn, pre + bn, float(b.eps)))
    return pairs


def bn_scale(sd, bn, eps):
    """gamma / sqrt(var + eps) in float32, one rounding per step, as the Rust conversion computes it."""
    var = sd[bn + ".running_var"]
    return sd[bn + ".weight"] / np.sqrt(var + np.float32(eps))


def lstm_gates(a):
    """A PyTorch LSTM tensor's gate blocks (input, forget, cell, output) in ONNX's order (input, output, forget,
    cell), as the exporter reorders them."""
    i, f, c, o = np.split(a, 4)
    return np.concatenate([i, o, f, c])


def follow(recipe, sd, dtype, dims):
    """One initializer's values made from the state_dict by its recipe (see the module's docs)."""
    op, src = recipe["nori.op"], recipe["nori.from"]
    if op == "copy":
        a = sd[src]
    elif op == "transpose":
        a = np.ascontiguousarray(sd[src].T)
    elif op == "conv_bn":
        s = bn_scale(sd, recipe["nori.bn"], float(recipe["nori.eps"]))
        a = sd[src] * s.reshape(-1, *([1] * (sd[src].ndim - 1)))
    elif op == "bn_shift":
        s = bn_scale(sd, recipe["nori.bn"], float(recipe["nori.eps"]))
        a = sd[src + ".bias"] - sd[src + ".running_mean"] * s
    elif op == "lstm":
        a = np.concatenate([lstm_gates(sd[k]).ravel() for k in src.split(",")]).reshape(dims)
    else:
        raise ValueError(op)
    if list(a.shape) != list(dims):
        raise ValueError(f"{src}: shape {a.shape}, the graph wants {dims}")
    return a.astype(dtype)


def beat_this_candidates(sd, pairs):
    """The recipes that may make one of Beat This!'s initializers: a copy, a transpose, or a Conv2d/BatchNorm fold."""

    def candidates(name, want):
        found = []
        if name.startswith("m.") and name[2:] in sd:
            found.append({"nori.op": "copy", "nori.from": name[2:]})
        else:
            for k, v in sd.items():
                if v.ndim == 2 and v.T.shape == want.shape:
                    found.append({"nori.op": "transpose", "nori.from": k})
            for conv, bn, eps in pairs:
                e = repr(eps)
                if sd[conv + ".weight"].shape == want.shape:
                    found.append({"nori.op": "conv_bn", "nori.from": conv + ".weight", "nori.bn": bn, "nori.eps": e})
                if sd[bn + ".bias"].shape == want.shape:
                    found.append({"nori.op": "bn_shift", "nori.from": bn, "nori.bn": bn, "nori.eps": e})
        return found

    return candidates


def split(src: Path, dst: Path, sd, candidates, weights_file=WEIGHTS_FILE):
    """The model's weights out, as external data in `weights_file` with a recipe each (`candidates(name, values)`
    lists those that may make an initializer); value names shortened. Returns the weights file's bytes as numpy makes
    them from the recipe, and how many values differ from PyTorch's."""
    import onnx
    from onnx import TensorProto, numpy_helper

    m = onnx.load(str(src))
    g = m.graph
    blob, differ, total = bytearray(), 0, 0
    for t in g.initializer:
        want = numpy_helper.to_array(t)
        dtype = {TensorProto.FLOAT: np.float32, TensorProto.FLOAT16: np.float16}[t.data_type]
        # The one whose values are PyTorch's: exactly for a copy, a transpose or a reorder, within float rounding
        # for a fold.
        scored = []
        for r in candidates(t.name, want):
            got = follow(r, sd, dtype, want.shape)
            diff = float(np.abs(got.astype(np.float64) - want.astype(np.float64)).max())
            if diff <= 1e-3 * max(float(np.abs(want).max()), 1e-6) and (r["nori.op"] in ("conv_bn", "bn_shift") or diff == 0):
                scored.append((diff, r, got))
        if len(scored) != 1:
            sys.exit(f"{t.name}: {len(scored)} ways to make it from the checkpoint")
        _, recipe, got = scored[0]
        differ += int((got != want).sum())
        total += want.size
        data = got.tobytes()
        entries = {"location": weights_file, "offset": str(len(blob)), "length": str(len(data)), **recipe}
        blob += data
        t.ClearField("raw_data")
        t.data_location = TensorProto.EXTERNAL
        del t.external_data[:]
        for k, v in entries.items():
            e = t.external_data.add()
            e.key, e.value = k, v
    # Short names: every value but the graph's inputs and outputs, in the order they appear; no node names.
    keep = {v.name for v in g.input} | {v.name for v in g.output}
    names = {}

    def short(n):
        if n == "" or n in keep:
            return n
        if n not in names:
            names[n] = format(len(names), "x")
        return names[n]

    for t in g.initializer:
        t.name = short(t.name)
    for n in g.node:
        n.name = ""
        n.input[:] = [short(i) for i in n.input]
        n.output[:] = [short(o) for o in n.output]
    del g.value_info[:]
    biggest = max((a.t.ByteSize() for n in g.node if n.op_type == "Constant" for a in n.attribute if a.name == "value"),
                  default=0)
    print(f"largest constant left in the graph: {biggest} bytes")
    dst.write_bytes(m.SerializeToString())
    return bytes(blob), differ, total


def test_input():
    """A spectrogram-like input: log-mel values run from 0 (silence) to about 7."""
    rng = np.random.default_rng(0)
    return (rng.random((1, FRAMES, MELS)) * 6.0).astype(np.float32)


def run_onnx(path: Path, x):
    import onnxruntime as ort

    so = ort.SessionOptions()
    so.intra_op_num_threads = 1
    s = ort.InferenceSession(str(path), so, providers=["CPUExecutionProvider"])
    b, d = s.run(None, {s.get_inputs()[0].name: x})
    return b.ravel(), d.ravel()


def compare(name, ref, got):
    diff = max(float(np.abs(ref[0] - got[0]).max()), float(np.abs(ref[1] - got[1]).max()))
    flips = int(((ref[0] > 0) != (got[0] > 0)).sum() + ((ref[1] > 0) != (got[1] > 0)).sum())
    print(f"{name}: largest logit difference {diff:.4f}, frames on the other side of zero {flips}")
    return diff < 0.05 and flips <= 2


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--beat-this", required=True, type=Path, help="a checkout of github.com/CPJKU/beat_this")
    ap.add_argument("--checkpoint", type=Path, help="small0.ckpt, if already downloaded")
    ap.add_argument("--out", required=True, type=Path, help="the graph, weights left out")
    ap.add_argument("--full", type=Path, help="also the whole fused model, with PyTorch's weights in it")
    args = ap.parse_args()

    import torch

    with tempfile.TemporaryDirectory() as tmp:
        ckpt = args.checkpoint or Path(tmp) / "small0.ckpt"
        if not args.checkpoint:
            print("fetching", CHECKPOINT_URL)
            urllib.request.urlretrieve(CHECKPOINT_URL, ckpt)
        if sha256(ckpt) != CHECKPOINT_SHA256:
            sys.exit(f"{ckpt} is not the published small0 checkpoint (SHA-256 {sha256(ckpt)})")
        model, hparams = load_model(args.beat_this, ckpt)
        print("small0:", hparams, sum(p.numel() for p in model.parameters()), "parameters")
        sd = {k: v.numpy() for k, v in model.state_dict().items() if v.dtype == torch.float32}
        pairs = conv_bn_pairs(model)
        fp32, fp16 = Path(tmp) / "small0-fp32.onnx", Path(tmp) / "small0-fp16.onnx"
        full = args.full or Path(tmp) / "small0-full.onnx"
        # Exported before the model has run: a run first leaves the rotary embedding's cache filled, and the file
        # then carries it as constants (180 kB more, the same logits).
        export(model, fp32)
        to_fp16_weights(fp32, fp16)
        # The reference from a model loaded again: tracing leaves the traced sizes in that cache, and the traced
        # model itself then answers differently (logits off by 5); the ONNX file is right.
        model, _ = load_model(args.beat_this, ckpt)
        x = test_input()
        with torch.no_grad():
            out = model(torch.from_numpy(x))
        ref = (out["beat"].numpy().ravel(), out["downbeat"].numpy().ravel())
        ok = compare("fp16 export against PyTorch", ref, run_onnx(fp16, x))
        fuse_attention(fp16, full)
        print(f"{full}: {full.stat().st_size} bytes, SHA-256 {sha256(full)}")
        blob, differ, total = split(full, args.out, sd, beat_this_candidates(sd, pairs))
    print(f"{differ} of {total} weights differ from PyTorch's fold by float rounding")
    print(f"{args.out}: {args.out.stat().st_size} bytes, SHA-256 {sha256(args.out)}")
    print(f"{WEIGHTS_FILE}: {len(blob)} bytes, SHA-256 {hashlib.sha256(blob).hexdigest()}")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
