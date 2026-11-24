local M = {}

-- smart-splits-style navigation. Inside nvim this is plain window movement;
-- at an nvim edge the move crosses into the adjacent Rex pane. When nvim runs
-- as a Rex sidebar, the state file the daemon wrote tells us which session --
-- and which block -- we belong to. Standalone nvim has no state: edge moves
-- and resizes are silent no-ops, like smart-splits without a mux.
local DIRECTIONS = {
  left = { win = "h" },
  down = { win = "j" },
  up = { win = "k" },
  right = { win = "l" },
}

local function rex_bin()
  return vim.env.REX_BIN or "rex"
end

-- Same lookup order as the Rex side (rex/util.lua): explicit override, then
-- $XDG_STATE_HOME/rex-nvim, then ~/.local/state/rex-nvim.
local function state_dir()
  local env = vim.env.REX_NVIM_STATE_DIR
  if env and env ~= "" then return env end
  local xdg = vim.env.XDG_STATE_HOME
  if xdg and xdg ~= "" then return xdg .. "/rex-nvim" end
  return (vim.env.HOME or "") .. "/.local/state/rex-nvim"
end

local function state()
  local key = vim.env.REX_NVIM_KEY
  if not key or key == "" then return nil end
  local ok, lines = pcall(vim.fn.readfile, state_dir() .. "/" .. key .. ".json")
  if not ok or type(lines) ~= "table" then return nil end
  local decoded_ok, decoded = pcall(vim.json.decode, table.concat(lines, "\n"))
  if not decoded_ok or type(decoded) ~= "table" then return nil end
  return decoded
end

local function move(dir, exec)
  local d = DIRECTIONS[dir]
  if not d then return end
  exec = exec or require("rex-nvim.exec").default_exec
  local before = vim.api.nvim_get_current_win()
  vim.cmd("wincmd " .. d.win)
  if vim.api.nvim_get_current_win() ~= before then return end
  local st = state()
  if not st then return end
  pcall(exec, { rex_bin(), "focus", "-s", st.session_id, dir })
end

local function resize(dir, amount, exec)
  local d = DIRECTIONS[dir]
  if not d then return end
  exec = exec or require("rex-nvim.exec").default_exec
  -- Lazy: init.lua requires this module at load, so a top-level require would
  -- be a require cycle. Keymap callbacks pass no amount and rely on the count.
  local n = amount
    or ((vim.v.count > 0 and vim.v.count or 1) * require("rex-nvim").config.resize.amount)

  -- winnr(dir) is the current window's number when there is no window in that
  -- direction, so this stays put: splitting is only touched inside nvim.
  if vim.fn.winnr(d.win) ~= vim.fn.winnr() then
    if dir == "left" then
      vim.cmd("vertical resize -" .. n)
    elseif dir == "right" then
      vim.cmd("vertical resize +" .. n)
    elseif dir == "up" then
      vim.cmd("resize -" .. n)
    else
      vim.cmd("resize +" .. n)
    end
    return
  end

  local st = state()
  if not st or not st.sidebar_block_id then return end
  -- mux=true is load-bearing: it tells the Rex action to resize the pane
  -- directly instead of forwarding the key back into this nvim.
  pcall(exec, {
    rex_bin(), "do", "-s", st.session_id, "rex_nvim_resize", "--args",
    vim.json.encode({ direction = dir, amount = n, mux = true, block_id = st.sidebar_block_id }),
  })
end

M.move_cursor_left = function(exec) return move("left", exec) end
M.move_cursor_down = function(exec) return move("down", exec) end
M.move_cursor_up = function(exec) return move("up", exec) end
M.move_cursor_right = function(exec) return move("right", exec) end

M.resize_left = function(amount, exec) return resize("left", amount, exec) end
M.resize_down = function(amount, exec) return resize("down", amount, exec) end
M.resize_up = function(amount, exec) return resize("up", amount, exec) end
M.resize_right = function(amount, exec) return resize("right", amount, exec) end

return M