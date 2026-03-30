#!/usr/bin/env python3
"""
Tools Package — lazy loading via __getattr__.

Every subpackage/tool module is loaded on first access, not at package import time.
This keeps `import tools` fast (~0ms) and defers all tool module imports until
they're actually needed.

Subpackage access:
    from tools import terminal_tool   → lazy-loads tools/terminal_tool
    from tools import web_tools       → lazy-loads tools/web_tools

Standalone helpers:
    check_file_requirements()   → delegates to terminal_tool (avoids circular import)
"""

from __future__ import annotations
from typing import Any

__all__ = [
    "web_tools",
    "terminal_tool",
    "vision_tools",
    "mixture_of_agents_tool",
    "image_generation_tool",
    "skills_tool",
    "skill_manager_tool",
    "browser_tool",
    "cronjob_tools",
    "rl_training_tool",
    "file_tools",
    "tts_tool",
    "todo_tool",
    "clarify_tool",
    "code_execution_tool",
    "delegate_tool",
]


_TOOL_SUBPACKAGES: dict[str, list[str]] = {
    "web_tools": ["web_search_tool", "web_extract_tool", "web_crawl_tool", "check_firecrawl_api_key"],
    "terminal_tool": ["terminal_tool", "check_terminal_requirements", "cleanup_vm",
                      "cleanup_all_environments", "get_active_environments_info",
                      "register_task_env_overrides", "clear_task_env_overrides",
                      "TERMINAL_TOOL_DESCRIPTION"],
    "vision_tools": ["vision_analyze_tool", "check_vision_requirements"],
    "mixture_of_agents_tool": ["mixture_of_agents_tool", "check_moa_requirements"],
    "image_generation_tool": ["image_generate_tool", "check_image_generation_requirements"],
    "skills_tool": ["skills_list", "skill_view", "check_skills_requirements", "SKILLS_TOOL_DESCRIPTION"],
    "skill_manager_tool": ["skill_manage", "check_skill_manage_requirements", "SKILL_MANAGE_SCHEMA"],
    "browser_tool": ["browser_navigate", "browser_snapshot", "browser_click", "browser_type",
                     "browser_scroll", "browser_back", "browser_press", "browser_close",
                     "browser_get_images", "browser_vision", "cleanup_browser",
                     "cleanup_all_browsers", "get_active_browser_sessions",
                     "check_browser_requirements", "BROWSER_TOOL_SCHEMAS"],
    "cronjob_tools": ["cronjob", "schedule_cronjob", "list_cronjobs", "remove_cronjob",
                      "check_cronjob_requirements", "get_cronjob_tool_definitions",
                      "CRONJOB_SCHEMA"],
    "rl_training_tool": ["rl_list_environments", "rl_select_environment", "rl_get_current_config",
                         "rl_edit_config", "rl_start_training", "rl_check_status",
                         "rl_stop_training", "rl_get_results", "rl_list_runs",
                         "rl_test_inference", "check_rl_api_keys", "get_missing_keys"],
    "file_tools": ["read_file_tool", "write_file_tool", "patch_tool", "search_tool",
                   "get_file_tools", "clear_file_ops_cache"],
    "tts_tool": ["text_to_speech_tool", "check_tts_requirements"],
    "todo_tool": ["todo_tool", "check_todo_requirements", "TODO_SCHEMA", "TodoStore"],
    "clarify_tool": ["clarify_tool", "check_clarify_requirements", "CLARIFY_SCHEMA"],
    "code_execution_tool": ["execute_code", "check_sandbox_requirements", "EXECUTE_CODE_SCHEMA"],
    "delegate_tool": ["delegate_task", "check_delegate_requirements", "DELEGATE_TASK_SCHEMA"],
}

_LAZY_CACHE: dict[str, Any] = {}


def __getattr__(name: str) -> Any:
    # Subpackage — load lazily on first access
    if name in _TOOL_SUBPACKAGES:
        if name not in _LAZY_CACHE:
            _LAZY_CACHE[name] = __import__(f"tools.{name}", fromlist=[name])
        return _LAZY_CACHE[name]

    # Inline helpers (defined directly to avoid needing subpackage imports)
    if name == "check_file_requirements":
        from tools.terminal_tool import check_terminal_requirements
        return check_terminal_requirements

    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
