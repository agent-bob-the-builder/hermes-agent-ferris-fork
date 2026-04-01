<img width="498" height="381" alt="image" src="https://github.com/user-attachments/assets/c612e742-6342-4fd3-b5bc-92e313aa608c" />

# Hermes Agent - Ferris Fork ☤

A performance oriented Rust fork of [Hermes Agent](https://github.com/NousResearch/hermes-agent) by Nous Research. Maintained by [agent-bob-the-builder](https://github.com/agent-bob-the-builder) at [github.com/agent-bob-the-builder/hermes-agent-ferris-fork](https://github.com/agent-bob-the-builder/hermes-agent-ferris-fork).

---

## What & Why

24 PyO3 extension crates replace hot-path Python code — no visible behaviour change, but meaningfully faster on every agent turn. All wired crates use transparent Python fallbacks; if a crate is missing or fails to load, the Python implementation runs instead with no visible difference.

| Crate | Hot path | Status |
|---|---|---|
| `compressor_rs` | ContextCompressor.compress() | Wired ✓ |
| `model_tools_rs` | Tool registry + message sanitization | Wired ✓ |
| `prompt_builder_rs` | System prompt assembly | Wired ✓ |
| `skin_engine_rs` | Skin/theme loading and config | Wired ✓ |
| `hermes_state_rs` | SQLite SessionDB + FTS5 search | Wired ✓ |
| `file_ops_rs` | Binary detection, line numbering, path expansion, shell escaping, unified diff, fuzzy file search | Wired ✓ |
| `fuzzy_match_rs` | 8-strategy fuzzy find-and-replace | Wired ✓ |
| `patch_parser_rs` | V4A patch format parsing | Wired ✓ |
| `ansi_strip_rs` | Strip ANSI escape sequences | Wired ✓ |
| `redact_rs` | Sensitive data redaction | Wired ✓ |
| `subprocess_rs` | Subprocess orchestration | Wired ✓ |
| `tool_dispatch_rs` | Rayon-based parallel tool batch execution | Wired ✓ |
| `retry_state_machine_rs` | Retry/fallback/compression state machine | Wired ✓ |
| `honcho_http_rs` | Honcho HTTP client | Wired ✓ |
| `context_refs_rs` | @-reference parsing + token stripping | Wired ✓ |
| `approval_rs` | Dangerous command detection / approval | Wired ✓ |
| `url_safety_rs` | URL safety checks for web tools | Wired ✓ |
| `url_safety_python_rs` | Pure-Python URL safety fallback in Rust | Wired ✓ |
| `hermes_time_rs` | Timezone-aware clock (datetime ops) | Wired ✓ |
| `checkpoint_manager_rs` | Git-based transparent filesystem snapshots via git2 | Wired ✓ |
| `title_generator_rs` | Session title prompt formatting + parsing | Wired ✓ |
| `model_metadata_rs` | Model metadata parsing, context length extraction, token estimation | Wired ✓ |
| `skill_utils_rs` | SKILL.md/YAML frontmatter parsing, path resolution, category inference | Wired ✓ |
| `usage_pricing_rs` | Usage pricing structs, cost calculation, Decimal handling | Wired ✓ |

Wired crates have **transparent Python fallbacks** — if a crate is missing or fails to load, the Python implementation runs instead with no visible difference.

**Wiring map:**

```
AIAgent (run_agent.py)
├── context_compressor.py
│   └── compressor_rs.compress_async()             [Wired ✓]
├── model_tools.py + tools/registry_rs.py
│   └── model_tools_rs.sanitize()                 [Wired ✓]
├── hermes_state.py
│   └── hermes_state_rs session ops               [Wired ✓]
├── hermes_time.py
│   └── hermes_time_rs.now() / get_timezone() / reset_cache()                              [Wired ✓]
├── run_agent.py
│   └── maybe_auto_title() → title_generator_rs.format_title_prompt()                     [Wired ✓]
│                           → title_generator_rs.parse_title_response()                   [Wired ✓]
│                           → title_generator_rs.should_auto_title()                       [Wired ✓]
├── agent/model_metadata.py
│   ├── strip_provider_prefix() → model_metadata_rs                                       [Wired ✓]
│   ├── is_local_endpoint() → model_metadata_rs                                           [Wired ✓]
│   ├── extract_context_length() → model_metadata_rs                                      [Wired ✓]
│   ├── extract_max_completion_tokens() → model_metadata_rs                               [Wired ✓]
│   ├── extract_pricing() → model_metadata_rs                                             [Wired ✓]
│   ├── parse_context_limit_from_error() → model_metadata_rs                              [Wired ✓]
│   ├── model_id_matches() → model_metadata_rs                                            [Wired ✓]
│   ├── get_next_probe_tier() → model_metadata_rs                                         [Wired ✓]
│   └── estimate_tokens_rough() → model_metadata_rs                                        [Wired ✓]
├── hermes_cli/skin_engine.py
│   ├── skin_engine_rs init_skin_from_config()    [Wired ✓]
│   └── prompt_builder_rs _build_system_prompt()  [Wired ✓]
├── tools/
│   ├── file_operations.py (ShellFileOperations)
│   │   ├── _is_likely_binary() → file_ops_rs             [Wired ✓]
│   │   ├── _add_line_numbers() → file_ops_rs             [Wired ✓]
│   │   ├── _native_expand_path() → file_ops_rs           [Wired ✓]
│   │   ├── _escape_shell_arg() → file_ops_rs             [Wired ✓]
│   │   ├── _unified_diff() → file_ops_rs                 [Wired ✓]
│   │   ├── _suggest_similar_files() → file_ops_rs        [Wired ✓]
│   │   └── _search_native() → file_ops_rs                [Wired ✓]
│   ├── fuzzy_match.py
│   │   └── fuzzy_find_and_replace() → fuzzy_match_rs     [Wired ✓]
│   ├── patch_parser.py
│   │   └── parse_v4a_patch() → patch_parser_rs            [Wired ✓]
│   ├── subprocess_rs.py
│   │   └── spawn / interrupt / cleanup_session           [Wired ✓]
│   └── url_safety.py
│       └── URLSafetyChecker                              [Wired ✓]
├── agent/redact.py
│   └── redact_text() → redact_rs                         [Wired ✓]
├── run_agent.py
│   ├── _should_parallelize_tool_batch()
│   │   └── tool_dispatch_rs.should_parallelize()          [Wired ✓]
│   ├── _execute_tool_calls_concurrent_rs()
│   │   └── tool_dispatch_rs.run_concurrent_tool_batch()  [Wired ✓]
│   └── _rs_evaluate_retry()
│       └── _retry_state_machine_rs.rs_evaluate()          [Wired ✓]
├── honcho_integration/session.py
│   └── _honcho_http_rust → honcho_http_rs                [Wired ✓]
├── agent/context_references.py
│   ├── parse_context_references() → context_refs_rs       [Wired ✓]
│   └── _remove_reference_tokens() → context_refs_rs      [Wired ✓]
└── tools/approval.py
    └── check_approval() → approval_rs                     [Callable ✓]
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
    SUB["tools/subprocess_rs.py<br/>spawn/interrupt"]
    URL["tools/url_safety.py<br/>URLSafetyChecker"]
    RED["agent/redact.py<br/>redact_text()"]
    HON["honcho_integration/session.py<br/>_honcho_http_rust"]
    TDP["run_agent.py<br/>_execute_tool_calls_concurrent_rs()"]
    RET["run_agent.py<br/>_rs_evaluate_retry()"]
    CRE["agent/context_references.py<br/>parse_context_references()"]
    HT["hermes_time.py<br/>now() / get_timezone()"]
    CM["tools/checkpoint_manager.py<br/>CheckpointManager"]
    TG["agent/title_generator.py<br/>format_title_prompt() / parse_title_response()"]
    MM["agent/model_metadata.py<br/>extract_context_length() / is_local_endpoint() / etc."]
    APP["tools/approval.py<br/>check_approval()"]

    PB_RS["prompt_builder_rs"]
    CO_RS["compressor_rs"]
    MT_RS["model_tools_rs"]
    HS_RS["hermes_state_rs"]
    SK_RS["skin_engine_rs"]
    FO_RS["file_ops_rs"]
    FM_RS["fuzzy_match_rs"]
    PP_RS["patch_parser_rs"]
    SU_RS["subprocess_rs"]
    UR_RS["url_safety_rs"]
    UP_RS["url_safety_python_rs"]
    HT_RS["hermes_time_rs"]
    CM_RS["checkpoint_manager_rs"]
    TG_RS["title_generator_rs"]
    MM_RS["model_metadata_rs"]
    RE_RS["redact_rs"]
    HH_RS["honcho_http_rs"]
    TD_RS["tool_dispatch_rs"]
    RT_RS["retry_state_machine_rs"]
    CR_RS["context_refs_rs"]
    AP_RS["approval_rs"]

    RA -->|"prompt assembly"| PCP
    RA -->|"tool registry"| MTP
    RA -->|"context compression"| CCP
    RA -->|"session / search"| SST
    RA -->|"skin loading"| SHE
    RA -->|"honcho session"| HON
    RA -->|"parallel tool batch"| TDP
    RA -->|"retry/fallback"| RET
    RA -->|"@ reference parsing"| CRE

    PCP -->|"wired ✓"| PB_RS
    PCP -.->|"fallback"| PCP

    CCP -->|"wired ✓"| CO_RS
    CCP -.->|"fallback"| CCP

    MTP -->|"wired ✓"| MT_RS
    MTP -.->|"fallback"| MTP

    SST -->|"wired ✓"| HS_RS
    SST -.->|"fallback"| SST

    SHE -->|"wired ✓"| SK_RS
    SHE -.->|"fallback"| SHE

    FOP -->|"wired ✓"| FO_RS
    FOP -.->|"fallback"| FOP

    FUZ -->|"wired ✓"| FM_RS
    FUZ -.->|"fallback"| FUZ

    PAT -->|"wired ✓"| PP_RS
    PAT -.->|"fallback"| PAT

    SUB -->|"wired ✓"| SU_RS
    SUB -.->|"fallback"| SUB

    URL -->|"wired ✓"| UR_RS
    UR_RS -.->|"fallback"| UP_RS
    URL -.->|"fallback"| URL

    RED -->|"wired ✓"| RE_RS
    RED -.->|"fallback"| RED

    HON -->|"wired ✓"| HH_RS
    HON -.->|"fallback"| HON

    TDP -->|"wired ✓"| TD_RS
    TDP -.->|"fallback"| TDP

    RET -->|"wired ✓"| RT_RS
    RET -.->|"fallback"| RET

    CRE -->|"wired ✓"| CR_RS
    CRE -.->|"fallback"| CRE

    HT -->|"wired ✓"| HT_RS
    HT -.->|"fallback"| HT

    CM -->|"wired ✓"| CM_RS
    CM -.->|"fallback"| CM

    TG -->|"wired ✓"| TG_RS
    TG -.->|"fallback"| TG

    MM -->|"wired ✓"| MM_RS
    MM -.->|"fallback"| MM

    APP -->|"callable ✓"| AP_RS
    APP -.->|"fallback"| APP

    style PB_RS fill:#de5347,color:#fff
    style CO_RS fill:#de5347,color:#fff
    style MT_RS fill:#de5347,color:#fff
    style HS_RS fill:#de5347,color:#fff
    style SK_RS fill:#de5347,color:#fff
    style FO_RS fill:#de5347,color:#fff
    style FM_RS fill:#de5347,color:#fff
    style PP_RS fill:#de5347,color:#fff
    style SU_RS fill:#de5347,color:#fff
    style UR_RS fill:#de5347,color:#fff
    style RE_RS fill:#de5347,color:#fff
    style HH_RS fill:#de5347,color:#fff
    style TD_RS fill:#de5347,color:#fff
    style RT_RS fill:#de5347,color:#fff
    style CR_RS fill:#de5347,color:#fff
    style UP_RS fill:#de5347,color:#fff
    style HT_RS fill:#de5347,color:#fff
    style CM_RS fill:#de5347,color:#fff
    style TG_RS fill:#de5347,color:#fff
    style MM_RS fill:#de5347,color:#fff
    style AP_RS fill:#de5347,color:#fff
```

---

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/agent-bob-the-builder/hermes-agent-ferris-fork/main/install.sh | bash
```

---

## Upstream

For everything else — CLI commands, messaging gateway, skills, memory, MCP, cron, etc. — see the [full Hermes Agent documentation](https://hermes-agent.nousresearch.com/docs/).
