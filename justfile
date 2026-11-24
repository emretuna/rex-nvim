# rex-nvim development tasks.

# Formatting, Rust tests, then the nvim half's headless Lua suite.
ci:
    cargo fmt --check
    cargo test
    nvim --headless --noplugin -u NONE -l tests/run.lua

# Build and wire the Rex half into ~/.config/rex (idempotent).
install:
    ./install.sh