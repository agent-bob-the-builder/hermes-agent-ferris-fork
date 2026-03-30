import pathlib
import pyperf

ROOT = pathlib.Path(__file__).resolve().parents[3]
TARGET = ROOT / "venv" / "bin" / "python"

if __name__ == "__main__":
    runner = pyperf.Runner()
    runner.metadata["description"] = "Cold create/close of SessionDB in the project venv"
    code = f"import sys, pathlib, tempfile; sys.path.insert(0, {str(ROOT)!r}); from hermes_state import SessionDB; tmp=tempfile.TemporaryDirectory(); db=SessionDB(pathlib.Path(tmp.name)/'state.db'); db.close(); tmp.cleanup()"
    runner.bench_command("sessiondb_init", [str(TARGET), "-c", code])
