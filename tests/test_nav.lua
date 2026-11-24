local nav = require("rex-nvim.nav")
local rn = require("rex-nvim")

local function recorder()
  local calls = {}
  return calls, function(argv)
    table.insert(calls, argv)
    return { code = 0, stdout = "", stderr = "" }
  end
end

local function with_env(name, value, fn)
  local previous = vim.env[name]
  vim.env[name] = value
  local ok, err = pcall(fn)
  vim.env[name] = previous
  if not ok then error(err, 0) end
end

-- Every test starts and ends on a single window, so a failing assertion cannot
-- leak splits into the rest of the suite.
local function with_layout(fn)
  vim.cmd("only")
  local ok, err = pcall(fn)
  pcall(vim.cmd, "only")
  if not ok then error(err, 0) end
end

local function with_config(fn)
  local saved = vim.deepcopy(rn.config)
  local ok, err = pcall(fn)
  rn.config = saved
  if not ok then error(err, 0) end
end

-- Sidebar state file the daemon would have written: <dir>/<key>.json.
local STATE_KEY = "nav-test-key"
local function with_state(fixture, fn)
  local dir = vim.fn.tempname()
  vim.fn.mkdir(dir, "p")
  local fh = assert(io.open(dir .. "/" .. STATE_KEY .. ".json", "w"))
  fh:write(vim.json.encode(fixture))
  fh:close()
  local ok, err = pcall(function()
    with_env("REX_NVIM_KEY", STATE_KEY, function()
      with_env("REX_NVIM_STATE_DIR", dir, fn)
    end)
  end)
  vim.fn.delete(dir, "rf")
  if not ok then error(err, 0) end
end

local STATE = { session_id = "session:s1", sidebar_block_id = "block:9" }

local function win_in(direction)
  return vim.fn.win_getid(vim.fn.winnr(direction))
end

T.test("nav: move_cursor_left moves between nvim splits without calling rex", function()
  with_layout(function()
    with_env("REX_BIN", nil, function()
      vim.cmd("vsplit")
      local left = win_in("h")
      vim.api.nvim_set_current_win(win_in("l"))
      local calls, exec = recorder()
      nav.move_cursor_left(exec)
      T.eq(vim.api.nvim_get_current_win(), left)
      T.eq(#calls, 0)
    end)
  end)
end)

T.test("nav: move at the nvim edge focuses the adjacent Rex pane", function()
  with_layout(function()
    with_env("REX_BIN", nil, function()
      with_state(STATE, function()
        local calls, exec = recorder()
        nav.move_cursor_left(exec)
        T.eq(#calls, 1)
        T.eq(calls[1], { "rex", "focus", "-s", "session:s1", "left" })
      end)
    end)
  end)
end)

T.test("nav: edge moves honor REX_BIN", function()
  with_layout(function()
    with_env("REX_BIN", "/opt/rex-nvim/bin/rex", function()
      with_state(STATE, function()
        local calls, exec = recorder()
        nav.move_cursor_left(exec)
        T.eq(#calls, 1)
        T.eq(calls[1], { "/opt/rex-nvim/bin/rex", "focus", "-s", "session:s1", "left" })
      end)
    end)
  end)
end)

T.test("nav: edge move without sidebar state is a silent no-op", function()
  with_layout(function()
    with_env("REX_NVIM_KEY", nil, function()
      local calls, exec = recorder()
      nav.move_cursor_left(exec)
      T.eq(#calls, 0)
      T.eq(#vim.api.nvim_list_wins(), 1)
    end)
  end)
end)

T.test("nav: resize between nvim splits grows the adjacent window locally", function()
  with_layout(function()
    with_env("REX_BIN", nil, function()
      vim.cmd("vsplit")
      local left = win_in("h")
      vim.api.nvim_set_current_win(win_in("l"))
      local before = vim.fn.winwidth(left)
      local calls, exec = recorder()
      nav.resize_left(5, exec)
      T.eq(vim.fn.winwidth(left), before + 5)
      T.eq(#calls, 0)
    end)
  end)
end)

T.test("nav: resize at the nvim edge asks Rex to resize the sidebar pane", function()
  with_layout(function()
    with_env("REX_BIN", nil, function()
      with_state(STATE, function()
        local calls, exec = recorder()
        nav.resize_left(30, exec)
        T.eq(#calls, 1)
        T.eq({ calls[1][1], calls[1][2], calls[1][3], calls[1][4], calls[1][5], calls[1][6] },
          { "rex", "do", "-s", "session:s1", "rex_nvim_resize", "--args" })
        T.eq(vim.json.decode(calls[1][7]),
          { direction = "left", amount = 30, mux = true, block_id = "block:9" })
      end)
    end)
  end)
end)

T.test("nav: resize uses config.resize.amount when no amount is given", function()
  with_config(function()
    with_layout(function()
      with_env("REX_BIN", nil, function()
        with_state(STATE, function()
          rn.setup({ resize = { amount = 7 } })
          local calls, exec = recorder()
          nav.resize_left(nil, exec)
          T.eq(#calls, 1)
          T.eq(vim.json.decode(calls[1][7]).amount, 7)
        end)
      end)
    end)
  end)
end)

-- Direction mapping at the nvim edge, for both the focus forwarding and the
-- mux resize args.
for _, dir in ipairs({ "left", "down", "up", "right" }) do
  T.test("nav: edge move direction " .. dir .. " reaches rex focus", function()
    with_layout(function()
      with_env("REX_BIN", nil, function()
        with_state(STATE, function()
          local calls, exec = recorder()
          nav["move_cursor_" .. dir](exec)
          T.eq(#calls, 1)
          T.eq(calls[1], { "rex", "focus", "-s", "session:s1", dir })
        end)
      end)
    end)
  end)

  T.test("nav: edge resize direction " .. dir .. " reaches rex_nvim_resize", function()
    with_layout(function()
      with_env("REX_BIN", nil, function()
        with_state(STATE, function()
          local calls, exec = recorder()
          nav["resize_" .. dir](12, exec)
          T.eq(#calls, 1)
          T.eq(vim.json.decode(calls[1][7]),
            { direction = dir, amount = 12, mux = true, block_id = "block:9" })
        end)
      end)
    end)
  end)
end