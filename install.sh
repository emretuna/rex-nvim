#!/usr/bin/env bash
# rex-nvim installer.
#
#   ./install.sh               build the binary, install it, wire the Rex half
#   ./install.sh --uninstall   remove the binary, symlink, and init.lua block
#
# Idempotent: run it again to upgrade. Writes nothing outside $HOME, the
# binary path, the Rex state dir, and ~/.config/rex.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
MSRV="1.89"
BIN_DIR="$HOME/.local/bin"
BIN="$BIN_DIR/rex-nvim"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/rex"
INIT_LUA="$CONFIG_DIR/init.lua"
LINK="$CONFIG_DIR/rex-nvim"
STATE_DIR="${REX_NVIM_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/rex-nvim}"
INSTALL_JSON="$STATE_DIR/install.json"

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
usage: ./install.sh [--uninstall]

  (no options)  build (cargo, MSRV 1.89), install to ~/.local/bin/rex-nvim,
                write the Rex state, link ~/.config/rex/rex-nvim, and add the
                marker block to ~/.config/rex/init.lua
  --uninstall   stop sidebar daemons, then remove the marker block, symlink,
                binary and install.json
EOF
}

json_escape() {
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

lua_escape() {
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

version_ge() {
  # version_ge HAVE WANT; dotted numeric comparison, no sort -V on macOS.
  awk -v have="$1" -v want="$2" 'BEGIN {
    n = split(have, h, "."); split(want, w, ".")
    for (i = 1; i <= 3; i++) {
      hv = (h[i] == "" ? 0 : h[i] + 0)
      wv = (w[i] == "" ? 0 : w[i] + 0)
      if (hv > wv) exit 0
      if (hv < wv) exit 1
    }
    exit 0
  }'
}

strip_marker() {
  local file="$1"
  [[ -f "$file" ]] || return 0
  awk '
    /^-- rex-nvim begin$/ { skip = 1; next }
    /^-- rex-nvim end$/   { skip = 0; next }
    skip != 1            { print }
  ' "$file" > "$file.rex-nvim.tmp"
  mv "$file.rex-nvim.tmp" "$file"
}

add_marker() {
  local file="$1" root="$2"
  mkdir -p "$(dirname "$file")"
  [[ -f "$file" ]] || : > "$file"
  if grep -q 'require("rex-nvim")' "$file"; then
    say "init.lua already loads rex-nvim; leaving it alone"
    return 0
  fi
  # A file that lost only its require line but kept the markers would otherwise
  # nest marker blocks on the next run.
  strip_marker "$file"
  {
    printf '\n-- rex-nvim begin\n'
    printf 'require("rex-nvim").setup{ root = "%s" }\n' "$(lua_escape "$root")"
    printf -- '-- rex-nvim end\n'
  } >> "$file"
  say "added the rex-nvim block to $file"
}

rex_config() {
  local action="$1"
  if ! command -v rex >/dev/null 2>&1; then
    warn "rex is not on PATH; skipping 'rex config $action'"
    return 0
  fi
  if rex config "$action"; then
    return 0
  fi
  warn "'rex config $action' failed (is the Rex server running?)"
  return 0
}

uninstall() {
  if [[ -x "$BIN" ]]; then
    "$BIN" daemons stop --all --force >/dev/null 2>&1 || true
  fi

  if [[ -f "$INIT_LUA" ]] && grep -q '^-- rex-nvim begin$' "$INIT_LUA"; then
    strip_marker "$INIT_LUA"
    say "removed the rex-nvim block from $INIT_LUA"
  fi
  if [[ -L "$LINK" ]]; then
    rm -f "$LINK"
    say "removed $LINK"
  elif [[ -e "$LINK" ]]; then
    warn "$LINK is not a symlink; leaving it alone"
  fi
  rm -f "$BIN"
  say "removed $BIN"
  rm -f "$INSTALL_JSON"
  say "removed $INSTALL_JSON"

  rex_config reload
  say ""
  say "rex-nvim uninstalled. Reload or restart the Rex app if the keys persist."
}

install() {
  command -v cargo >/dev/null 2>&1 || die \
    "cargo not found. Install Rust $MSRV or newer with rustup: https://rustup.rs"
  local rustc_version
  rustc_version="$(rustc --version | awk '{print $2}')"
  version_ge "$rustc_version" "$MSRV" || die \
    "rustc $rustc_version is too old; rex-nvim needs $MSRV or newer (rustup update)"

  say "building rex-nvim (release)..."
  ( cd "$ROOT" && cargo build --release )
  [[ -x "$ROOT/target/release/rex-nvim" ]] || die "cargo build produced no target/release/rex-nvim"

  mkdir -p "$BIN_DIR"
  cp "$ROOT/target/release/rex-nvim" "$BIN"
  chmod +x "$BIN"
  say "installed $BIN"

  local rex_path=""
  if command -v rex >/dev/null 2>&1; then
    rex_path="$(command -v rex)"
  else
    warn "rex is not on PATH; recording an empty rex path (the Rex half resolves it at runtime)"
  fi

  mkdir -p "$STATE_DIR"
  printf '{"root":"%s","bin":"%s","rex":"%s"}\n' \
    "$(json_escape "$ROOT")" "$(json_escape "$BIN")" "$(json_escape "$rex_path")" \
    > "$INSTALL_JSON"
  say "wrote $INSTALL_JSON"

  mkdir -p "$CONFIG_DIR"
  if [[ -L "$LINK" || ! -e "$LINK" ]]; then
    ln -sfn "$ROOT/rex" "$LINK"
    say "linked $LINK -> $ROOT/rex"
  else
    die "$LINK exists and is not a symlink; move it aside and run install.sh again"
  fi

  add_marker "$INIT_LUA" "$ROOT"

  say ""
  say "checking the Rex config..."
  rex_config check
  rex_config reload

  say ""
  say "rex-nvim is installed."
  say ""
  say "nvim side (lazy.nvim), once the repo is pushed:"
  say '  { "emretuna/rex-nvim", opts = {} }'
  say "from this checkout until then:"
  say "  { dir = \"$ROOT\", opts = {} }"
  say ""
  say "keys (Rex, never overriding an existing binding):"
  say "  cmd+e   toggle the sidebar"
  say "  cmd+o   pick a file the agent touched"
  say "  <leader>ac / al / as / aS / ai   comment, list, send, submit, reference"
  say ""
  say "health check: rex-nvim doctor [--with-agent NAME]"
}

case "${1:-}" in
  "") install ;;
  --uninstall) uninstall ;;
  -h|--help) usage ;;
  *) usage >&2; exit 2 ;;
esac