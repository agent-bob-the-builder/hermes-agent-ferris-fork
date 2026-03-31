<img width="498" height="381" alt="image" src="https://github.com/user-attachments/assets/c612e742-6342-4fd3-b5bc-92e313aa608c" />

# Hermes Agent - Ferris Fork ☤

A performance oriented Rust fork of [Hermes Agent](https://github.com/NousResearch/hermes-agent) by Nous Research. Maintained by [agent-bob-the-builder](https://github.com/agent-bob-the-builder) at [github.com/agent-bob-the-builder/hermes-agent-ferris-fork](https://github.com/agent-bob-the-builder/hermes-agent-ferris-fork).

---

## What & Why

15 PyO3 extension crates replace hot-path Python code — no visible behaviour change, but meaningfully faster on every agent turn. All are wired into the agent loop with transparent Python fallbacks.

| Crate | Hot path | Status |
|---|---|---|
| `rust_compressor` | `ContextCompressor.compress()` | Production ✓ |
| `model_tools_rs` | Tool registry + message sanitization | Production ✓ |
| `prompt_builder_rs` | System prompt assembly | Production ✓ |
| `skin_engine_rs` | Skin/theme loading and config | Production ✓ |
| `hermes_state_rs` | SQLite SessionDB + FTS5 search | Production ✓ |
| `file_ops_rs` | Binary detection, line numbering, path expansion, shell escaping, unified diff, fuzzy file search | Production ✓ |
| `fuzzy_match_rs` | 8-strategy fuzzy find-and-replace | Production ✓ |
| `patch_parser_rs` | V4A patch format parsing | Production ✓ |
| `rust_ansi_strip` | Strip ANSI escape sequences | Production ✓ |
| `rust_redact` | Sensitive data redaction | Production ✓ |
| `subprocess_rs` | Subprocess orchestration | Production ✓ |
| `run_agent_loop_rs` | Core agent loop | Production ✓ |
| `tool_dispatch_rs` | Tool dispatch routing | Production ✓ |
| `retry_state_machine_rs` | Retry with exponential back-off | Production ✓ |
| `honcho_http_rs` | Honcho HTTP client | Production ✓ |

All crates are **transparent fallbacks**: if a crate is missing or fails to load, the Python implementation runs instead with no visible difference.

**Wiring map:**

```
AIAgent (run_agent.py)
├── context_compressor.py
│   └── rust_compressor.compress_async()          [Production ✓]
├── model_tools.py + tools/registry_rs.py
│   └── model_tools_rs.sanitize()                 [Production ✓]
├── hermes_state.py
│   └── hermes_state_rs session ops               [Production ✓]
├── hermes_cli/skin_engine.py
│   ├── skin_engine_rs init_skin_from_config()    [Production ✓]
│   └── prompt_builder_rs _build_system_prompt()  [Production ✓]
├── tools/
│   ├── file_operations.py (ShellFileOperations)
│   │   ├── _is_likely_binary() → file_ops_rs             [Production ✓]
│   │   ├── _add_line_numbers() → file_ops_rs             [Production ✓]
│   │   ├── _native_expand_path() → file_ops_rs           [Production ✓]
│   │   ├── _escape_shell_arg() → file_ops_rs             [Production ✓]
│   │   ├── _unified_diff() → file_ops_rs                 [Production ✓]
│   │   ├── _suggest_similar_files() → file_ops_rs        [Production ✓]
│   │   └── _search_native() → file_ops_rs                [Production ✓]
│   ├── fuzzy_match.py
│   │   └── fuzzy_find_and_replace() → fuzzy_match_rs     [Production ✓]
│   └── patch_parser.py
│       └── parse_v4a_patch() → patch_parser_rs           [Production ✓]
└── agent/
    └── trajectory.py → retry_state_machine_rs           [Production ✓]
```

```mermaid
graph TD
    RA["run_agent.py<br/>(AIAgent)"]
    PCP["prompt_builder.py<br/>_build_system_prompt()"]
    CCP["context_compressor.py<br/>ContextCompressor.compress()"]
    MTP["model_tools.py<br/>sanitize_api_messages()"]
    SST["hermes_state.py<br/>SessionDB"]
    SHE["hermes_cli/skin_engine.py<br/>init_skin_from_config()"]
    FOP["tools/file_operations.py<br/>ShellFileOperations"]
    FUZ["tools/fuzzy_match.py<br/>fuzzy_find_and_replace()"]
    PAT["tools/patch_parser.py<br/>parse_v4a_patch()"]
    RET["agent/trajectory.py<br/>retry_state_machine_rs"]

    PB_RS["prompt_builder_rs"]
    CO_RS["rust_compressor"]
    MT_RS["model_tools_rs"]
    HS_RS["hermes_state_rs"]
    SK_RS["skin_engine_rs"]
    FO_RS["file_ops_rs"]
    FM_RS["fuzzy_match_rs"]
    PP_RS["patch_parser_rs"]
    RT_RS["retry_state_machine_rs"]

    RA -->|"prompt assembly"| PCP
    RA -->|"tool registry"| MTP
    RA -->|"context compression"| CCP
    RA -->|"session / search"| SST
    RA -->|"skin loading"| SHE
    RA -->|"retry / trajectory"| RET

    PCP -->|"production ✓"| PB_RS
    PCP -.->|"fallback"| PCP

    CCP -->|"production ✓"| CO_RS
    CCP -.->|"fallback"| CCP

    MTP -->|"production ✓"| MT_RS
    MTP -.->|"fallback"| MTP

    SST -->|"production ✓"| HS_RS
    SST -.->|"fallback"| SST

    SHE -->|"production ✓"| SK_RS
    SHE -.->|"fallback"| SHE

    FOP -->|"production ✓"| FO_RS
    FOP -.->|"fallback"| FOP

    FUZ -->|"production ✓"| FM_RS
    FUZ -.->|"fallback"| FUZ

    PAT -->|"production ✓"| PP_RS
    PAT -.->|"fallback"| PAT

    RET -->|"production ✓"| RT_RS
    RET -.->|"fallback"| RET

    style PB_RS fill:#de5347,color:#fff
    style CO_RS fill:#de5347,color:#fff
    style MT_RS fill:#de5347,color:#fff
    style HS_RS fill:#de5347,color:#fff
    style SK_RS fill:#de5347,color:#fff
    style FO_RS fill:#de5347,color:#fff
    style FM_RS fill:#de5347,color:#fff
    style PP_RS fill:#de5347,color:#fff
    style RT_RS fill:#de5347,color:#fff
```

---

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/agent-bob-the-builder/hermes-agent-ferris-fork/main/install.sh | bash
```

---

## Upstream

For everything else — CLI commands, messaging gateway, skills, memory, MCP, cron, etc. — see the [full Hermes Agent documentation](https://hermes-agent.nousresearch.com/docs/).
