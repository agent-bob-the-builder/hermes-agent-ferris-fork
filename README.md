<img width="498" height="381" alt="image" src="https://github.com/user-attachments/assets/c612e742-6342-4fd3-b5bc-92e313aa608c" />

# Hermes Agent - Ferris Fork ☤

A performance oriented Rust fork of [Hermes Agent](https://github.com/NousResearch/hermes-agent) by Nous Research. Maintained by [agent-bob-the-builder](https://github.com/agent-bob-the-builder) at [github.com/agent-bob-the-builder/hermes-agent-ferris-fork](https://github.com/agent-bob-the-builder/hermes-agent-ferris-fork).

---

## What & Why

Eight PyO3 extension crates replace hot-path Python code — no visible behaviour change, but meaningfully faster on every agent turn. The first five are wired into the agent loop; the remaining three handle supporting infrastructure.

| Crate | Hot path | Status |
|---|---|---|
| `rust_compressor` | `ContextCompressor.compress()` | Production ✓ |
| `model_tools_rs` | Tool registry + message sanitization | Production ✓ |
| `prompt_builder_rs` | System prompt assembly | Production ✓ |
| `skin_engine_rs` | Skin/theme loading and config | Production ✓ |
| `hermes_state_rs` | SQLite SessionDB + FTS5 search | Production ✓ |
| `file_ops_rs` | Binary detection, line numbering, path expansion, fuzzy search | Built ✓ |
| `fuzzy_match_rs` | FTS query fuzzy matching with Unicode normalization | Built ✓ |
| `patch_parser_rs` | Patch file parsing (`diff -u` format) | Built ✓ |

All wired crates are **transparent fallbacks**: if a crate is missing or fails to load, the Python implementation runs instead with no visible difference. Built-only crates (`file_ops_rs`, `fuzzy_match_rs`, `patch_parser_rs`) are compiled and available — wiring them into the Python layer is the remaining work.

**Wiring map:**

```
AIAgent (run_agent.py)
├── context_compressor.py
│   └── rust_compressor.compress_async()      [Production ✓]
├── model_tools.py + tools/registry_rs.py
│   └── _model_tools_rust.sanitize()          [Production ✓]
├── hermes_state.py
│   └── _hermes_state_rust session ops        [Production ✓]
└── hermes_cli/skin_engine.py
    └── _skin_engine_rust init_skin_from_config() [Production ✓]
    └── _prompt_builder_rust.build()           [Production ✓]
```

```mermaid
graph TD
    RA["run_agent.py<br/>(AIAgent)"]
    PCP["prompt_builder.py<br/>_build_system_prompt()"]
    CCP["context_compressor.py<br/>ContextCompressor.compress()"]
    MTP["model_tools.py<br/>sanitize_api_messages()"]
    SST["hermes_state.py<br/>SessionDB"]
    SHE["hermes_cli/skin_engine.py<br/>init_skin_from_config()"]

    PB_RS["prompt_builder_rs<br/>_prompt_builder_rust.build()"]
    CO_RS["rust_compressor<br/>rust_compressor.so<br/>token_count() + compress()"]
    MT_RS["model_tools_rs<br/>_model_tools_rust.so<br/>sanitize()"]
    HS_RS["hermes_state_rs<br/>_hermes_state_rust.so<br/>SQLite + FTS5"]
    SK_RS["skin_engine_rs<br/>_skin_engine_rust.so<br/>load + parse"]
    FO_RS["file_ops_rs<br/>_file_ops_rust.so<br/>detection / paths / fuzzy"]
    FM_RS["fuzzy_match_rs<br/>_fuzzy_match_rust.so<br/>FTS fuzzy match"]
    PP_RS["patch_parser_rs<br/>_patch_parser_rust.so<br/>diff -u parse"]

    RA -->|"prompt assembly"| PCP
    RA -->|"tool registry"| MTP
    RA -->|"context compression"| CCP
    RA -->|"session / search"| SST
    RA -->|"skin loading"| SHE

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

    style PB_RS fill:#de5347,color:#fff
    style CO_RS fill:#de5347,color:#fff
    style MT_RS fill:#de5347,color:#fff
    style HS_RS fill:#de5347,color:#fff
    style SK_RS fill:#de5347,color:#fff
    style FO_RS fill:#5a3a9a,color:#fff
    style FM_RS fill:#5a3a9a,color:#fff
    style PP_RS fill:#5a3a9a,color:#fff
```

---

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/agent-bob-the-builder/hermes-agent-ferris-fork/main/install.sh | bash
```

---

## Upstream

For everything else — CLI commands, messaging gateway, skills, memory, MCP, cron, etc. — see the [full Hermes Agent documentation](https://hermes-agent.nousresearch.com/docs/).
