-- rex-nvim: Rex-side entry point.
--
-- init.lua in ~/.config/rex does:
--   require("rex-nvim").setup{ root = "/path/to/rex-nvim" }
-- and this module registers the actions and the default key bindings.

local util = require("rex-nvim.util")
local sidebar = require("rex-nvim.sidebar")
local agents = require("rex-nvim.agents")
local nav = require("rex-nvim.nav")

local M = {}

M.SHELL = "com.superlogical.terminal.shell"

local function log(level, message)
  if rex.log then rex.log(level, "rex-nvim: " .. tostring(message)) end
end

-- A key this file's load already bound. Our own action may be rebound safely;
-- anything else belongs to the user and is never overridden.
local function local_binding(key, action)
  local ok, bindings = pcall(rex.bindings, { key = key })
  if not ok or type(bindings) ~= "table" then
    ok, bindings = pcall(rex.bindings)
  end
  if not ok or type(bindings) ~= "table" then return nil end
  for _, binding in ipairs(bindings) do
    if binding.key == key and binding.mode == nil and binding.action ~= action then
      return binding
    end
  end
  return nil
end

local function bind_unless_taken(key, action, args)
  if key == false or key == nil or key == "" then return false end
  local existing = local_binding(key, action)
  if existing then
    log("warn", string.format("%s is already bound to %s; not overriding it",
      key, tostring(existing.action or "a function")))
    return false
  end
  rex.bind(key, action, args)
  return true
end

local function pick(ctx)
  local session_id = (ctx and ctx.session_id) or rex.session_id
  local origin = ctx and ctx.block_id
  local function say(message)
    util.notify(session_id, origin, message)
    log("warn", message)
  end
  local sup_ok, sup_err = util.ensure_supervisor()
  if not sup_ok then return nil, sup_err end
  local listed, lerr = agents.list(session_id)
  if not listed then return nil, "could not list agent panes: " .. tostring(lerr) end
  local list = listed.agents or {}
  if #list == 0 then
    say("no agent panes found in session")
    return { picked = false, reason = "no agent panes found in session" }
  end
  local view = util.view(session_id)
  local window = (origin and util.window_for_block(view, origin))
    or util.find_window(view, view and view.active_window_id)
  local window_id = window and window.window_id
  local target
  for _, agent in ipairs(list) do
    if origin and agent.block_id == origin then target = agent break end
  end
  if not target then
    for _, agent in ipairs(list) do
      if agent.window_id == window_id then target = agent break end
    end
  end
  target = target or list[1]
  local cwd = target.cwd or os.getenv("HOME") or "/"
  local bin = util.bin()
  local layer, err = rex.session.new_layer({
    window_id = window_id,
    bounds = { 0.1, 0.2, 0.8, 0.6 },
    focus = true,
    session_id = session_id,
    layout = rex.layout.block({
      flavor = M.SHELL,
      options = {
        command = {
          bin, "pick",
          "--session", session_id,
          "--window", window_id,
          "--block", target.block_id,
          "--kind", target.kind,
          "--origin", origin or target.block_id,
          "--cwd", cwd,
        },
        shell = "none",
        cwd = cwd,
      },
    }),
  })
  if not layer then return nil, "could not open the picker: " .. tostring(err) end
  local block_id = layer.block_ids and layer.block_ids[1]
  if block_id then
    rex.session.set_block_label({
      block_id = block_id,
      label = "open file",
      session_id = session_id,
    })
  end
  return { picked = true, block_id = block_id, layer_id = layer.layer_id }
end

M.pick = pick

local function command_split(ctx, args)
  local session_id = (ctx and ctx.session_id) or rex.session_id
  -- direction/side/ratio are validated by new_split's input checks.
  local direction = args.direction or "horizontal"
  local side = args.side or "after"
  local ratio = args.ratio or 0.5
  local cwd = sidebar.cwd_for(session_id, ctx and ctx.block_id)
  local nvim_bin = util.config().sidebar.nvim_bin
  -- Launch via the flavor's default login shell (like rex split and the
  -- original neogit_split): the rc PATH resolves a bare nvim_bin and stays
  -- intact for the toolchains nvim spawns. A shell="none" pane would report
  -- its login wrapper as the foreground with no cwd, breaking chained presses.
  local command = { nvim_bin, "-c", args.command }
  local r, err = rex.session.new_split({
    anchor_id = ctx and ctx.block_id,
    direction = direction,
    side = side,
    ratio = ratio,
    focus = true,
    session_id = session_id,
    layout = rex.layout.block({
      flavor = M.SHELL,
      options = {
        command = command,
        cwd = cwd,
      },
    }),
  })
  if not r then error("rex-nvim: could not open the split: " .. tostring(err), 0) end
  return { block_id = r.block_ids and r.block_ids[1], view = "split" }
end

-- Toggle records for command splits: one file per window mapping each
-- command to the split it last opened. With toggle=true the next press in
-- that window closes the split instead of stacking another one; a record
-- whose block is gone (closed by hand) falls through to a fresh open.
local function command_records_path(session_id, window_id)
  return util.state_path("cmd_" .. util.key(session_id, window_id))
end

local function command_split_toggled(ctx, args)
  local session_id = (ctx and ctx.session_id) or rex.session_id
  local block_id = ctx and ctx.block_id
  local view, verr = util.view(session_id)
  if not view then error("rex-nvim: could not read the session view: " .. tostring(verr), 0) end
  local window = (block_id and util.window_for_block(view, block_id))
    or util.find_window(view, view.active_window_id)
  if not window then error("rex-nvim: the session has no windows", 0) end
  local path = command_records_path(session_id, window.window_id)
  local records = util.read_json(path) or {}
  local rec = records[args.command]
  if rec and rec.block_id and util.block_in_window(window, rec.block_id) then
    local r, cerr = rex.call("block.close", { session_id = session_id, block_id = rec.block_id })
    if not r then error("rex-nvim: could not close the split: " .. tostring(cerr), 0) end
    records[args.command] = nil
    util.write_json_atomic(path, records)
    local back = rec.origin_block_id
    if back and back ~= rec.block_id and util.block_in_window(window, back) then
      rex.session.focus_block({ block_id = back, session_id = session_id })
    end
    return { view = "split", closed = true, block_id = rec.block_id }
  end
  local result = command_split(ctx, args)
  records[args.command] = { block_id = result.block_id, origin_block_id = block_id }
  util.write_json_atomic(path, records)
  return result
end

function M.setup(opts)
  opts = opts or {}
  local root = opts.root or util.root()
  if type(root) ~= "string" or root == "" then
    error("rex-nvim.setup: root is required (the path to the rex-nvim checkout)", 0)
  end
  util.set_root(root)
  if opts.bin then util.set_bin(opts.bin) end
  nav.configure({ resize_amount = opts.resize_amount })

  local keys = opts.keys or {}
  local toggle_key = keys.toggle
  if toggle_key == nil then toggle_key = "cmd+e" end
  local pick_key = keys.pick
  if pick_key == nil then pick_key = "cmd+o" end

  rex.action({
    name = "rex_nvim_toggle",
    title = "rex-nvim: Toggle Sidebar",
    description = "Show or hide the persistent nvim sidebar for this window.",
    category = "rex-nvim",
    run = function(ctx)
      local result, err = sidebar.toggle(ctx)
      if not result then error(err or "rex-nvim: could not toggle the sidebar", 0) end
      return result
    end,
  })

  rex.action({
    name = "rex_nvim_pick",
    title = "rex-nvim: Open File",
    description = "Pick a file from the session's agent panes and open it in the sidebar.",
    category = "rex-nvim",
    run = function(ctx)
      local result, err = pick(ctx)
      if not result then error(err or "rex-nvim: could not open the picker", 0) end
      return result
    end,
  })

  rex.action({
    name = "rex_nvim_open",
    title = "rex-nvim: Open File in Sidebar",
    description = "Open a file at an optional line and column in the window's sidebar.",
    category = "rex-nvim",
    args = {
      path = "string",
      line = "integer?",
      col = "integer?",
      window_id = "string?",
      origin_block_id = "string?",
    },
    run = function(ctx, args)
      local result, err = sidebar.open_file(args, ctx and ctx.session_id)
      if not result then error(err or "rex-nvim: could not open the file", 0) end
      return result
    end,
  })

  rex.action({
    name = "rex_nvim_stop",
    title = "rex-nvim: Stop Sidebar Daemon",
    description = "Stop a sidebar nvim daemon by key; force discards unsaved work.",
    category = "rex-nvim",
    args = { key = "string", force = "boolean?" },
    run = function(ctx, args)
      local result, err = sidebar.stop_daemon(args.key, args.force == true)
      if not result then error(err or "rex-nvim: could not stop the daemon", 0) end
      return result
    end,
  })

  rex.action({
    name = "rex_nvim_agents",
    title = "rex-nvim: List Agent Panes",
    description = "List the agent panes in this session as JSON.",
    category = "rex-nvim",
    run = function(ctx)
      return agents.list(ctx and ctx.session_id)
    end,
  })

  rex.action({
    name = "rex_nvim_command",
    title = "rex-nvim: Run nvim Command",
    description = "Run a nvim command line in a new split or the window's sidebar. toggle=true closes the split this command last opened in the window instead of opening another.",
    category = "rex-nvim",
    args = {
      type = "object",
      properties = {
        command = { type = "string" },
        view = { type = "string", enum = { "split", "sidebar" } },
        direction = { type = "string", enum = { "horizontal", "vertical" } },
        side = { type = "string", enum = { "before", "after" } },
        ratio = { type = "number", exclusiveMinimum = 0, exclusiveMaximum = 1 },
        toggle = { type = "boolean" },
      },
      required = { "command" },
      additionalProperties = false,
    },
    run = function(ctx, args)
      -- The running server does not enforce the args schema's required/enum
      -- keywords; guard them here so bad args fail before any side effect.
      if type(args.command) ~= "string" or args.command == "" then
        error("rex-nvim: command is required (a nvim command line without the leading colon)", 0)
      end
      local view = args.view or "split"
      if view ~= "split" and view ~= "sidebar" then
        error('rex-nvim: view must be "split" or "sidebar"', 0)
      end
      if view == "sidebar" then
        local result, err = sidebar.run_command(ctx, args.command)
        if not result then error(err or "rex-nvim: could not run the command", 0) end
        return result
      end
      if args.toggle then
        return command_split_toggled(ctx, args)
      end
      return command_split(ctx, args)
    end,
  })

  rex.action({
    name = "rex_nvim_nav",
    title = "rex-nvim: Move Focus",
    description = "Move focus between nvim splits and Rex panes.",
    category = "rex-nvim",
    args = { direction = "string" },
    run = function(ctx, args)
      local result = nav.move(ctx, args.direction)
      if not result then error("rex-nvim: could not move focus", 0) end
      return result
    end,
  })

  rex.action({
    name = "rex_nvim_resize",
    title = "rex-nvim: Resize Pane",
    description = "Resize the split on the given side by an amount of cells.",
    category = "rex-nvim",
    args = {
      direction = "string",
      amount = "integer?",
      block_id = "string?",
      mux = "boolean?",
    },
    run = function(ctx, args)
      local result = nav.resize(ctx, args)
      if not result then error("rex-nvim: could not resize the pane", 0) end
      return result
    end,
  })

  local function key_or(name, default)
    local key = keys[name]
    if key == nil then return default end
    return key
  end

  bind_unless_taken(toggle_key, "rex_nvim_toggle")
  bind_unless_taken(pick_key, "rex_nvim_pick")
  bind_unless_taken(key_or("nav_left", "ctrl+h"), "rex_nvim_nav", { direction = "left" })
  bind_unless_taken(key_or("nav_down", "ctrl+j"), "rex_nvim_nav", { direction = "down" })
  bind_unless_taken(key_or("nav_up", "ctrl+k"), "rex_nvim_nav", { direction = "up" })
  bind_unless_taken(key_or("nav_right", "ctrl+l"), "rex_nvim_nav", { direction = "right" })
  bind_unless_taken(key_or("resize_left", "alt+h"), "rex_nvim_resize", { direction = "left" })
  bind_unless_taken(key_or("resize_down", "alt+j"), "rex_nvim_resize", { direction = "down" })
  bind_unless_taken(key_or("resize_up", "alt+k"), "rex_nvim_resize", { direction = "up" })
  bind_unless_taken(key_or("resize_right", "alt+l"), "rex_nvim_resize", { direction = "right" })
  return M
end

return M