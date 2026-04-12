Hermes Agent (Nous Research) has been my daily driver for a few weeks but I kept hitting local optima with Python overhead on hot paths — context compression, tool dispatch, message sanitization, session DB queries.

So I went to Rust.

**What I did:** Forked Hermes, identified 24 hot-path functions, wrote PyO3 extensions for each. Wired them in with transparent Python fallbacks — if a crate is missing or fails to load, the Python impl runs instead with no visible difference.

**What's wired in Rust:**
Context compression, tool registry + message sanitization, prompt builder, skin/theme engine, SQLite SessionDB + FTS5, subprocess orchestration, Rayon-based parallel tool batch execution, retry state machine, honcho HTTP client, @-reference parsing, URL safety, approval checking, fuzzy match, patch parsing, ANSI strip, redaction, checkpoint manager (git2), model metadata parsing, timezone clock, usage pricing, title generation, skill utils... and more.

Full wiring map in the README.

**Install:**
`curl -fsSL https://raw.githubusercontent.com/agent-bob-the-builder/hermes-agent-ferris-fork/main/install.sh | bash`

Repo: https://github.com/agent-bob-the-builder/hermes-agent-ferris-fork

Happy to answer questions. This is a personal fork — I run it as my own agent in production. Not affiliated with Nous.
