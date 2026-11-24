local agents = require("rex-nvim.agents")

-- What `rex do rex_nvim_agents --json` prints (the rex action in step 6).
local fixture = vim.json.encode({
  agents = {
    { block_id = "block:a1", session_id = "session:s1", window_id = "window:w1", kind = "pi",
      status = "idle", cwd = "/tmp/proj-a", title = "π - proj-a" },
    { block_id = "block:b2", session_id = "session:s2", window_id = "window:w2", kind = "claude",
      status = "working", cwd = "/tmp/proj-b", title = "claude" },
  },
})

local function fake_exec(out, code)
  return function(_) return { code = code or 0, stdout = out, stderr = "" } end
end

local function with_env(name, value, fn)
  local previous = vim.env[name]
  vim.env[name] = value
  local ok, err = pcall(fn)
  vim.env[name] = previous
  if not ok then error(err, 0) end
end

T.test("agents: parses list and normalizes fields", function()
  with_env("REX_SESSION", nil, function()
    local list, err = agents.list(fake_exec(fixture))
    T.eq(err, nil)
    T.eq(#list, 2)
    local by_block = {}
    for _, agent in ipairs(list) do by_block[agent.block_id] = agent end
    T.eq(by_block["block:a1"].kind, "pi")
    T.eq(by_block["block:a1"].status, "idle")
    T.eq(by_block["block:a1"].title, "π - proj-a")
    T.eq(by_block["block:b2"].title, "claude") -- falls back to kind
  end)
end)

T.test("agents: list runs the rex action and asks for JSON", function()
  local seen
  agents.list(function(argv) seen = argv; return { code = 0, stdout = fixture, stderr = "" } end)
  T.eq(seen, { "rex", "do", "rex_nvim_agents", "--json" })
end)

T.test("agents: the rex binary comes from REX_BIN", function()
  with_env("REX_BIN", "/opt/rex-nvim/bin/rex", function()
    local seen
    agents.list(function(argv) seen = argv; return { code = 0, stdout = fixture, stderr = "" } end)
    T.eq(seen[1], "/opt/rex-nvim/bin/rex")
  end)
end)

T.test("agents: no current session sorts by title", function()
  with_env("REX_SESSION", nil, function()
    local list = agents.list(fake_exec(fixture))
    T.eq(list[1].block_id, "block:b2")
    T.eq(list[1].title, "claude")
    T.eq(list[2].block_id, "block:a1")
    T.eq(list[2].title, "π - proj-a")
  end)
end)

T.test("agents: current session excludes other sessions", function()
  with_env("REX_SESSION", "session:s2", function()
    local list = agents.list(fake_exec(fixture))
    T.eq(#list, 1)
    T.eq(list[1].block_id, "block:b2")
  end)
end)

T.test("agents: CLI failure returns err", function()
  local list, err = agents.list(fake_exec("", 1))
  T.eq(list, nil)
  T.ok(err and err:match("rex"))
end)

T.test("agents: unparseable JSON returns err", function()
  local list, err = agents.list(fake_exec("not json"))
  T.eq(list, nil)
  T.ok(err and err:match("unparseable"))
end)

T.test("agents: resolve returns the lone session agent", function()
  local a = agents.resolve({ { block_id = "block:a1", window_id = "window:w1" } })
  T.eq(a.block_id, "block:a1")
end)

T.test("agents: resolve picks the single agent in the current window", function()
  with_env("REX_WINDOW_ID", "window:w2", function()
    local a = agents.resolve({
      { block_id = "block:a1", window_id = "window:w1" },
      { block_id = "block:a2", window_id = "window:w2" },
    })
    T.eq(a.block_id, "block:a2")
  end)
end)

T.test("agents: resolve returns nil when the window is ambiguous", function()
  with_env("REX_WINDOW_ID", "window:w1", function()
    local a = agents.resolve({
      { block_id = "block:a1", window_id = "window:w1" },
      { block_id = "block:a2", window_id = "window:w1" },
    })
    T.eq(a, nil)
  end)
end)

T.test("agents: resolve returns nil when no window context disambiguates", function()
  with_env("REX_WINDOW_ID", nil, function()
    local a = agents.resolve({
      { block_id = "block:a1", window_id = "window:w1" },
      { block_id = "block:b2", window_id = "window:w2" },
    })
    T.eq(a, nil)
  end)
end)

T.test("agents: display row leads with agent kind", function()
  local row = agents.display({ kind = "pi", title = "π - a", status = "idle", cwd = "/x/y/proj" })
  T.eq(row, "pi · idle · proj")
end)
