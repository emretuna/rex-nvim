-- rex-nvim: self-check for the Rex half.
--
-- Run as: rex do <root>/rex/doctor.lua [with_agent=NAME]
-- (the `rex-nvim doctor` wrapper passes --with-agent NAME the same way).
--
-- The checks run in a throwaway session and a throwaway state directory, so a
-- broken install never leaves a daemon, supervisor or state file behind. The
-- only real-session work is --with-agent, which is read-only.

-- Bootstrap module resolution from this file's own path, exactly like
-- rex/supervisor.lua: the symlink in ~/.config/rex may not exist yet.
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
  return root
end

local root = bootstrap()

local util = require("rex-nvim.util")
local sidebar = require("rex-nvim.sidebar")
local agents = require("rex-nvim.agents")

if root and root ~= "" then util.set_root(root) end

local SHELL = "com.superlogical.terminal.shell"
local SCRATCH_LABEL = "rex-nvim-doctor"
local NVIM_MIN_MAJOR = 0
local NVIM_MIN_MINOR = 10

-- Doctor must not start the lifecycle supervisor: it would keep watching the
-- temporary state directory after this process exits. The checks exercise the
-- sidebar directly, and no daemon here outlives cleanup.
util.ensure_supervisor = function() return true end

local checks = { ok = 0, fail = 0 }

local function check(name, ok, detail)
  if ok then
    checks.ok = checks.ok + 1
    print("OK   " .. name .. (detail and (" (" .. detail .. ")") or ""))
  else
    checks.fail = checks.fail + 1
    print("FAIL " .. name .. (detail and (": " .. detail) or ""))
  end
  return ok
end

local function warn(message)
  print("WARN " .. tostring(message))
end

local function reason(r)
  if r == nil then return "command could not run" end
  local text = util.trim((r.stderr or "") .. " " .. (r.stdout or ""))
  if text == "" then text = "exit status " .. tostring(r.code) end
  return text
end

-- Best effort: the installed binary's inventory is useful context even when
-- the rest of the checks fail. It reads the real state dir, before isolation.
local function print_daemons(bin)
  local r = util.exec({ bin, "daemons" }, { timeout = 60 })
  if r and r.ok then
    local text = util.trim(r.stdout)
    if text ~= "" then print(text) end
  else
    warn("could not list daemons with " .. bin .. ": " .. reason(r))
  end
end

-- nvim ----------------------------------------------------------------------

local function nvim_version(nvim_bin)
  local r = util.exec({ nvim_bin, "--version" }, { timeout = 10 })
  if r == nil or not r.ok then return nil, reason(r) end
  local first = (r.stdout or ""):match("^[^\r\n]*")
  local major, minor = (first or ""):match("NVIM v(%d+)%.(%d+)")
  if not major then return nil, "unexpected --version output: " .. tostring(first) end
  return { major = tonumber(major), minor = tonumber(minor), text = first }
end

-- Sessions and views --------------------------------------------------------

local function ensure_dir(path)
  if path and path ~= "" then util.ensure_dir(path) end
end

local function temp_dir()
  local r = util.exec({ "mktemp", "-d", "/tmp/rex-nvim-doctor.XXXXXX" }, { timeout = 10 })
  if not r or not r.ok then return nil, reason(r) end
  local path = util.trim(r.stdout)
  if path == "" then return nil, "mktemp returned no path" end
  return path
end

local function remove_dir(path)
  if not path or path == "" then return end
  util.exec({ "rm", "-rf", path }, { timeout = 10 })
end

-- session.list_labels is a local view because the scratch session is created
-- after the label sweep.
local function sessions()
  local listed = rex.call("session.list")
  return (listed and listed.sessions) or {}
end

local function attach(session_id)
  pcall(rex.session.attach, { session_id = session_id })
end

-- A doctor killed with Ctrl-C can leave its scratch session behind. Remove any
-- older copy by label before creating this run's.
local function sweep_stale_scratch()
  for _, session in ipairs(sessions()) do
    if session.label == SCRATCH_LABEL then
      attach(session.session_id)
      pcall(rex.call, "session.destroy", { session_id = session.session_id })
    end
  end
end

local function create_scratch()
  local created, err = rex.call("session.create", {
    label = SCRATCH_LABEL,
    initial_windows = {
      {
        window_label = "doctor",
        layout = {
          split = {
            direction = "horizontal",
            ratio = 0.5,
            before = {
              block = {
                flavor = SHELL,
                label = "doctor a",
                options = {
                  command = { "sh", "-c", "sleep 600" },
                  shell = "none",
                },
              },
            },
            after = {
              block = {
                flavor = SHELL,
                label = "doctor b",
                options = {
                  command = { "sh", "-c", "sleep 600" },
                  shell = "none",
                },
              },
            },
          },
        },
      },
    },
  })
  if not created then return nil, tostring(err) end
  local info = created.initial_windows and created.initial_windows[1]
  local block_ids = (info and info.block_ids) or {}
  if not created.session_id or not info or not info.window_id or #block_ids < 2 then
    return nil, "session.create returned an unexpected shape"
  end
  return {
    session_id = created.session_id,
    window_id = info.window_id,
    block_a = block_ids[1],
    block_b = block_ids[2],
  }
end

local function tiled_layer(window)
  for _, layer in ipairs(window.layers or {}) do
    if layer.kind == "tiled" then return layer end
  end
  return nil
end

local function layer_blocks(layer)
  return (layer and layer.blocks) or {}
end

-- Walk the split tree for the node that holds this block.
local function node_rect(layout, block_id)
  if type(layout) ~= "table" then return nil end
  if layout.block_id == block_id then return layout.rect end
  local split = layout.split
  if split then
    return node_rect(split.before, block_id) or node_rect(split.after, block_id)
  end
  return nil
end

-- The split node that directly holds the sidebar, and which side it is on.
-- The split ratio is the fraction belonging to `before`; measure the sidebar's
-- share of the space it was attached to (0.5 for the default), not its share of
-- the whole window (an anchor pane may already be half the window).
local function sidebar_split(layout, block_id)
  if type(layout) ~= "table" or not layout.split then return nil end
  local split = layout.split
  if type(split.before) == "table" and split.before.block_id == block_id then
    return layout, "before"
  end
  if type(split.after) == "table" and split.after.block_id == block_id then
    return layout, "after"
  end
  local before, before_side = sidebar_split(split.before, block_id)
  if before then return before, before_side end
  local after, after_side = sidebar_split(split.after, block_id)
  if after then return after, after_side end
  return nil
end

local function sidebar_ratio(view, window_id, block_id)
  local window = util.find_window(view, window_id)
  local layer = window and tiled_layer(window)
  if not layer then return nil end
  local node, side = sidebar_split(layer.layout, block_id)
  if not node or type(node.split.ratio) ~= "number" then return nil end
  if side == "after" then return 1 - node.split.ratio, node.split.ratio end
  return node.split.ratio, node.split.ratio
end

-- The sidebar must span the full extent perpendicular to its split: for a
-- side-by-side split that is the height (1.0 for a full-height pane); for a
-- stacked one it is the width. Compare with the split's own rect so the check
-- holds for every [sidebar] position, not just "right".
local function sidebar_extent(view, window_id, block_id)
  local window = util.find_window(view, window_id)
  local layer = window and tiled_layer(window)
  if not layer then return nil end
  local node = sidebar_split(layer.layout, block_id)
  local rect = node_rect(layer.layout, block_id)
  if not node or not node.rect or not rect then return nil end
  local horizontal = node.split.direction == "horizontal"
  local actual = horizontal and rect[4] or rect[3]
  local expected = horizontal and node.rect[4] or node.rect[3]
  return actual, expected, node.split.direction
end

local function tiled_count(view, window_id)
  local window = util.find_window(view, window_id)
  return window and #layer_blocks(tiled_layer(window)) or 0
end

local function block_screen(session_id, block_id)
  local modes = util.block_method(session_id, block_id, "modes")
  return modes and modes.screen
end

local function capture(session_id, block_id)
  local r = util.exec({
    util.rex_bin(), "capture", "-s", session_id, "-b", block_id,
    "--trim", "--unwrap",
  }, { timeout = 30 })
  if r and r.ok then return util.trim(r.stdout) end
  return nil, reason(r)
end

local function wait_for(seconds, fn)
  local deadline = os.time() + seconds
  local value, detail
  repeat
    value, detail = fn()
    if value then return value, detail end
    util.sleep(0.2)
  until os.time() >= deadline
  return nil, detail
end

local function approx(a, b, tolerance)
  if type(a) ~= "number" then return false end
  return math.abs(a - b) <= (tolerance or 0.02)
end

-- --with-agent ---------------------------------------------------------------

local function check_agent(expected)
  local scope = rex.session_id or os.getenv("REX_SESSION")
  local targets = {}
  if scope and scope ~= "" then
    targets[1] = scope
  else
    for _, session in ipairs(sessions()) do
      if session.label ~= SCRATCH_LABEL then targets[#targets + 1] = session.session_id end
    end
  end
  local seen = {}
  local errors = {}
  for _, session_id in ipairs(targets) do
    if not scope then attach(session_id) end
    local listed, err = agents.list(session_id)
    if listed then
      for _, agent in ipairs(listed.agents or {}) do
        seen[agent.kind] = true
      end
    else
      errors[#errors + 1] = tostring(err)
    end
  end
  local detail
  if seen[expected] then
    detail = "kind " .. expected .. " found in " .. tostring(#targets) .. " session(s)"
  elseif #errors > 0 then
    detail = table.concat(errors, "; ")
  else
    detail = "no " .. expected .. " pane in " .. tostring(#targets) .. " session(s)"
  end
  return check("agent " .. expected, seen[expected] == true, detail)
end

-- Scratch-session checks -----------------------------------------------------

local cleanup = {
  key = nil,
  scratch = nil,
  temp = nil,
  temp_file = nil,
}

local function stop_scratch()
  if cleanup.key then
    local ok, err = pcall(sidebar.stop_daemon, cleanup.key, true)
    if not ok then warn("stop_daemon failed: " .. tostring(err)) end
  end
  if cleanup.scratch then
    attach(cleanup.scratch.session_id)
    pcall(rex.call, "session.destroy", { session_id = cleanup.scratch.session_id })
  end
  if cleanup.temp_file then os.remove(cleanup.temp_file) end
  remove_dir(cleanup.temp)
  util.set_state_dir(nil)
end

local function run_scratch()
  local temp, terr = temp_dir()
  if not temp then
    check("isolated state dir", false, tostring(terr))
    return
  end
  ensure_dir(temp .. "/run")
  ensure_dir(temp .. "/tmp")
  cleanup.temp = temp
  util.set_state_dir(temp)

  -- util.install_info() now reads the scratch dir; seed it so util.bin(),
  -- util.root() and util.rex_bin() keep pointing at the real install while
  -- state files and sockets stay isolated.
  local info = {
    root = util.root(),
    bin = util.bin(),
    rex = util.rex_bin(),
  }
  local wrote, werr = util.write_json_atomic(temp .. "/install.json", info)
  if not wrote then
    check("isolated state dir", false, tostring(werr))
    return
  end
  check("isolated state dir", true, temp)

  sweep_stale_scratch()
  local scratch, serr = create_scratch()
  if not scratch then
    check("scratch session", false, tostring(serr))
    return
  end
  cleanup.scratch = scratch
  attach(scratch.session_id)
  check("scratch session", true, scratch.session_id)

  -- Toggle ON ---------------------------------------------------------------
  local result, terr2 = sidebar.toggle({ session_id = scratch.session_id, block_id = scratch.block_a })
  if not result then
    check("toggle on", false, tostring(terr2))
    return
  end
  cleanup.key = result.key
  local sidebar_block = result.block_id
  check("toggle on", result.visible == true and type(sidebar_block) == "string",
    "key " .. tostring(result.key))

  local state = util.read_json(util.state_path(result.key))
  local health, detail = sidebar.probe(state, 5)
  check("daemon healthy", health == "answered",
    health == "answered" and ("pid " .. tostring(detail)) or tostring(detail))

  -- Full-height pane and remote-ui attach ----------------------------------
  local view = util.view(scratch.session_id)
  local actual, expected, direction = sidebar_extent(view, scratch.window_id, sidebar_block)
  check("sidebar full-height", actual ~= nil and approx(actual, expected, 0.01),
    actual and string.format("%s extent %.3f of %.3f", tostring(direction), actual, expected)
      or "sidebar not found in the tiled layout")

  local screen = wait_for(10, function()
    local state_name = block_screen(scratch.session_id, sidebar_block)
    if state_name == "alternate" then return true end
    return nil, state_name
  end)
  check("remote-ui attach", screen == true, screen and "screen=alternate" or ("screen=" .. tostring(screen)))

  local text = wait_for(10, function()
    local captured = capture(scratch.session_id, sidebar_block)
    if captured and captured ~= "" then return captured end
    return nil, captured
  end)
  check("sidebar renders nvim", text ~= nil and text ~= "",
    text and (tostring(#text) .. " bytes captured") or "empty capture")

  -- Open a file at line 2 ---------------------------------------------------
  local probe_path = temp .. "/rex-nvim-doctor-open.txt"
  local f = io.open(probe_path, "w")
  if f then
    f:write("doctor line one\nREX_NVIM_DOCTOR_LINE_TWO\ndoctor line three\n")
    f:close()
    cleanup.temp_file = probe_path
  end
  local opened, oerr = sidebar.open_file({
    path = probe_path,
    line = 2,
    window_id = scratch.window_id,
    origin_block_id = scratch.block_a,
  }, scratch.session_id)
  local found = opened and wait_for(10, function()
    local captured = capture(scratch.session_id, sidebar_block)
    if captured and captured:find("REX_NVIM_DOCTOR_LINE_TWO", 1, true) then return true end
    return nil, captured
  end)
  check("open file at line 2", found == true,
    found == true and probe_path or tostring(oerr or "capture never showed line 2"))

  -- Toggle OFF --------------------------------------------------------------
  local off, off_err = sidebar.toggle({ session_id = scratch.session_id, block_id = scratch.block_a })
  if not off then
    check("toggle off", false, tostring(off_err))
    return
  end
  local placement = util.block_placement(scratch.session_id, sidebar_block)
  view = util.view(scratch.session_id)
  local count = tiled_count(view, scratch.window_id)
  check("toggle off detaches the pane",
    off.visible == false and placement and placement.placement == "detached" and count == 2,
    string.format("placement=%s tiled_blocks=%d",
      tostring(placement and placement.placement), count))

  -- Toggle ON again: same block, native reattach -----------------------------
  local on, on_err = sidebar.toggle({ session_id = scratch.session_id, block_id = scratch.block_a })
  if not on then
    check("toggle on again", false, tostring(on_err))
    return
  end
  view = util.view(scratch.session_id)
  local ratio, split_ratio = sidebar_ratio(view, scratch.window_id, on.block_id)
  check("toggle on reattaches",
    on.visible == true and on.block_id == sidebar_block and approx(ratio, 0.5, 0.02),
    string.format("block=%s sidebar_share=%s split_ratio=%s",
      tostring(on.block_id), tostring(ratio), tostring(split_ratio)))
end

-- Main -----------------------------------------------------------------------

local with_agent = rex.args and rex.args.with_agent
if type(with_agent) == "number" then with_agent = tostring(with_agent) end

local info = util.install_info() or {}
if info.root and info.root ~= "" then util.set_root(info.root) end
local bin = (info.bin and info.bin ~= "") and info.bin or "rex-nvim"

print("rex-nvim doctor")
print("root: " .. tostring(util.root() or "unknown"))
print("bin:  " .. tostring(bin))
print("")
print("--- rex-nvim daemons (best effort) ---")
print_daemons(bin)
print("-------------------------------------")
print("")

local cfg
local ok, cfg_err = pcall(util.config)
if ok then cfg = cfg_err else warn("could not read config: " .. tostring(cfg_err)) end
cfg = cfg or util.DEFAULT_CONFIG
local nvim_bin = cfg.sidebar.nvim_bin or "nvim"

local version, version_err = nvim_version(nvim_bin)
local nvim_ok = version ~= nil
  and (version.major > NVIM_MIN_MAJOR
    or (version.major == NVIM_MIN_MAJOR and version.minor >= NVIM_MIN_MINOR))
check("nvim >= 0.10",
  nvim_ok,
  version and version.text or ("not found: " .. tostring(nvim_bin) .. " (" .. tostring(version_err) .. ")"))

if with_agent and with_agent ~= "" then check_agent(tostring(with_agent)) end

if nvim_ok then
  local ok_run, run_err = pcall(run_scratch)
  if not ok_run then
    check("doctor checks", false, tostring(run_err))
  end
else
  warn("nvim checks skipped: nvim is missing or too old")
end

local ok_clean, clean_err = pcall(stop_scratch)
if not ok_clean then warn("cleanup failed: " .. tostring(clean_err)) end

print("")
print(string.format("%d ok, %d failed", checks.ok, checks.fail))
if checks.fail > 0 then
  rex.stop(1)
else
  rex.stop(0)
end