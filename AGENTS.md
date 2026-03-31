# Hermes Agent - Development Guide

Instructions for AI coding assistants and developers working on the hermes-agent codebase.

## Development Environment

```bash
source venv/bin/activate  # Python only
```

## Project Structure

```
hermes-agent/
├── run_agent.py          # AIAgent — core conversation loop
├── model_tools.py        # Tool orchestration
├── toolsets.py           # Toolset definitions
├── cli.py                # HermesCLI — interactive CLI
├── hermes_state.py       # SessionDB — SQLite FTS5
├── agent/                # Agent internals
│   ├── prompt_builder.py     # System prompt assembly
│   ├── context_compressor.py # Auto context compression
│   ├── prompt_caching.py     # Anthropic prompt caching
│   ├── auxiliary_client.py  # Vision/summarization client
│   ├── model_metadata.py     # Model context lengths
│   ├── models_dev.py        # models.dev registry
│   ├── display.py           # KawaiiSpinner, tool preview
│   ├── skill_commands.py    # Skill slash commands
│   └── trajectory.py        # Trajectory saving
├── hermes_cli/           # CLI subcommands
│   ├── main.py            # Entry point — all `hermes` subcommands
│   ├── config.py         # DEFAULT_CONFIG, OPTIONAL_ENV_VARS, migration
│   ├── commands.py       # COMMAND_REGISTRY + SlashCommandCompleter
│   ├── callbacks.py      # Terminal callbacks
│   ├── setup.py          # Interactive setup wizard
│   ├── skin_engine.py    # Skin/theme engine (pure data)
│   ├── skills_config.py  # `hermes skills`
│   ├── tools_config.py   # `hermes tools`
│   ├── skills_hub.py     # `/skills` slash command
│   ├── models.py         # Model catalog
│   ├── model_switch.py   # /model switch
│   └── auth.py           # Provider credential resolution
├── tools/                # Tool implementations
│   ├── registry.py       # Central registry (schemas, handlers, dispatch)
│   ├── approval.py       # Dangerous command detection
│   ├── terminal_tool.py # Terminal orchestration
│   ├── process_registry.py # Background processes
│   ├── file_tools.py     # read/write/search/patch
│   ├── web_tools.py      # Web search/extract
│   ├── browser_tool.py  # Browser automation
│   ├── code_execution_tool.py # execute_code sandbox
│   ├── delegate_tool.py  # Subagent delegation
│   ├── mcp_tool.py       # MCP client
│   └── environments/     # Terminal backends (local, docker, ssh, modal…)
├── gateway/              # Messaging platform gateway
│   ├── run.py            # Main loop, slash commands, dispatch
│   ├── session.py        # SessionStore — conversation persistence
│   └── platforms/        # telegram, discord, slack, whatsapp…
├── acp_adapter/          # VS Code / Zed / JetBrains
├── cron/                 # Scheduler
├── environments/         # RL training (Atropos)
├── tests/                # ~3000 tests
└── batch_runner.py       # Parallel batch processing
```

**User config:** `~/.hermes/config.yaml`, `~/.hermes/.env`

## File Dependency Chain

```
tools/registry.py  (no deps — imported by all tool files)
       ↑
tools/*.py  (each calls registry.register() at import time)
       ↑
model_tools.py  (imports tools/registry + triggers tool discovery)
       ↑
run_agent.py, cli.py, batch_runner.py, environments/
```

## AIAgent Interface

```python
class AIAgent:
    def __init__(self,
        model: str = "anthropic/claude-opus-4.6",
        max_iterations: int = 90,
        enabled_toolsets: list = None,
        disabled_toolsets: list = None,
        quiet_mode: bool = False,
        save_trajectories: bool = False,
        platform: str = None,
        session_id: str = None,
        skip_context_files: bool = False,
        skip_memory: bool = False,
        ...
    ): ...

    def chat(self, message: str) -> str:
        """Simple interface — returns final response string."""

    def run_conversation(self, user_message: str, system_message: str = None,
                         conversation_history: list = None, task_id: str = None) -> dict:
        """Full interface — returns dict with final_response + messages."""
```

Core loop: send messages with tools → receive tool calls → execute via `handle_function_call()` → append results → repeat. Messages follow OpenAI format. Reasoning content stored in `assistant_msg["reasoning"]`.

---

## Tool Registration Pattern

**3-file pattern:**

**1. `tools/your_tool.py`:**
```python
import json, os
from tools.registry import registry

def check_requirements() -> bool:
    return bool(os.getenv("EXAMPLE_API_KEY"))

def your_tool(param: str, task_id: str = None) -> str:
    return json.dumps({"success": True, "data": "..."})

registry.register(
    name="your_tool",
    toolset="example",
    schema={"name": "your_tool", "description": "...", "parameters": {...}},
    handler=lambda args, **kw: your_tool(param=args.get("param", ""), task_id=kw.get("task_id")),
    check_fn=check_requirements,
    requires_env=["EXAMPLE_API_KEY"],
)
```

**2. Import in `model_tools.py` `_discover_tools()`.**

**3. Add to `toolsets.py`** — `_HERMES_CORE_TOOLS` (all platforms) or a new toolset.

Registry handles schema collection, dispatch, availability checking, error wrapping. **All handlers MUST return a JSON string.**

**State files**: use `get_hermes_home()` for persistent state — never `Path.home() / ".hermes"`. Profile-aware.

**Cross-tool references in schemas**: forbidden. Tool schemas must not mention other tools by name — those tools may be unavailable. Add cross-references dynamically in `get_tool_definitions()` instead.

---

## Configuration System

### config.yaml

1. Add to `DEFAULT_CONFIG` in `hermes_cli/config.py`
2. Bump `_config_version` to trigger migration for existing users

### .env variables

Add to `OPTIONAL_ENV_VARS` in `hermes_cli/config.py`:
```python
"NEW_API_KEY": {
    "description": "What it's for",
    "prompt": "Display name",
    "url": "https://...",
    "password": True,
    "category": "tool",  # provider, tool, messaging, setting
},
```

### Config loaders

| Loader | Used by | Location |
|--------|---------|----------|
| `load_cli_config()` | CLI mode | `cli.py` |
| `load_config()` | `hermes tools`, `hermes setup` | `hermes_cli/config.py` |
| Direct YAML load | Gateway | `gateway/run.py` |

### Profile System

Hermes supports multiple isolated instances via `HERMES_HOME` env var. All paths must use `get_hermes_home()` — never hardcode `~/.hermes`. Profile operations are HOME-anchored at `Path.home() / ".hermes" / "profiles"`.

---

## Known Pitfalls

### DO NOT hardcode `~/.hermes` paths
Every path that touches config, memory, sessions, skills, or gateway state must use `get_hermes_home()`. Hardcoding `~/.hermes` breaks profiles — each profile has its own `HERMES_HOME` directory. This caused 5 bugs in PR #3575.

### DO NOT reference other tools in tool schemas
Schema descriptions must not name tools from other toolsets. Those tools may be disabled or unavailable, causing the model to hallucinate calls to non-existent tools.

### DO NOT use `simple_term_menu` for interactive menus
Rendering bugs in tmux/iTerm2 (ghosting on scroll). Use `curses` (stdlib) instead.

### DO NOT use `\033[K` (ANSI erase-to-EOL) in spinner/display code
Leaks as literal `?[K` text under `prompt_toolkit`'s `patch_stdout`. Use space-padding instead.

### `_last_resolved_tool_names` is a process-global
`delegate_tool.py` saves/restores this around subagent runs. Code that reads it during child execution may see stale values.

### Tests must not write to `~/.hermes/`
The `_isolate_hermes_home` autouse fixture redirects `HERMES_HOME` to a temp dir. Profile tests must also mock `Path.home()` so `_get_profiles_root()` resolves correctly.

---

## Important Policies

### Prompt Caching Must Not Break
Do NOT alter past context mid-conversation, change toolsets mid-conversation, or reload memories mid-conversation. Cache-breaking forces dramatically higher costs. The ONLY legitimate time: during context compression.

### Working Directory
- **CLI**: current directory (`os.getcwd()`)
- **Messaging**: `MESSAGING_CWD` env var (default: home)

### Background Process Notifications
Gateway watcher pushes status to chat. Control with `display.background_process_notifications`:
- `all` — running updates + final message (default)
- `result` — only final completion
- `error` — only final on non-zero exit
- `off` — no messages
