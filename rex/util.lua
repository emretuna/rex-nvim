-- rex-nvim: shared helpers for the Rex half of the plugin.
--
-- Rex runs Lua 5.1 and forbids io.popen/os.execute, so every subprocess goes
-- through rex.exec. Nothing here may call nvim-only APIs: it all runs inside
-- the Rex server's config/script runtime.

local M = {}

local HOME = os.getenv("HOME") or ""

-- The Rex server may inherit a minimal PATH (launchd on macOS). Every command
-- we run gets these prefixes so `nvim`, `git` and friends resolve.
local PATH_PREFIXES = {
  "/opt/homebrew/bin",
  "/usr/local/bin",
  "/usr/bin",
  "/bin",
  "/home/linuxbrew/.linuxbrew/bin",
}
if HOME ~= "" then
  PATH_PREFIXES[#PATH_PREFIXES + 1] = HOME .. "/.local/bin"
  PATH_PREFIXES[#PATH_PREFIXES + 1] = HOME .. "/.cargo/bin"
end

M.DEFAULT_CONFIG = {
  sidebar = {
    nvim_bin = "nvim",
    nvim_env = {},
    position = "right",
  },
  picker = {
    scan_lines = 300,
    max_files = 20,
    frecency = true,
  },
}

local state_override
local root_override
local bin_override
local uid_cache

local function copy(value)
  if type(value) ~= "table" then return value end
  local out = {}
  for k, v in pairs(value) do out[k] = copy(v) end
  return out
end

function M.trim(text)
  if type(text) ~= "string" then return "" end
  return (text:gsub("^%s+", ""):gsub("%s+$", ""))
end

-- State and socket paths -----------------------------------------------------

-- Tests and `doctor` point the state directory somewhere scratch; the override
-- wins over the environment because Lua 5.1 has no os.setenv.
function M.set_state_dir(dir)
  state_override = dir
end

function M.default_state_dir()
  local env = os.getenv("REX_NVIM_STATE_DIR")
  if env and env ~= "" then return env end
  local xdg = os.getenv("XDG_STATE_HOME")
  if xdg and xdg ~= "" then return xdg .. "/rex-nvim" end
  return HOME .. "/.local/state/rex-nvim"
end

function M.state_dir()
  return state_override or M.default_state_dir()
end

function M.uid()
  if uid_cache then return uid_cache end
  local uid
  if rex.exec then
    local r = rex.exec({ "id", "-u" }, { timeout = 5 })
    if r and r.ok then uid = M.trim(r.stdout) end
  end
  if not uid or uid == "" then uid = "0" end
  uid_cache = uid
  return uid
end

-- macOS caps sun_path at 104 bytes; stay under 100 to be safe.
function M.socket_dir(key)
  local dir = M.state_dir() .. "/run"
  if #(dir .. "/" .. key .. ".sock") > 100 then
    return "/tmp/rex-nvim-" .. M.uid() .. "/run"
  end
  return dir
end

function M.socket_path(key)
  return M.socket_dir(key) .. "/" .. key .. ".sock"
end

function M.state_path(key)
  return M.state_dir() .. "/" .. key .. ".json"
end

function M.log_path(name)
  return M.state_dir() .. "/" .. name
end

-- Rex IDs are UUIDv7: chars 1-8 are timestamp bits that only change every
-- ~65 seconds, so using them alone collides for windows created in one minute
-- (observed: three windows in one script shared a key and overwrote each
-- other's state). Keep the timestamp prefix for traceability and add 32 random
-- bits from the tail; the result still fits a macOS socket path.
local function short_id(id)
  id = tostring(id or "")
  id = id:gsub("^session:", ""):gsub("^window:", ""):gsub("^block:", ""):gsub("-", "")
  return id:sub(1, 8) .. id:sub(-8)
end

function M.key(session_id, window_id)
  return "s" .. short_id(session_id) .. "_w" .. short_id(window_id)
end

-- Filesystem ----------------------------------------------------------------

function M.ensure_dir(dir)
  if not dir or dir == "" then return false end
  local r = M.exec({ "mkdir", "-p", dir }, { timeout = 5 })
  return r ~= nil and r.ok
end

function M.read_json(path)
  local f = io.open(path, "r")
  if not f then return nil end
  local text = f:read("*a")
  f:close()
  if not text or text == "" then return nil end
  local value = rex.json.decode(text)
  if type(value) ~= "table" then return nil end
  return value
end

function M.write_json_atomic(path, value)
  local text, err = rex.json.encode(value)
  if not text then return nil, err end
  local dir = path:match("^(.*)/[^/]*$")
  if dir then M.ensure_dir(dir) end
  local tmp = path .. ".tmp"
  local f, ferr = io.open(tmp, "w")
  if not f then return nil, ferr end
  f:write(text)
  f:close()
  local ok, rerr = os.rename(tmp, path)
  if not ok then return nil, rerr end
  return true
end

function M.save_state(state)
  if not state or not state.key then return nil, "state has no key" end
  return M.write_json_atomic(M.state_path(state.key), state)
end

function M.remove_state(key)
  local path = M.state_path(key)
  os.remove(path)
  os.remove(path .. ".tmp")
end

function M.remove_socket(key)
  os.remove(M.socket_path(key))
end

function M.load_all_states()
  local dir = M.state_dir()
  local out = {}
  local r = M.exec({ "ls", "-1", dir }, { timeout = 5 })
  if not r or not r.ok then return out end
  for name in (r.stdout or ""):gmatch("[^\r\n]+") do
    if name:match("^s%w+_w%w+%.json$") then
      local state = M.read_json(dir .. "/" .. name)
      if state and state.key then out[#out + 1] = state end
    end
  end
  return out
end

function M.append_log(path, line)
  local f = io.open(path, "a")
  if not f then return false end
  f:write(line, "\n")
  f:close()
  return true
end

function M.log_finisher(message)
  M.append_log(M.log_path("finisher.log"),
    os.date("%Y-%m-%dT%H:%M:%S") .. " " .. tostring(message))
end

-- Commands ------------------------------------------------------------------

function M.augment_path(path)
  local parts = {}
  for _, entry in ipairs(PATH_PREFIXES) do parts[#parts + 1] = entry end
  if path and path ~= "" then parts[#parts + 1] = path end
  return table.concat(parts, ":")
end

local function build_env(env_in)
  local env = {}
  for k, v in pairs(env_in or {}) do env[k] = v end
  env.PATH = M.augment_path(env.PATH or os.getenv("PATH"))
  return env
end

function M.exec(argv, opts)
  opts = opts or {}
  local options = {
    env = build_env(opts.env),
    timeout = opts.timeout or 15,
  }
  if opts.cwd then options.cwd = opts.cwd end
  if opts.stdin ~= nil then options.stdin = opts.stdin end
  if opts.stdout == false then options.stdout = false end
  if opts.stderr == false then options.stderr = false end
  local r, err = rex.exec(argv, options)
  if r == nil then return nil, err end
  return r
end

function M.exec_start(argv, opts)
  opts = opts or {}
  local options = {
    env = build_env(opts.env),
    detach = opts.detach ~= false,
  }
  if opts.cwd then options.cwd = opts.cwd end
  if opts.stdout ~= nil then options.stdout = opts.stdout end
  if opts.stderr ~= nil then options.stderr = opts.stderr end
  return rex.exec.start(argv, options)
end

-- rex.sleep only exists in `rex do` scripts; config actions need a command.
function M.sleep(seconds)
  if rex.sleep then
    rex.sleep(seconds)
    return true
  end
  local r = M.exec({ "sleep", tostring(seconds) }, { timeout = math.max(10, seconds + 10) })
  return r ~= nil and r.ok
end

function M.now()
  return os.time()
end

function M.kill0(pid)
  if not pid then return false end
  local r = M.exec({ "kill", "-0", tostring(pid) }, { timeout = 5 })
  return r ~= nil and r.ok
end

-- Install info and config ---------------------------------------------------

function M.set_root(root)
  root_override = root
end

function M.set_bin(bin)
  bin_override = bin
end

function M.install_info()
  return M.read_json(M.state_dir() .. "/install.json")
end

function M.root()
  if root_override then return root_override end
  local info = M.install_info()
  return info and info.root
end

function M.bin()
  if bin_override then return bin_override end
  local info = M.install_info()
  if info and info.bin and info.bin ~= "" then return info.bin end
  return "rex-nvim"
end

-- install.sh records the absolute rex path. Before install (and in tests) the
-- Rex server may run with a PATH that does not contain it, so resolve once.
local rex_resolved

function M.rex_bin()
  local info = M.install_info()
  if info and info.rex and info.rex ~= "" then return info.rex end
  if rex_resolved then return rex_resolved end
  local r = M.exec({ "which", "rex" }, { timeout = 5 })
  local path = r and r.ok and M.trim(r.stdout) or nil
  rex_resolved = (path and path ~= "" and path) or "rex"
  return rex_resolved
end

-- `rex-nvim config --json` fills the defaults; merge them anyway so a missing
-- or older binary still yields a complete table.
function M.config()
  local bin = M.bin()
  local r = M.exec({ bin, "config", "--json" }, { timeout = 10 })
  local value
  if r and r.ok and (r.stdout or "") ~= "" then
    value = rex.json.decode(r.stdout)
  end
  local cfg = copy(M.DEFAULT_CONFIG)
  if type(value) ~= "table" then return cfg end
  if type(value.sidebar) == "table" then
    for k, v in pairs(value.sidebar) do cfg.sidebar[k] = v end
  end
  if type(value.picker) == "table" then
    for k, v in pairs(value.picker) do cfg.picker[k] = v end
  end
  if type(cfg.sidebar.nvim_env) ~= "table" then cfg.sidebar.nvim_env = {} end
  return cfg
end

function M.nvim_env()
  local cfg = M.config()
  return cfg.sidebar.nvim_env or {}
end

-- Terminal helpers ----------------------------------------------------------

-- `block.method` needs both IDs; rex.block.call would target the calling block.
function M.block_method(session_id, block_id, method, args)
  local result, err = rex.call("block.method", {
    session_id = session_id,
    block_id = block_id,
    method = method,
    args = args or {},
  })
  if result == nil then return nil, err end
  return result
end

-- OSC 9 must be block OUTPUT: `write` types into the program. An ephemeral
-- unplaced block prints the sequence and removes itself when the program exits.
function M.notify(session_id, block_id, message)
  local text = tostring(message):gsub("[\r\n]+", " ")
  local delivered = false
  if session_id then
    local command = {
      "sh", "-c",
      "printf '\\033]9;%s\\007' \"$1\"; sleep 0.3", "sh", text,
    }
    local result = rex.call("session.new_block", {
      session_id = session_id,
      flavor = "com.superlogical.terminal.shell",
      label = "rex-nvim notify",
      options = { command = command, shell = "none" },
    })
    delivered = result ~= nil
  end
  M.append_log(M.log_path("finisher.log"),
    os.date("%Y-%m-%dT%H:%M:%S") .. " notify: " .. text)
  if rex.log then
    rex.log(delivered and "info" or "warn",
      "rex-nvim: notification " .. (delivered and "sent: " or "failed: ") .. text)
  end
  return delivered
end

-- Sessions ------------------------------------------------------------------

-- A `--all` script is attached to only one session, so session.view raises 409
-- for the others. A fresh `rex api call -s` connection attaches to the session
-- it names; that is the same path `rex-nvim daemons` uses.
function M.view_via_cli(session_id)
  local r = M.exec({
    M.rex_bin(), "api", "call", "-s", session_id, "session.view", "{}",
  }, { timeout = 15 })
  if not r or not r.ok then
    return nil, (r and M.trim(r.stderr ~= "" and r.stderr or r.stdout)) or "rex api call failed"
  end
  local view = rex.json.decode(r.stdout)
  if type(view) ~= "table" then return nil, "invalid session.view output" end
  return view
end

function M.view(session_id)
  local view, err = rex.session.view({ session_id = session_id })
  if view == nil then return nil, err end
  return view
end

function M.find_window(view, window_id)
  if not view or not view.windows then return nil end
  for _, window in ipairs(view.windows) do
    if window.window_id == window_id then return window end
  end
  return nil
end

function M.window_for_block(view, block_id)
  if not view or not view.windows or not block_id then return nil end
  for _, window in ipairs(view.windows) do
    for _, layer in ipairs(window.layers or {}) do
      for _, block in ipairs(layer.blocks or {}) do
        if block.block_id == block_id then return window end
      end
    end
  end
  return nil
end

function M.block_in_window(window, block_id)
  if not window or not block_id then return false end
  for _, layer in ipairs(window.layers or {}) do
    for _, block in ipairs(layer.blocks or {}) do
      if block.block_id == block_id then return true end
    end
  end
  return false
end

function M.placed_blocks(session_id)
  local result, err = rex.session.list_blocks({ session_id = session_id })
  if result == nil then return nil, err end
  return result.blocks or {}
end

function M.block_placement(session_id, block_id)
  local blocks, err = M.placed_blocks(session_id)
  if not blocks then return nil, err end
  for _, block in ipairs(blocks) do
    if block.block_id == block_id then return block end
  end
  return nil
end

-- Supervisor ----------------------------------------------------------------

function M.supervisor_pid()
  local f = io.open(M.log_path("supervisor.pid"), "r")
  if not f then return nil end
  local text = f:read("*a")
  f:close()
  local digits = text and text:match("%d+")
  return digits and tonumber(digits) or nil
end

-- kill0 alone isn't enough: PIDs get reused. The command line must still be
-- our supervisor script.
function M.supervisor_alive()
  local pid = M.supervisor_pid()
  if not pid then return nil end
  if not M.kill0(pid) then return nil end
  local r = M.exec({ "ps", "-p", tostring(pid), "-o", "args=" }, { timeout = 5 })
  if not r or not r.ok then return nil end
  if not (r.stdout or ""):find("supervisor.lua", 1, true) then return nil end
  return pid
end

function M.ensure_supervisor()
  if M.supervisor_alive() then return true end
  local root = M.root()
  if not root then return nil, "rex-nvim install.json not found; run install.sh" end
  local script = root .. "/rex/supervisor.lua"
  local f = io.open(script, "r")
  if not f then return nil, "supervisor script not found at " .. script end
  f:close()
  M.ensure_dir(M.state_dir())
  local handle, err = M.exec_start({ M.rex_bin(), "do", script, "--all" }, {
    detach = true,
    stdout = false,
    stderr = false,
    env = {
      REX_NVIM_STATE_DIR = M.state_dir(),
      REX_NVIM_ROOT = root,
    },
  })
  if not handle then return nil, "could not start the rex-nvim supervisor: " .. tostring(err) end
  local out = io.open(M.log_path("supervisor.pid"), "w")
  if out then
    out:write(tostring(handle.pid))
    out:close()
  end
  return true
end

return M