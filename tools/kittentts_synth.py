#!/usr/bin/env python3
"""
KittenTTS synthesis helper script.

Runs KittenTTS synthesis in-process so the ONNX model is unloaded after use.
This keeps memory usage low for a long-running agent process.

Usage:
    python kittentts_synth.py --text "Hello world" --out /tmp/out.wav \
        --voice Bruno --speed 0.95
"""
import argparse
import os
import sys
import warnings

# Suppress HF Hub unauthenticated warnings
os.environ["HF_HUB_DISABLE_SYMLINKS_WARNING"] = "1"
os.environ["HF_HUB_DISABLE_EXPERIMENTAL_WARNING"] = "1"

def main():
    parser = argparse.ArgumentParser(description="KittenTTS synthesis helper")
    parser.add_argument("--text", required=True, help="Text to synthesize")
    parser.add_argument("--out", required=True, help="Output WAV path")
    parser.add_argument("--voice", default="Bruno", help="Voice name (Bella/Jasper/Luna/Bruno/Rosie/Hugo/Kiki/Leo)")
    parser.add_argument("--speed", default="0.95", help="Speech speed (0.5-2.0)")
    args = parser.parse_args()

    # Change to a tmpdir to avoid polluting the working directory with HF cache
    orig_cwd = os.getcwd()
    tmpdir = "/tmp"
    try:
        os.chdir(tmpdir)

        from kittentts import KittenTTS

        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            ktts = KittenTTS()
            ktts.generate_to_file(
                text=args.text,
                output_path=args.out,
                voice=args.voice,
                speed=float(args.speed),
            )
    finally:
        os.chdir(orig_cwd)

    # Verify output
    if not os.path.exists(args.out):
        print(f"ERROR: Output file not created: {args.out}", file=sys.stderr)
        sys.exit(1)
    size = os.path.getsize(args.out)
    print(f"OK: Audio saved to {args.out} ({size} bytes)")

if __name__ == "__main__":
    main()
