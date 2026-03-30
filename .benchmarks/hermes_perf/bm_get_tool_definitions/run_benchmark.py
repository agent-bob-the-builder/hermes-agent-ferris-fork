import pathlib
import pyperf

ROOT = pathlib.Path(__file__).resolve().parents[3]
TARGET = ROOT / "venv" / "bin" / "python"

if __name__ == "__main__":
    runner = pyperf.Runner()
    runner.metadata["description"] = "Cold resolve of tool schemas in the project venv"
    code = f"import sys; sys.path.insert(0, {str(ROOT)!r}); from model_tools import get_tool_definitions; get_tool_definitions(enabled_toolsets=['file_tools','terminal_tools','skills_tools'], quiet_mode=True)"
    runner.bench_command("get_tool_definitions", [str(TARGET), "-c", code])
