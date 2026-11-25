-- rex-nvim: the per-window nvim sidebar daemon on the Rex side.
--
-- One nvim process per Rex window, listening on a socket; the visible pane is
-- `nvim --server <socket> --remote-ui`. Hiding the sidebar detaches the block
-- (the daemon keeps running), showing it moves the detached block back beside
-- the anchor block, so buffers, cursor and comments survive a toggle.

local util = require("rex-nvim.util")

local M = {}

local SHELL = "com.superlogical.terminal.shell"

-- right|left|top|bottom -> split geometry
M.POSITIONS = {
  right = { direction = "horizontal", side = "after" },
  left = { direction = "horizontal", side = "before" },
  top = { direction = "vertical", side = "before" },
  bottom = { direction = "vertical", side = "after" },
}

-- One round trip: pid, unsaved listed buffers, pending comments, window id.
-- Reads package.loaded so it never loads the comments module.
M.PROBE_EXPR = [[join([getpid(), len(filter(getbufinfo({'bufmodified':1}),'v:val.listed')), luaeval("(function() local c = package.loaded['rex-nvim.comments'] if not c then return 0 end local ok, l = pcall(c.list) return ok and #l or 0 end)()"), $REX_WINDOW_ID], ' ')]]

-- Timeouts (seconds), pinned by the plan.
M.PROBE_FIRST_TIMEOUT = 2
M.PROBE_TIMEOUT = 5
M.START_DEADLINE = 10
M.START_POLL = 0.1
M.STOP_PROBE_TIMEOUT = 5
M.STOP_QUIT_DEADLINE = 10
M.UNSTICK_TIMEOUT = 2
M.OPEN_TIMEOUT = 10
M.OPEN_RETRY_TIMEOUT = 5
M.COMMAND_TIMEOUT = 10
M.LOCK_WAIT = 25
M.KILL_WAIT = 2
M.RATIO = 0.5

local function eval_expr(state, expr, timeout)
  return util.exec({ state.nvim_bin, "--server", state.socket, "--remote-expr", expr },
    { timeout = timeout })
end

local function send_keys(state, keys, timeout)
  return util.exec({ state.nvim_bin, "--server", state.socket, "--remote-send", keys },
    { timeout = timeout })
end

M.send_keys = send_keys

-- Connection failures and vim errors both exit non-zero; tell them apart.
local function classify(r)
  if r == nil then return "gone" end
  if r.timed_out then return "unresponsive" end
  if r.code == 0 then return "answered" end
  local text = ((r.stderr or "") .. (r.stdout or "")):lower()
  if text:find("failed to connect", 1, true)
    or text:find("connection refused", 1, true)
    or text:find("no such file or directory", 1, true)
  then
    return "gone"
  end
  return "error"
end

local function reason(r)
  if r == nil then return "nvim could not run" end
  local text = util.trim((r.stderr ~= "" and r.stderr) or (r.stdout or ""))
  if text == "" then text = "exit status " .. tostring(r.code) end
  return text
end

local function daemon_error(state, detail)
  return "the nvim daemon for " .. tostring(state.key) .. " answered with an error ("
    .. tostring(util.trim(detail)) .. "); stop it with: rex-nvim daemons stop "
    .. tostring(state.key) .. " --force"
end

local function unresponsive_error(state)
  return "the nvim daemon for " .. tostring(state.key)
    .. " is not responding; stop it with: rex-nvim daemons stop "
    .. tostring(state.key) .. " --force"
end

function M.probe(state, timeout)
  local r = eval_expr(state, "getpid()", timeout or M.PROBE_TIMEOUT)
  local health = classify(r)
  if health == "answered" then return "answered", util.trim(r.stdout) end
  if health == "error" then return "error", reason(r) end
  if health == "unresponsive" then return "unresponsive", "probe timed out" end
  return "gone", reason(r)
end

function M.probe_stats(state)
  local r = eval_expr(state, M.PROBE_EXPR, M.PROBE_TIMEOUT)
  if classify(r) ~= "answered" then return nil, reason(r) end
  local pid, unsaved, comments = util.trim(r.stdout):match("^(%-?%d+)%s+(%d+)%s+(%d+)")
  if not pid then return nil, "unexpected probe output" end
  return {
    pid = tonumber(pid),
    unsaved = tonumber(unsaved),
    comments = tonumber(comments),
  }
end

-- Keep state.pid honest if the socket belongs to another nvim than we recorded.
local function adopt(state, detail)
  local actual = tonumber(detail)
  if actual and actual ~= state.pid then
    state.pid = actual
    util.save_state(state)
  end
  return true
end

local function vim_path_escape(path)
  return (path:gsub("\\", "\\\\"):gsub(",", "\\,"):gsub(" ", "\\ "))
end

local function lua_quote(value)
  return (tostring(value):gsub("\\", "\\\\"):gsub("'", "\\'"))
end

local function daemon_env(state)
  local env = {
    REX_SESSION = state.session_id,
    REX_WINDOW_ID = state.window_id,
    REX_NVIM_KEY = state.key,
    REX_NVIM_BIN = util.bin(),
    REX_BIN = util.rex_bin(),
    PATH = util.augment_path(os.getenv("PATH")),
  }
  for _, entry in ipairs(util.nvim_env()) do
    local key, value = tostring(entry):match("^([%w_]+)=(.*)$")
    if key then env[key] = value end
  end
  return env
end

-- `nvim --server ... --remote-ui` as typed into the pane. nvim_env rides along
-- through `env`; a bare nvim_bin also gets the augmented PATH.
function M.sidebar_command(state)
  local env = {}
  for _, entry in ipairs(util.nvim_env()) do
    if tostring(entry):match("^[%w_]+=") then env[#env + 1] = tostring(entry) end
  end
  if not tostring(state.nvim_bin):find("/", 1, true) then
    local has_path = false
    for _, entry in ipairs(env) do
      if entry:match("^PATH=") then has_path = true end
    end
    if not has_path then env[#env + 1] = "PATH=" .. util.augment_path(os.getenv("PATH")) end
  end
  local cmd = {}
  if #env > 0 then
    cmd[#cmd + 1] = "env"
    for _, entry in ipairs(env) do cmd[#cmd + 1] = entry end
  end
  cmd[#cmd + 1] = state.nvim_bin
  cmd[#cmd + 1] = "--server"
  cmd[#cmd + 1] = state.socket
  cmd[#cmd + 1] = "--remote-ui"
  return cmd
end

function M.spawn_daemon(state)
  local root = util.root()
  if not root then return nil, "rex-nvim install.json not found; run install.sh" end
  util.ensure_dir(util.socket_dir(state.key))
  os.remove(state.socket)
  local command = {
    state.nvim_bin,
    "--headless",
    "--listen", state.socket,
    "--cmd", "if exists('+messagesopt') | set messagesopt=history:500,wait:0 | endif",
    "--cmd", "set nomore",
    "--cmd", "set rtp+=" .. vim_path_escape(root),
    "--cmd", "lua vim.api.nvim_create_autocmd('VimEnter',{callback=function() pcall(function() "
      .. "vim.opt.rtp:append('" .. lua_quote(root) .. "'); require('rex-nvim').setup() end) end})",
  }
  local handle, err = util.exec_start(command, {
    detach = true,
    cwd = state.cwd,
    stdout = false,
    stderr = false,
    env = daemon_env(state),
  })
  if not handle then return nil, "could not start nvim: " .. tostring(err) end
  state.pid = handle.pid
  util.save_state(state)
  local deadline_at = util.now() + M.START_DEADLINE
  while util.now() < deadline_at do
    local health, detail = M.probe(state, M.PROBE_FIRST_TIMEOUT)
    if health == "answered" then return adopt(state, detail) end
    util.sleep(M.START_POLL)
  end
  return nil, "nvim did not answer on " .. state.socket .. " within "
    .. tostring(M.START_DEADLINE) .. "s; see " .. util.log_path("finisher.log")
end

-- Per-key mutex for the daemon/pane/command critical sections. Two rapid
-- presses must not double-spawn nvim on one socket nor queue heavy commands
-- until a client times out. Plain mkdir is atomic; LOCK_WAIT outlasts the
-- worst-case holder (spawn + command), so takeover only hits crashed holders.
local function with_key_lock(state, fn)
  local lock = util.state_dir() .. "/" .. state.key .. ".lock"
  local deadline_at = util.now() + M.LOCK_WAIT
  while true do
    local r = util.exec({ "mkdir", lock }, { timeout = 5 })
    if r and r.ok then break end
    if util.now() > deadline_at then
      os.remove(lock)
      local r2 = util.exec({ "mkdir", lock }, { timeout = 5 })
      if r2 and r2.ok then break end
      return nil, "another rex-nvim action is busy on " .. tostring(state.key)
    end
    util.sleep(0.1)
  end
  local ok, a, b = pcall(fn)
  os.remove(lock)
  if not ok then error(a, 0) end
  return a, b
end

M.with_key_lock = with_key_lock

-- Read-modify-write strictly under the lock: merge the persisted mutable
-- fields onto the action's state copy so a save never clobbers a field a
-- concurrent locked action just wrote.
local function refresh_state(state)
  local fresh = util.read_json(util.state_path(state.key))
  if fresh then
    state.pid = fresh.pid
    state.sidebar_block_id = fresh.sidebar_block_id
    state.cwd = fresh.cwd or state.cwd
    state.origin_block_id = fresh.origin_block_id or state.origin_block_id
    state.ratio = fresh.ratio or state.ratio
  end
  return state
end

-- Health gate: never spawn a second nvim on top of one that is answering.
-- Caller holds the key lock (M.ensure_daemon / M.run_command wrap it).
local function ensure_daemon_locked(state)
  refresh_state(state)
  -- A live recorded pid is authoritative: adopt, report, or unstick -- never
  -- spawn a second nvim on top of one that is merely busy.
  if state.pid and util.kill0(state.pid) then
    local health, detail = M.probe(state, M.PROBE_FIRST_TIMEOUT)
    if health == "answered" then return adopt(state, detail) end
    if health == "error" then return nil, daemon_error(state, detail) end
    if health == "unresponsive" then
      send_keys(state, "<C-\\><C-n><Esc><C-c><CR>", M.UNSTICK_TIMEOUT)
      local deadline_at = util.now() + M.START_DEADLINE
      while util.now() < deadline_at do
        local remaining = deadline_at - util.now()
        if remaining <= 0 then break end
        local h2, d2 = M.probe(state, math.max(1, math.min(M.PROBE_TIMEOUT, remaining)))
        if h2 == "answered" then return adopt(state, d2) end
        if h2 == "error" then return nil, daemon_error(state, d2) end
        if h2 == "gone" then break end
        util.sleep(0.2)
      end
      if util.kill0(state.pid) then return nil, unresponsive_error(state) end
    end
  end
  -- No live pid answered: a concurrent press may have spawned one whose pid
  -- we have not recorded; probe the socket, then spawn if nothing answers.
  local health, detail = M.probe(state, M.PROBE_FIRST_TIMEOUT)
  if health == "answered" then return adopt(state, detail) end
  return M.spawn_daemon(state)
end

function M.ensure_daemon(state)
  return with_key_lock(state, function() return ensure_daemon_locked(state) end)
end

-- State ---------------------------------------------------------------------

function M.cwd_for(session_id, block_id)
  if block_id then
    local proc = util.block_method(session_id, block_id, "process")
    local cwd = proc and proc.foreground and proc.foreground.cwd
    if cwd and cwd ~= "" then return cwd end
    local pwd = util.block_method(session_id, block_id, "pwd")
    local path = pwd and pwd.pwd and pwd.pwd:match("^file://[^/]*(/.+)$")
    if path and path ~= "" then return path end
  end
  return os.getenv("HOME") or "/"
end

function M.load_state(session_id, window_id)
  return util.read_json(util.state_path(util.key(session_id, window_id)))
end

function M.state(session_id, window_id, block_id)
  local key = util.key(session_id, window_id)
  local cfg = util.config()
  local state = util.read_json(util.state_path(key))
  if state then
    state.key = key
    state.session_id = session_id
    state.window_id = window_id
    state.socket = state.socket or util.socket_path(key)
    state.nvim_bin = cfg.sidebar.nvim_bin
    state.position = cfg.sidebar.position
    state.ratio = state.ratio or M.RATIO
    if state.cwd == nil or state.cwd == "" then
      state.cwd = M.cwd_for(session_id, block_id)
    end
    return state
  end
  state = {
    session_id = session_id,
    window_id = window_id,
    key = key,
    socket = util.socket_path(key),
    pid = nil,
    nvim_bin = cfg.sidebar.nvim_bin,
    cwd = M.cwd_for(session_id, block_id),
    origin_block_id = block_id,
    sidebar_block_id = nil,
    created_at = util.now(),
    position = cfg.sidebar.position,
    ratio = M.RATIO,
  }
  util.save_state(state)
  return state
end

local function anchor_for(window, state)
  if state.origin_block_id
    and state.origin_block_id ~= state.sidebar_block_id
    and util.block_in_window(window, state.origin_block_id)
  then
    return state.origin_block_id
  end
  for _, layer in ipairs(window.layers or {}) do
    for _, block in ipairs(layer.blocks or {}) do
      if block.block_id ~= state.sidebar_block_id then
        return block.block_id
      end
    end
  end
  return nil
end

-- The pane is `nvim --server … --remote-ui`. If that client exited (the daemon
-- died under it) the block is a corpse that move_block cannot revive.
local function pane_exited(session_id, block_id)
  local process = util.block_method(session_id, block_id, "process")
  if not process then return false end
  return process.last_exit ~= nil
end

-- The toggle-ON branch, idempotent: reattach or create, never hide.
local function ensure_open_inner(state)
  local view, verr = util.view(state.session_id)
  if not view then return nil, "could not read the session view: " .. tostring(verr) end
  local window = util.find_window(view, state.window_id)
  if not window then
    return nil, "window " .. tostring(state.window_id) .. " no longer exists"
  end
  local sidebar = state.sidebar_block_id
  if sidebar and pane_exited(state.session_id, sidebar) then
    rex.call("block.close", { session_id = state.session_id, block_id = sidebar })
    sidebar = nil
    state.sidebar_block_id = nil
    util.save_state(state)
  end
  if sidebar and util.block_in_window(window, sidebar) then
    rex.session.focus_block({ block_id = sidebar, session_id = state.session_id })
    return sidebar
  end
  local anchor = anchor_for(window, state)
  if not anchor then return nil, "no block to anchor the sidebar next to" end
  local cfg = util.config()
  state.position = cfg.sidebar.position
  local geometry = M.POSITIONS[cfg.sidebar.position] or M.POSITIONS.right
  local exists = sidebar and util.block_placement(state.session_id, sidebar) ~= nil
  if exists then
    local _, merr = rex.session.move_block({
      block_id = sidebar,
      anchor_block_id = anchor,
      direction = geometry.direction,
      side = geometry.side,
      ratio = state.ratio,
      session_id = state.session_id,
    })
    if merr then return nil, "could not reattach the sidebar: " .. tostring(merr) end
  else
    local created, serr = rex.session.new_split({
      anchor_id = anchor,
      direction = geometry.direction,
      side = geometry.side,
      ratio = M.RATIO,
      focus = true,
      session_id = state.session_id,
      layout = rex.layout.block({
        flavor = SHELL,
        options = {
          command = M.sidebar_command(state),
          shell = "none",
          cwd = state.cwd,
        },
      }),
    })
    if not created then
      return nil, "could not create the sidebar: " .. tostring(serr)
    end
    sidebar = created.block_ids and created.block_ids[1]
    if not sidebar then
      return nil, "could not create the sidebar: the server returned no block"
    end
    state.sidebar_block_id = sidebar
  end
  util.save_state(state)
  rex.session.focus_block({ block_id = sidebar, session_id = state.session_id })
  return sidebar
end

-- Refresh state and reattach or create one pane. Caller holds the key lock.
local function ensure_open_locked(state)
  refresh_state(state)
  return ensure_open_inner(state)
end

-- Serialize pane creation per key: two rapid presses reattach one pane.
function M.ensure_open(state)
  return with_key_lock(state, function() return ensure_open_locked(state) end)
end

-- Supervisor + view + window + state file: the shared prefix of every
-- "do something in this window's sidebar" action.
local function resolve_state(ctx)
  local session_id = (ctx and ctx.session_id) or rex.session_id
  local block_id = ctx and ctx.block_id
  local sup_ok, sup_err = util.ensure_supervisor()
  if not sup_ok then return nil, sup_err end
  local view, verr = util.view(session_id)
  if not view then return nil, "could not read the session view: " .. tostring(verr) end
  local window = (block_id and util.window_for_block(view, block_id))
    or util.find_window(view, view.active_window_id)
  if not window then return nil, "the session has no windows" end
  return M.state(session_id, window.window_id, block_id), nil, window
end

-- Detach the sidebar pane and restore focus. Keeps sidebar_block_id so the
-- next show reattaches the same block (and its daemon).
local function hide_sidebar(state, window, block_id)
  local sidebar = state.sidebar_block_id
  local _, derr = rex.session.detach_block({
    block_id = sidebar,
    session_id = state.session_id,
  })
  if derr then return nil, "could not hide the sidebar: " .. tostring(derr) end
  local back = block_id
  if not back or back == sidebar or not util.block_in_window(window, back) then
    local fresh = util.view(state.session_id)
    local after = fresh and util.find_window(fresh, state.window_id)
    back = after and after.focused_block_id
  end
  if back then rex.session.focus_block({ block_id = back, session_id = state.session_id }) end
  return { key = state.key, visible = false, block_id = sidebar }
end

function M.toggle(ctx)
  local block_id = ctx and ctx.block_id
  local state, err, window = resolve_state(ctx)
  if not state then return nil, err end
  local sidebar = state.sidebar_block_id
  if sidebar and util.block_in_window(window, sidebar) then
    return hide_sidebar(state, window, block_id)
  end
  local ok, oerr = M.ensure_daemon(state)
  if not ok then return nil, oerr end
  local opened, open_err = M.ensure_open(state)
  if not opened then return nil, open_err end
  return { key = state.key, visible = true, block_id = opened }
end

-- Run a nvim command line (no leading colon) in the window's sidebar,
-- showing the pane first. cd to the calling pane's cwd so repo-scoped
-- commands like :Neogit or :DiffviewOpen target the pane's repository.
function M.run_command(ctx, command, toggle)
  if type(command) ~= "string" or command == "" then
    return nil, "command is required (a nvim command line without the leading colon)"
  end
  local state, err = resolve_state(ctx)
  if not state then return nil, err end
  local block_id = ctx and ctx.block_id
  -- One lock over the whole flow INCLUDING the toggle decision: a second
  -- press arriving mid-open sees the persisted pane and hides, instead of
  -- running the command again and leaving the sidebar open.
  return with_key_lock(state, function()
    refresh_state(state)
    local view, verr = util.view(state.session_id)
    if not view then return nil, "could not read the session view: " .. tostring(verr) end
    local window = util.find_window(view, state.window_id)
    if not window then return nil, "window " .. tostring(state.window_id) .. " no longer exists" end
    if toggle and state.sidebar_block_id and util.block_in_window(window, state.sidebar_block_id) then
      return hide_sidebar(state, window, block_id)
    end
    local ok, derr = ensure_daemon_locked(state)
    if not ok then return nil, derr end
    local opened, oerr = ensure_open_locked(state)
    if not opened then return nil, oerr end
    local cwd = M.cwd_for(state.session_id, block_id)
    local expr = "[nvim_command('cd ' . fnameescape('" .. cwd:gsub("'", "''")
      .. "')), nvim_command('" .. command:gsub("'", "''") .. "')]"
    local r = eval_expr(state, expr, M.COMMAND_TIMEOUT)
    if r and r.timed_out then
      return nil, "timed out running :" .. command .. " in the sidebar nvim (it may be busy)"
    end
    if not r or not r.ok then
      return nil, "could not run :" .. command .. " in the sidebar nvim: " .. reason(r)
    end
    return { key = state.key, block_id = opened, command = command }
  end)
end

-- Opening files -------------------------------------------------------------

local function ensure_absolute(path, cwd)
  if path:sub(1, 1) == "/" then return path end
  if path:sub(1, 2) == "~/" then return (os.getenv("HOME") or "") .. path:sub(2) end
  return (cwd or "/") .. "/" .. path
end

function M.open_expr(path, line, col)
  return string.format(
    "[nvim_command('drop ' . fnameescape('%s')), cursor(%d, %d)]",
    (path:gsub("'", "''")),
    math.floor(tonumber(line) or 1),
    math.floor(tonumber(col) or 1))
end

local function notify_failure(session_id, block_id, message)
  util.notify(session_id, block_id, message)
  util.log_finisher(message)
end

function M.open_file(args, session_id)
  args = args or {}
  session_id = args.session_id or session_id or rex.session_id
  local path = args.path
  if type(path) ~= "string" or path == "" then return nil, "no path given" end
  local view, verr = util.view(session_id)
  if not view then return nil, "could not read the session view: " .. tostring(verr) end
  local window_id = args.window_id or view.active_window_id
  if not window_id then return nil, "the session has no windows" end
  local state = M.state(session_id, window_id, args.origin_block_id)
  path = ensure_absolute(path, state.cwd)
  local target = args.origin_block_id
  if not target then
    local window = util.find_window(view, window_id)
    target = window and window.focused_block_id
  end
  local ok, derr = M.ensure_daemon(state)
  if not ok then
    notify_failure(session_id, target, "Could not open " .. path .. ": " .. tostring(derr))
    return nil, derr
  end
  local sidebar, oerr = M.ensure_open(state)
  if not sidebar then
    notify_failure(session_id, target, "Could not open " .. path .. ": " .. tostring(oerr))
    return nil, oerr
  end
  local expr = M.open_expr(path, args.line, args.col)
  local r = eval_expr(state, expr, M.OPEN_TIMEOUT)
  if r and r.timed_out then
    send_keys(state, "<C-\\><C-n>", M.UNSTICK_TIMEOUT)
    r = eval_expr(state, expr, M.OPEN_RETRY_TIMEOUT)
  end
  if r == nil or r.code ~= 0 or r.timed_out then
    local failure = reason(r)
    notify_failure(session_id, target, "Could not open " .. path .. ": " .. failure)
    return nil, failure
  end
  rex.session.focus_block({ block_id = sidebar, session_id = session_id })
  return {
    key = state.key,
    path = path,
    line = math.floor(tonumber(args.line) or 1),
    col = math.floor(tonumber(args.col) or 1),
    sidebar_block_id = sidebar,
  }
end

-- Stopping daemons ----------------------------------------------------------

local function wait_pid_gone(pid, seconds)
  if not pid then return true end
  local deadline_at = util.now() + seconds
  while util.now() < deadline_at do
    if not util.kill0(pid) then return true end
    util.sleep(0.1)
  end
  return not util.kill0(pid)
end

-- nvim heads its own process group (verified), so the group kill also takes
-- LSP and formatter children with it.
local function kill_group(pid)
  if not pid then return end
  local target = "-" .. tostring(pid)
  util.exec({ "kill", "-TERM", "--", target }, { timeout = 5 })
  util.exec({ "kill", "-CONT", "--", target }, { timeout = 5 })
  local deadline_at = util.now() + M.KILL_WAIT
  while util.now() < deadline_at do
    if not util.kill0(pid) then return end
    util.sleep(0.1)
  end
  util.exec({ "kill", "-KILL", "--", target }, { timeout = 5 })
  deadline_at = util.now() + M.KILL_WAIT
  while util.now() < deadline_at do
    if not util.kill0(pid) then return end
    util.sleep(0.1)
  end
end

local function cleanup(key, state)
  if state and state.socket then os.remove(state.socket) end
  util.remove_socket(key)
  util.remove_state(key)
end

function M.stop_daemon(key, force)
  if type(key) ~= "string" or key == "" then return nil, "no daemon key given" end
  local state = util.read_json(util.state_path(key))
  if not state then
    util.remove_socket(key)
    return { key = key, stopped = false, reason = "no state file" }
  end
  state.key = key
  local nvim_bin = util.config().sidebar.nvim_bin
  state.nvim_bin = nvim_bin
  state.socket = state.socket or util.socket_path(key)
  local health, detail = M.probe(state, M.STOP_PROBE_TIMEOUT)
  if health == "error" then return nil, daemon_error(state, detail) end
  if health == "answered" then
    local stats = M.probe_stats(state)
    if not stats then
      return nil, daemon_error(state, "could not read the daemon's state")
    end
    -- Kill the socket owner: the recorded pid may be stale (orphan race).
    state.pid = stats.pid or state.pid
    if (stats.unsaved > 0 or stats.comments > 0) and not force then
      return nil, string.format(
        "%s has %d unsaved buffer(s) and %d pending comment(s); run: rex-nvim daemons stop %s --force",
        key, stats.unsaved, stats.comments, key)
    end
    send_keys(state, "<C-\\><C-n><Cmd>qa!<CR>", M.UNSTICK_TIMEOUT)
    if wait_pid_gone(state.pid, M.STOP_QUIT_DEADLINE) then
      cleanup(key, state)
      return { key = key, stopped = true, unsaved = stats.unsaved, comments = stats.comments }
    end
  elseif health == "unresponsive" then
    if not force then return nil, unresponsive_error(state) end
  else
    cleanup(key, state)
    return { key = key, stopped = true, stale = true }
  end
  kill_group(state.pid)
  cleanup(key, state)
  return { key = key, stopped = true, killed = true }
end

return M