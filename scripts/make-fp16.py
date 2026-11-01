# /// script
# requires-python = ">=3.10"
# dependencies = ["onnx==1.23.2", "onnxruntime==1.31.0"]
# ///
"""Makes an fp16 copy of a sherpa-onnx NeMo transducer (encoder, decoder, joiner) for the GPU.

Halves disk and VRAM where no fp16 export is published (Parakeet TDT v3, Parakeet Unified), for GPUs short of
memory. Inputs and outputs stay fp32, so the app feeds it exactly as before. On an RTX 3090 the result keeps
the same WER but is ~50 ms slower per line, so fp32 stays the default there. Check with `examples/footprint`.

Usage: uv run scripts/make-fp16.py <fp32 model folder> <new fp16 folder>
With these pinned versions the output is byte for byte the catalog's, so CI can make and publish it.
"""

import shutil
import sys
from pathlib import Path

import onnx
from onnxruntime.transformers.float16 import convert_float_to_float16


def main(source: Path, target: Path) -> None:
    target.mkdir(parents=True, exist_ok=True)
    for part in ("encoder", "decoder", "joiner"):
        # Types must be known to convert cleanly. Inferring them on the file works past the 2 GB limit
        # that in-memory inference hits with the encoder's external weights.
        typed = source / f"{part}.typed.onnx"
        onnx.shape_inference.infer_shapes_path(str(source / f"{part}.onnx"), str(typed))
        model = onnx.load(str(typed))
        typed.unlink()
        converted = convert_float_to_float16(model, keep_io_types=True, disable_shape_infer=True)
        onnx.save(converted, str(target / f"{part}.fp16.onnx"))
        print(f"{part}: {(target / f'{part}.fp16.onnx').stat().st_size / 1e6:.0f} MB")
    shutil.copy(source / "tokens.txt", target / "tokens.txt")


if __name__ == "__main__":
    main(Path(sys.argv[1]), Path(sys.argv[2]))
