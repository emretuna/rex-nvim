local M = {}
local exec_mod = require("rex-nvim.exec")

-- The agent inventory is a rex action (rex_nvim_agents), which derives it from
-- the session's terminal blocks: there is no separate agent registry.
local function list_argv()
  return { vim.env.REX_BIN or "rex", "do", "rex_nvim_agents", "--json" }, "rex do rex_nvim_agents"
end

function M.list(exec)
  exec = exec or exec_mod.default_exec
  local argv, name = list_argv()
  local r = exec(argv)
  if r.code ~= 0 then
    return nil, name .. " failed: " .. (r.stderr ~= "" and r.stderr or ("exit " .. r.code))
  end
  local ok, decoded = pcall(vim.json.decode, r.stdout)
  if not ok or type(decoded) ~= "table" then return nil, name .. ": unparseable JSON" end
  local raw = decoded.agents or {}
  local out = {}
  local here = vim.env.REX_SESSION
  for _, a in ipairs(raw) do
    if not here or a.session_id == here then
      table.insert(out, {
        block_id = a.block_id,
        session_id = a.session_id,
        window_id = a.window_id,
        kind = a.kind or "unknown",
        status = a.status or "unknown",
        cwd = a.cwd or "",
        title = a.title or a.kind or "agent",
      })
    end
  end
  table.sort(out, function(x, y) return x.title < y.title end)
  return out
end

-- Resolve the one agent to target without a picker, or nil when it's ambiguous.
-- `list` is already session-scoped by M.list. Narrowest unambiguous match wins:
--   1. a single agent sharing the current window (REX_WINDOW_ID) — the sibling
--      block, the same convention the file picker uses to find "the agent in
--      this window";
--   2. otherwise, a lone agent in the session.
-- Anything ambiguous (2+ candidates) returns nil so the caller shows the picker.
function M.resolve(list)
  if #list == 1 then return list[1] end
  local win = vim.env.REX_WINDOW_ID
  if win then
    local in_window = {}
    for _, a in ipairs(list) do
      if a.window_id == win then table.insert(in_window, a) end
    end
    if #in_window == 1 then return in_window[1] end
  end
  return nil
end

function M.display(agent)
  local tail = vim.fn.fnamemodify(agent.cwd, ":t")
  -- Lead with the agent kind (pi/claude/codex…) — the actual agent identity —
  -- then its state and where it's running. (The block title tended to just
  -- repeat the session/repo name shown by the cwd tail.)
  return string.format("%s · %s · %s", agent.kind, agent.status, tail)
end

return M
