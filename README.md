# rex-nvim

Neovim built into your [Rex](https://www.superlogical.com/rex) workspace: a
persistent nvim sidebar one key away, a fuzzy picker over the files your agent
touched, and code annotations you send straight to the agent's pane.

Inspired by [herdr-nvim](https://github.com/ChmaraX/herdr-nvim) — the same
feature set, rebuilt on Rex's native primitives: detachable panes, event
scripts, and terminal process introspection.

Two halves in one repo:

- **`rex/`** — the Rex config module: actions, key bindings, the sidebar
  daemon, agent discovery, the lifecycle supervisor, and `doctor`.
- **`src/` + `lua/` + `plugin/`** — the `rex-nvim` binary (picker, daemon
  inventory, config bridge, doctor wrapper) and the nvim plugin behind the
  sidebar's `:Rex` command.

## Features

- **Full-height nvim sidebar, one key to toggle.** `cmd+e` hides or shows the
  window's nvim pane. Hiding *detaches* the running pane instead of killing it,
  so buffers, cursor, folds and pending comments survive; showing moves the
  same pane back beside its anchor. Each Rex window gets its own nvim daemon,
  and closing the window or the session reaps it (unsaved buffers included).
- **Fuzzy file picker.** `cmd+o` opens on the files the agent in that pane
  touched this session, newest first with `+N -M` diff stats and a `new`
  badge. Type to fuzzy-search the whole repo (`.gitignore` honored, served by
  [`fff-search`](https://crates.io/crates/fff-search)); `⏎` opens the file in
  the sidebar at the right line.
- **Code annotations you send to the agent.** Comment a line or a visual
  selection like a code review, list them, then paste or submit them to any
  agent in the session — with `file:line`, snippets and git context.
- **Diff views, annotated too.** Comment a hunk in diffview.nvim,
  gitsigns.nvim, fugitive or Neogit: the built-in resolvers find the file
  behind the view, the comment is snapshotted so closing the view never loses
  it, and the agent receives `path (diff, kind)` with the full selected diff
  text. Unknown plugins and scratch buffers degrade to an honest label — never
  a pseudo-path; `setup{ diff = { resolvers = { fn } } }` adds your own
  resolver.
- **Git UIs on your own keys.** `rex_nvim_command` runs any nvim command line
  in a new terminal split or the window's sidebar, with `direction`, `side` and
  `ratio` geometry args. `toggle = true` makes a split binding a toggle: the
  next press closes the split it opened (per window) instead of stacking
  another. No default binding: you choose the key and the UI.
- **Inline `path:12-20` references.** Drop a bare reference into the agent's
  input in the middle of a sentence; never submits, never touches pending
  comments.
- **Daemon inventory and health checks.** `rex-nvim daemons` lists every
  sidebar nvim (window, RAM incl. LSP children, uptime, state) and stops one,
  all, or orphans; `rex-nvim doctor` exercises the whole pipeline in a
  throwaway session.

## Requirements

- Neovim **≥ 0.10**
- Rex **≥ 0.1.0-preview.21** (API revision 18), with a running server
- Rust **1.89** or newer to build the `rex-nvim` binary from source
- macOS or Linux. Windows is out of scope until the Rex app ships there.

## Install

Clone the repo and run the installer (any checkout path works; `install.sh`
records the repo root):

```sh
git clone https://github.com/emretuna/rex-nvim.git ~/Code/rex-nvim
cd ~/Code/rex-nvim
./install.sh
```

It builds `target/release/rex-nvim` (cargo; fails with a rustup hint if Rust is
missing or older than 1.89), copies it to `~/.local/bin/rex-nvim`, writes
`~/.local/state/rex-nvim/install.json`, links `~/.config/rex/rex-nvim` at this
repo's `rex/` directory, and adds this block to `~/.config/rex/init.lua` — only
if the file does not already load rex-nvim:

```lua
-- rex-nvim begin
require("rex-nvim").setup{ root = "/path/to/rex-nvim" }
-- rex-nvim end
```

It then runs `rex config check` and `rex config reload` (warnings, not
failures, when the server is unreachable). Run it again to upgrade;
`./install.sh --uninstall` stops the daemons and removes the marker block,
symlink, binary and `install.json`.

The nvim half installs with lazy.nvim:

```lua
{ "emretuna/rex-nvim", opts = {} }
```

Working from a local checkout instead? Point lazy at it:
`{ dir = "~/Code/rex-nvim", opts = {} }`.

With Neovim ≥ 0.12 you can use the built-in `vim.pack` instead of lazy.nvim
(`setup()` enables the `<leader>a` keymaps, same as lazy's `opts`):

```lua
vim.pack.add({ "https://github.com/emretuna/rex-nvim" })
require("rex-nvim").setup({
  -- diff = { resolvers = { my_resolver } }, -- extra resolvers, tried first
})
```

The sidebar daemon injects its own runtimepath, so the Rex half works without
a plugin manager; the nvim plugin is what provides `:Rex` and the comments.

### Keys

| Key | Does |
| --- | --- |
| `cmd+e` | show or hide the window's sidebar |
| `cmd+o` | pick a file the agent touched, open it in the sidebar |
| `<leader>ac` | comment the current line / selection |
| `<leader>al` | list comments (`⏎` edit, `d` delete, `q` close) |
| `<leader>as` | paste all comments into the agent's input |
| `<leader>aS` | send all comments to the agent (paste + submit) |
| `<leader>ai` | drop `path:line` at the agent's cursor |

The Rex keys are never bound over an existing `init.lua` binding. Pass
`setup{ keys = { toggle = "…", pick = "…" } }` to choose others, or `false` to
skip one. The nvim-side keys are configurable — see `:help rex-nvim-config`.

Git UIs are user-bound, not default keys: `rex_nvim_command` takes a `command`
line plus `view` (`split` or `sidebar`) and geometry args. In
`~/.config/rex/init.lua`, after the setup marker:

```lua
rex.bind("cmd+g", "rex_nvim_command", { command = "DiffviewOpen", view = "split", toggle = true })
rex.bind("shift+cmd+g", "rex_nvim_command", { command = "Neogit", view = "sidebar" })
```

## Navigation

smart-splits.nvim-style navigation, so you can use `require('rex-nvim')` in
place of `require('smart-splits')` in your keymaps:

```lua
require("rex-nvim").setup({ resize = { amount = 3 } })

-- resizing splits, these accept a count, so 10<A-h> resizes by 10 * config.resize.amount
vim.keymap.set('n', '<A-h>', require('rex-nvim').resize_left)
vim.keymap.set('n', '<A-j>', require('rex-nvim').resize_down)
vim.keymap.set('n', '<A-k>', require('rex-nvim').resize_up)
vim.keymap.set('n', '<A-l>', require('rex-nvim').resize_right)
-- moving between splits
vim.keymap.set('n', '<C-h>', require('rex-nvim').move_cursor_left)
vim.keymap.set('n', '<C-j>', require('rex-nvim').move_cursor_down)
vim.keymap.set('n', '<C-k>', require('rex-nvim').move_cursor_up)
vim.keymap.set('n', '<C-l>', require('rex-nvim').move_cursor_right)
```

At an nvim split edge, a move focuses the adjacent Rex pane and a resize
resizes the Rex split. `setup()` on the Rex side binds `ctrl+h/j/k/l` (move)
and `alt+h/j/k/l` (resize) for you and forwards them into the sidebar nvim
when it is focused, so navigation works both ways. Existing `init.lua`
bindings are never overridden; pass
`setup{ keys = { nav_left = "…", nav_down = false, … } }` to choose others, or
`false` to skip one. The Rex-side resize step is `setup{ resize_amount = N }`
(default 3).

Resize acts on the pane's immediate split border; at the window edge it is a
no-op.

## The sidebar and its daemon

The visible pane is `nvim --server <socket> --remote-ui`; the real nvim runs
headless behind it, one per Rex window. Toggling off calls
`session.detach_block` (the program keeps running), toggling on uses
`session.move_block` to put the same block back; a fresh pane is only created
when the old block is gone. State lives in
`<state>/s<first8><last8>_w<first8><last8>.json` — the short key keeps macOS
socket paths under the 104-byte `sun_path` limit.

Starting a daemon has a 10 s deadline. A daemon that is *already running* but
not answering is never replaced by a second nvim: `cmd+e` and the picker fail
with a message naming the stop command. Opening a file waits at most 10 s, then
unsticks a waiting nvim with `<C-\><C-n>` and retries for 5 s — 27 s worst case
for an open that hangs halfway through.

`rex-nvim daemons stop <key>` refuses a daemon with unsaved buffers or pending
comments; `--force` quits it and, if it will not exit, kills its process group
(LSP servers included). `daemons stop --all` and `--orphans` work the same way;
orphans are always forced, because their window is already gone. A daemon whose
socket answers with a vim error is `error` and is never stopped, not even with
`--force`.

A detached supervisor (`rex do <root>/rex/supervisor.lua --all`) reaps daemons
whose window or session disappears and reacts to `window_closed`,
`session_destroyed` and `block_closed`. It is relaunched by the next toggle or
picker after a server restart; the daemons themselves survive it.

## The file picker

`cmd+o` opens a floating layer above the window, in the agent pane's cwd, with
the cursor on the newest file, so `⏎` opens it with no typing. Rows show the
path relative to the cwd (home-relative outside it), an amber match highlight,
`+N -M` diff stats or a `new` badge, and a dim relative age.

Candidates come from:

1. the agent's session log, when it is reachable — pi/omp and Claude Code
   JSONL logs, mined for `read`/`write`/`edit` tool calls;
2. `git status` and commits made during the session, with diff stats;
3. the last screenful of the agent pane as a scrape fallback (codex and agy
   have no reachable log);
4. the repo pool (`git ls-files --cached --others --exclude-standard`) as you
   type, fuzzy-matched by `fff-search` in AI mode.

Your fff frecency database is consulted read-only (a shallow copy is opened;
the original is never written). Empty candidates close the picker and post a
notification instead of an empty chooser.

Picker keys: `↑↓`/`Ctrl+P,N` move, `PgUp/PgDn`/`Home/End` jump, `Ctrl+A/E`
edit the query, `Ctrl+U` clears it, `⏎` opens, `Esc` closes.

## Comments and references

Comments are extmarks: in-memory only, follow your edits, and clear after a
successful send. A characterwise selection inside one line is sent as
`path:12:5-9` (1-indexed byte columns) with only the selected text; a span over
several lines is `path:12-14`; linewise and blockwise selections comment whole
lines.

The prompt pasted into the agent looks like:

```text
Code review comments from my editor (repo: rex-nvim, branch: main):

1. lua/rex-nvim/prompt.lua:12-14
   > local function format_item(item)
   >   ...
   Comment: use `string.format` here

Please address each comment. Reply with what you changed per item.
```

`<leader>ai` sends just `path:12-14 ` (trailing space, no submit). Paths are
shortened against the receiving agent's cwd; a file outside it stays absolute.
A buffer with no file name is refused; unsaved changes warn (the agent reads
the file on disk) but still send.

Diff views are first-class too. Any buffer that is not a real file — diffview's
`diffview://` side, gitsigns revision buffers, fugitive blobs, Neogit's
`NeogitStatus` hunks, a bare scratch — can be commented: built-in resolvers
find the file behind the view, and the comment is snapshotted at capture, so
it survives the view being closed or wiped and reaches the agent as
`path (diff, working)` with the full selected diff text (no 3-line cap). When
nothing resolves, the comment still sends, labelled by scheme, filetype or
buftype instead of a pseudo-path. References on those buffers are refused —
comment the change instead; the `@@` headers carry the lines.

Anything else: `setup{ diff = { resolvers = { fn } } }`, where
`fn(bufnr, name) -> { path?, label } | nil` with `label` required. User
resolvers run before the built-ins (pcall-guarded, first non-nil wins), so one
function supports another plugin or overrides a built-in.

`:Rex comment|list|send|submit|ref` mirrors the keys with ranges, and
`require("rex-nvim").statusline()` returns `""` or `"● N"` for a statusline
component.

Agents are discovered from the session's terminal blocks: the foreground
program (`pi`, `omp`, `claude`, `codex`, `agy`, `antigravity`,
`antigravity_cli`, `gemini`) or a program-status record. When several exist and
the current pane is not one, nvim asks which. Sending to a `working` agent
warns first.

## Config

`~/.config/rex-nvim/config.toml` (optional; malformed values warn and fall back
to defaults). `REX_NVIM_CONFIG` points at another file when the binary
inherits it (direct invocations); rex actions run server-side, so they read
the default config path.

```toml
[sidebar]
nvim_bin = "nvim"       # the Neovim to run, by name or absolute path
nvim_env = []           # e.g. ["NVIM_APPNAME=myapp"] for ~/.config/myapp
position = "right"      # right | left | top | bottom

[picker]
scan_lines = 300        # agent screen lines mined by the scrape fallback
max_files = 20          # session rows shown before you type
frecency = true         # consult the fff frecency database read-only
```

Invalid `nvim_env` entries (keys must be ASCII alphanumerics or `_`) are
skipped with a warning. `nvim_env` applies to the daemon, the sidebar pane and
every probe.

The nvim plugin's options (`prefix`, `keymaps`, `clear_after_send`,
`diff.resolvers`) are documented in `:help rex-nvim-config`.

## Doctor

```sh
rex-nvim doctor                 # full self-check
rex-nvim doctor --with-agent omp
```

`doctor` prints the live daemon inventory, then runs in a throwaway session and
state directory: nvim version, toggle-on, daemon health, full-height layout,
`--remote-ui` attach, opening a file at line 2, toggle-off detach and reattach
at the default ratio. `--with-agent` additionally asserts a named agent pane is
visible in a real session (read-only). It always cleans up the scratch session,
daemon and temp state, and exits 1 if any check fails.

## Troubleshooting

- **`nvim` not found / wrong nvim:** the sidebar command is `[sidebar]
  nvim_bin`. The Homebrew prefixes (macOS and Linux) plus `~/.local/bin` and
  `~/.cargo/bin` are prepended to the PATH the daemon gets; a plain shell
  started from systemd or SSH may still lack them, so set an absolute path:
  `nvim_bin = "/opt/homebrew/bin/nvim"`.
- **Picker/daemon failures:** the finisher appends the reason to
  `~/.local/state/rex-nvim/finisher.log` (or `$XDG_STATE_HOME/rex-nvim`); the
  supervisor logs to `supervisor.log` beside it.
- **A pane that never answers:** `rex-nvim daemons` shows it as `unresponsive`;
  `rex-nvim daemons stop <key> --force` kills it. `stale` means the pid or
  socket is gone but a state file remains; `error` means nvim answered with a
  vim error and must be inspected.
- **Notifications:** rex-nvim posts OSC 9 through a short-lived unplaced block.
  If your terminal client has notifications off
  (Settings → System Notifications), failures are still written to
  `finisher.log`.
- **After a server restart:** the supervisor respawns on the next toggle or
  picker. Old daemons keep running until then; `rex-nvim daemons stop
  --orphans` cleans them up.
- **Keys do nothing:** `rex config check` lists the loaded actions and
  bindings. If `cmd+e`/`cmd+o` were already bound in `init.lua`, rex-nvim
  leaves them alone (by design); choose other keys in `setup{ keys = … }`.

## Deviations from [herdr-nvim](https://github.com/ChmaraX/herdr-nvim)

- **No ctrl+click / open-link.** Rex renders links in its client with no
  plugin hook, so there is nothing to intercept; `cmd+o` and the picker cover
  file opening.
- **macOS and Linux only.** Daemon stop uses Unix process groups.
- **Notifications** go through an ephemeral terminal block that prints OSC 9
  and exits, rather than a client notification API.

## Development

```sh
./install.sh                  # build + wire the Rex half
./install.sh --uninstall      # take it out
just ci                       # cargo fmt --check + cargo test + headless Lua suite
```

Without `just`:

```sh
cargo fmt --check
cargo test
nvim --headless --noplugin -u NONE -l tests/run.lua
```