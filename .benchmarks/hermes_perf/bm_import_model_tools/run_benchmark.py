import pathlib
import pyperf
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]

if __name__ == "__main__":
    runner = pyperf.Runner()
    runner.metadata["description"] = "Cold import of model_tools in a fresh Python process"
    code = f"import sys; sys.path.insert(0, {str(ROOT)!r}); import model_tools"
    cmd = [sys.executable, "-c", code]
    runner.bench_command("import_model_tools", cmd)
