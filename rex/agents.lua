-- rex-nvim: agent pane discovery.
--
-- Rex has no agent registry: an agent is a terminal whose foreground program
-- (or program-status record) is a known agent. Read-only; never sends input.

local util = require("rex-nvim.util")

local M = {}

local TERMINAL = "com.superlogical.terminal"

M.KINDS = {
  pi = true,
  omp = true,
  claude = true,
  codex = true,
  agy = true,
  antigravity = true,
  antigravity_cli = true,
  gemini = true,
}

local function record_status(records)
  for _, record in ipairs(records or {}) do
    -- The root record has no id; that is the terminal's own program state.
    if record.id == nil and record.state then return record.state end
  end
  return nil
end

local function record_kind(records)
  for _, record in ipairs(records or {}) do
    if record.app and M.KINDS[record.app] then return record.app end
  end
  return nil
end

local function sidebar_cwd(session_id, block_id)
  local pwd = util.block_method(session_id, block_id, "pwd")
  local path = pwd and pwd.pwd and pwd.pwd:match("^file://[^/]*(/.+)$")
  if path and path ~= "" then return path end
  return nil
end

function M.list(session_id)
  session_id = session_id or rex.session_id
  local args = {}
  if session_id then args.session_id = session_id end
  local result, err = rex.session.list_blocks(args)
  if result == nil then return nil, err end
  local agents = {}
  for _, block in ipairs(result.blocks or {}) do
    if block.creator_name == TERMINAL then
      local proc = util.block_method(session_id, block.block_id, "process") or {}
      local statuses = util.block_method(session_id, block.block_id, "program_status") or {}
      local records = statuses.records or {}
      local foreground = proc.foreground
      local kind = foreground and foreground.name
      if not (kind and M.KINDS[kind]) then kind = record_kind(records) end
      if kind and M.KINDS[kind] then
        local cwd = (foreground and foreground.cwd)
          or sidebar_cwd(session_id, block.block_id)
          or (proc.child and proc.child.cwd)
          or os.getenv("HOME")
          or "/"
        agents[#agents + 1] = {
          block_id = block.block_id,
          session_id = session_id,
          window_id = block.window_id,
          kind = kind,
          status = record_status(records) or "unknown",
          cwd = cwd,
          title = block.label or kind,
        }
      end
    end
  end
  table.sort(agents, function(a, b)
    local at, bt = tostring(a.title or ""), tostring(b.title or "")
    if at == bt then return tostring(a.block_id) < tostring(b.block_id) end
    return at < bt
  end)
  return { agents = agents }
end

return M