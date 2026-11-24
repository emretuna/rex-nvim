local M = {}

-- Delivery to an agent pane goes through the rex CLI: a paste event lands the
-- text at the agent's cursor, and for a submit a single Enter follows. Rex has
-- no prompt command; pasting and pressing Enter is exactly what the TUI does.
local function rex_bin()
  return vim.env.REX_BIN or "rex"
end

local function session_id()
  return vim.env.REX_SESSION or ""
end

local function failure(name, r)
  return name .. " failed: " .. (r.stderr ~= "" and r.stderr or ("exit " .. r.code))
end

function M.send(block_id, text, opts, exec)
  opts = opts or {}
  exec = exec or require("rex-nvim.exec").default_exec
  local rex = rex_bin()
  local sess = session_id()
  local payload = vim.json.encode({
    block_id = block_id,
    event = { type = "paste", payload = { text = text } },
  })
  local r = exec({ rex, "api", "call", "-s", sess, "block.event", payload })
  if r.code ~= 0 then
    return false, failure("rex paste", r)
  end
  if opts.submit then
    local pressed = exec({ rex, "send-key", "-s", sess, "-b", block_id, "enter" })
    if pressed.code ~= 0 then
      return false, failure("rex send-key", pressed)
    end
  end
  return true
end

return M
