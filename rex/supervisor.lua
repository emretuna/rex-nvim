-- rex-nvim: daemon lifecycle supervisor.
--
-- Run detached as: rex do <root>/rex/supervisor.lua --all
-- It reaps orphaned daemons at startup and reacts to window_closed,
-- session_destroyed and block_closed. Rex does not validate event names, so a
-- typo would register a handler that never fires; the names below are pinned.

-- Bootstrap module resolution from this file's own path: the launcher may run
-- before install.sh has made the ~/.config/rex/rex-nvim symlink.
local function bootstrap()
  local source = (debug.getinfo(1, "S") or {}).source or ""
  source = source:gsub("^@", "")
  local dir = source:match("^(.*)/[^/]*$") or "."
  local from_source = dir:gsub("/rex$", "")
  local env_root = os.getenv("REX_NVIM_ROOT")
  local arg_root = rex.args and rex.args.root
  local root = arg_root or (env_root ~= "" and env_root) or from_source

  local loaders = package.loaders or package.searchers
  if loaders then
    table.insert(loaders, 1, function(name)
      local module = name:match("^rex%-nvim%.(.+)$")
      if not module then return nil end
      local path = root .. "/rex/" .. module:gsub("%.", "/") .. ".lua"
      local f = io.open(path, "r")
      if not f then return nil end
      f:close()
      return loadfile(path)
    end)
  end
  return root, dir
end

local root = bootstrap()

local util = require("rex-nvim.util")
local sidebar = require("rex-nvim.sidebar")

if root and root ~= "" then util.set_root(root) end

-- Detached runs discard stdout/stderr, so the log goes to the state dir.
local function log(message)
  util.append_log(util.log_path("supervisor.log"),
    os.date("%Y-%m-%dT%H:%M:%S") .. " " .. tostring(message))
  if rex.log then rex.log("info", "rex-nvim supervisor: " .. tostring(message)) end
end

-- Startup: stop daemons whose window or session no longer exists, then sweep
-- socket files that have no state at all.
local function reap()
  local listed = rex.call("session.list") or {}
  local sessions = {}
  for _, session in ipairs(listed.sessions or {}) do
    sessions[session.session_id] = true
  end
  local states = util.load_all_states()
  log("reaping: " .. tostring(#states) .. " state file(s), "
    .. tostring(#(listed.sessions or {})) .. " session(s)")
  for _, state in ipairs(states) do
    local alive = sessions[state.session_id] == true
    if alive then
      local view, verr = util.view_via_cli(state.session_id)
      alive = false
      for _, window in ipairs((view and view.windows) or {}) do
        if window.window_id == state.window_id then
          alive = true
          break
        end
      end
      if not alive then
        log("window " .. tostring(state.window_id) .. " is gone from session "
          .. tostring(state.session_id) .. (verr and (" (" .. tostring(verr) .. ")") or ""))
      end
    else
      log("session " .. tostring(state.session_id) .. " is gone")
    end
    if not alive then
      log("reaping orphaned daemon " .. tostring(state.key))
      local ok, err = sidebar.stop_daemon(state.key, true)
      if not ok then log("could not stop " .. tostring(state.key) .. ": " .. tostring(err)) end
    end
  end
  for _, dir in ipairs({ util.state_dir() .. "/run", "/tmp/rex-nvim-" .. util.uid() .. "/run" }) do
    local listing = util.exec({ "ls", "-1", dir }, { timeout = 5 })
    if listing and listing.ok then
      for name in (listing.stdout or ""):gmatch("[^\r\n]+") do
        local key = name:match("^(s%w+_w%w+)%.sock$")
        if key and util.read_json(util.state_path(key)) == nil then
          log("sweeping socket " .. name)
          os.remove(dir .. "/" .. name)
        end
      end
    end
  end
end

local function stop_window(session_id, window_id)
  if not window_id then return end
  local key = util.key(session_id, window_id)
  if util.read_json(util.state_path(key)) then
    local ok, err = sidebar.stop_daemon(key, true)
    if not ok then log("could not stop " .. key .. ": " .. tostring(err)) end
  end
end

local function stop_session(session_id)
  if not session_id then return end
  for _, state in ipairs(util.load_all_states()) do
    if state.session_id == session_id then
      local ok, err = sidebar.stop_daemon(state.key, true)
      if not ok then log("could not stop " .. tostring(state.key) .. ": " .. tostring(err)) end
    end
  end
end

local function forget_sidebar_block(block_id)
  if not block_id then return end
  for _, state in ipairs(util.load_all_states()) do
    if state.sidebar_block_id == block_id then
      log("sidebar block " .. tostring(block_id) .. " closed; forgetting it for " .. tostring(state.key))
      state.sidebar_block_id = nil
      util.save_state(state)
    end
  end
end

rex.on("window_closed", function(ctx, ev)
  stop_window((ctx and ctx.session_id) or (ev and ev.session_id), ev and ev.window_id)
end)

rex.on("session_destroyed", function(ctx, ev)
  stop_session((ev and ev.session_id) or (ctx and ctx.session_id))
end)

rex.on("block_closed", function(ctx, ev)
  forget_sidebar_block(ev and ev.block_id)
end)

reap()

log("running")