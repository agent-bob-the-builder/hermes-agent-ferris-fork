import pathlib
import sys

import pyperf

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

from agent.context_compressor import ContextCompressor

compressor = ContextCompressor(
    model="openai/gpt-5.4",
    quiet_mode=True,
    provider="openai",
    base_url="https://api.openai.com/v1",
)
messages = [{"role": "system", "content": "You are Hermes."}]
for i in range(120):
    messages.append({"role": "user", "content": f"Question {i}: " + ("x" * 400)})
    messages.append({"role": "assistant", "content": f"Answer {i}: " + ("y" * 600)})

def bench():
    compressor.should_compress_preflight(messages)

if __name__ == "__main__":
    runner = pyperf.Runner()
    runner.metadata["description"] = "Estimate token usage for a long conversation before compression"
    runner.bench_func("context_preflight", bench)
