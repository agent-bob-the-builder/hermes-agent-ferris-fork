import pathlib
import pyperf

ROOT = pathlib.Path(__file__).resolve().parents[3]
TARGET = ROOT / "venv" / "bin" / "python"

if __name__ == "__main__":
    runner = pyperf.Runner()
    runner.metadata["description"] = "Cold build of Hermes system prompt in the project venv"
    code = f"import sys; sys.path.insert(0, {str(ROOT)!r}); from run_agent import AIAgent; a=AIAgent(model='openai/gpt-5.4', quiet_mode=True, skip_memory=True, skip_context_files=True, enabled_toolsets=['file_tools','terminal_tools','skills_tools']); a._build_system_prompt()"
    runner.bench_command("build_system_prompt", [str(TARGET), "-c", code])
