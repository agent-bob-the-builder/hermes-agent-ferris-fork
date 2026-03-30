import pathlib
import pyperf
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]

if __name__ == "__main__":
    runner = pyperf.Runner()
    runner.metadata["description"] = "Cold import of run_agent in a fresh Python process"
    code = f"import sys; sys.path.insert(0, {str(ROOT)!r}); import run_agent"
    cmd = [sys.executable, "-c", code]
    runner.bench_command("import_run_agent", cmd)
