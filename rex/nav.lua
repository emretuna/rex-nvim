-- rex-nvim: Rex-side navigation between Rex panes and nvim splits.
--
-- init.lua binds ctrl+h/j/k/l (move) and alt+h/j/k/l (resize). When the focused
-- pane is the managed sidebar, the key is forwarded into its nvim, which runs
-- the same plugin's move_cursor_*/resize_* API; otherwise Rex moves focus or
-- resizes the split itself. The nvim side bounces back at its own edge and asks
-- for a Rex resize with `mux = true`, which tells this module to resize the
-- pane directly instead of forwarding the key into nvim again.

local util = require("rex-nvim.util")
local sidebar = require("rex-nvim.sidebar")

local M = {}

local resize_amount = 3

function M.configure(opts)
  opts = opts or {}
  resize_amount = opts.resize_amount or 3
end

local KEYS = {
  left = { ctrl = "<C-h>", alt = "<A-h>" },
  down = { ctrl = "<C-j>", alt = "<A-j>" },
  up = { ctrl = "<C-k>", alt = "<A-k>" },
  right = { ctrl = "<C-l>", alt = "<A-l>" },
}

local DIRECTIONS = { left = true, down = true, up = true, right = true }

local function check_direction(direction)
  if not DIRECTIONS[direction] then
    error("rex-nvim: unknown direction " .. tostring(direction), 0)
  end
end

-- The managed sidebar's daemon state, or nil when the focused pane is not a
-- managed sidebar (plain Rex navigation; other nvim instances have no socket
-- known here).
local function sidebar_state(block_id)
  if not block_id then return nil end
  for _, state in ipairs(util.load_all_states()) do
    if state.sidebar_block_id == block_id then
      if not state.socket or state.socket == "" then
        state.socket = util.socket_path(state.key)
      end
      if not state.nvim_bin or state.nvim_bin == "" then
        state.nvim_bin = "nvim"
      end
      return state
    end
  end
  return nil
end

function M.move(ctx, direction)
  check_direction(direction)
  local session_id = ctx and ctx.session_id
  local state = sidebar_state(ctx and ctx.block_id)
  if state then
    local r = sidebar.send_keys(state, KEYS[direction].ctrl, 2)
    if r and r.ok then return { forwarded = true, direction = direction } end
  end
  if session_id then
    rex.call("session.focus_direction", { session_id = session_id, direction = direction })
  end
  return { forwarded = false, direction = direction }
end

-- Layout traversals shared with `rex-nvim doctor`; copied here so this module
-- does not pull in the whole doctor script.

local function tiled_layer(window)
  for _, layer in ipairs(window.layers or {}) do
    if layer.kind == "tiled" then return layer end
  end
  return nil
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

-- The split node that directly holds the block, and which side it is on.
local function parent_split(layout, block_id)
  if type(layout) ~= "table" or not layout.split then return nil end
  local split = layout.split
  if type(split.before) == "table" and split.before.block_id == block_id then
    return layout, "before"
  end
  if type(split.after) == "table" and split.after.block_id == block_id then
    return layout, "after"
  end
  local before, before_side = parent_split(split.before, block_id)
  if before then return before, before_side end
  local after, after_side = parent_split(split.after, block_id)
  if after then return after, after_side end
  return nil
end

-- The leftmost leaf of a subtree: resize_pane names the two panes whose border
-- moves, and the first leaf on each side is the one that does.
local function first_leaf(node)
  if type(node) ~= "table" then return nil end
  if node.block_id then return node.block_id end
  local split = node.split
  if not split then return nil end
  return first_leaf(split.before) or first_leaf(split.after)
end

local function block_size(session_id, block_id)
  local size = util.block_method(session_id, block_id, "size")
  if type(size) ~= "table" then return nil end
  return size
end

-- Resize the split border that is the pane's immediate border. amount is in
-- cells; the API ratio applies to the whole split region, so convert through
-- the pane's share of that region. Border cells are ignored (±1 column).
local function ratio_resize(session_id, block_id, direction, amount)
  if not session_id or not block_id then return nil end
  local view = util.view(session_id)
  if not view then return nil end
  local window = util.window_for_block(view, block_id)
  local layer = window and tiled_layer(window)
  if not layer then return nil end
  local node, side = parent_split(layer.layout, block_id)
  if not node then return nil end
  local split = node.split
  local axis = (direction == "left" or direction == "right") and "horizontal" or "vertical"
  if split.direction ~= axis then return nil end
  if direction == "left" or direction == "up" then
    if side ~= "after" then return nil end
  else
    if side ~= "before" then return nil end
  end
  local target_rect = node_rect(node, block_id)
  local size = block_size(session_id, block_id)
  if not target_rect or not size then return nil end
  local extent
  if axis == "horizontal" then
    if not size.columns or not target_rect[3] or target_rect[3] == 0 then return nil end
    extent = size.columns * node.rect[3] / target_rect[3]
  else
    if not size.rows or not target_rect[4] or target_rect[4] == 0 then return nil end
    extent = size.rows * node.rect[4] / target_rect[4]
  end
  if not extent or extent <= 0 then return nil end
  local delta = amount / extent
  if direction == "left" or direction == "up" then delta = -delta end
  local ratio = split.ratio + delta
  if ratio < 0.05 then ratio = 0.05 end
  if ratio > 0.95 then ratio = 0.95 end
  local before_block_id = first_leaf(split.before)
  local after_block_id = first_leaf(split.after)
  if not before_block_id or not after_block_id then return nil end
  local result = rex.call("session.resize_pane", {
    session_id = session_id,
    direction = axis,
    before_block_id = before_block_id,
    after_block_id = after_block_id,
    ratio = ratio,
  })
  if result == nil then return nil end
  return { resized = true, direction = direction, amount = amount, ratio = ratio }
end

function M.resize(ctx, args)
  args = args or {}
  local direction = args.direction
  check_direction(direction)
  local session_id = ctx and ctx.session_id
  local block_id = args.block_id or (ctx and ctx.block_id)
  local amount = args.amount or resize_amount
  local state = sidebar_state(block_id)
  if state and args.mux ~= true then
    local r = sidebar.send_keys(state, KEYS[direction].alt, 2)
    return { forwarded = (r ~= nil and r.ok) or false, direction = direction }
  end
  if not session_id then session_id = state and state.session_id end
  return ratio_resize(session_id, block_id, direction, amount)
    or { resized = false, direction = direction, amount = amount }
end

return M