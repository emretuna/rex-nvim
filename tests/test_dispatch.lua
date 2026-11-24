local dispatch = require("rex-nvim.dispatch")

local function recorder(fail_on)
  local calls = {}
  return calls, function(argv)
    table.insert(calls, argv)
    if fail_on and #calls == fail_on then return { code = 1, stdout = "", stderr = "boom" } end
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

-- A paste goes through `rex api call -s SESSION block.event '<json>'`: return
-- the session it was scoped to and the decoded event it carried.
local function paste_call(argv)
  T.eq(argv[1], "rex")
  T.eq({ argv[2], argv[3], argv[4] }, { "api", "call", "-s" })
  T.eq(argv[6], "block.event")
  return argv[5], vim.json.decode(argv[7])
end

T.test("dispatch: paste mode pastes text only, no Enter", function()
  with_env("REX_SESSION", "session:s1", function()
    local calls, exec = recorder()
    local ok = dispatch.send("block:9", "line1\nline2", { submit = false }, exec)
    T.ok(ok)
    T.eq(#calls, 1)
    local session, event = paste_call(calls[1])
    T.eq(session, "session:s1")
    T.eq(event, { block_id = "block:9", event = { type = "paste", payload = { text = "line1\nline2" } } })
  end)
end)

T.test("dispatch: submit mode pastes, then presses Enter", function()
  with_env("REX_SESSION", "session:s1", function()
    local calls, exec = recorder()
    local ok = dispatch.send("block:9", "hi", { submit = true }, exec)
    T.ok(ok)
    T.eq(#calls, 2)
    local session, event = paste_call(calls[1])
    T.eq(session, "session:s1")
    T.eq(event.event.type, "paste")
    T.eq(calls[2], { "rex", "send-key", "-s", "session:s1", "-b", "block:9", "enter" })
  end)
end)

T.test("dispatch: failed paste returns err", function()
  local _, exec = recorder(1)
  local ok, err = dispatch.send("block:9", "hi", { submit = true }, exec)
  T.eq(ok, false)
  T.ok(err:match("rex paste failed"))
  T.ok(err:match("boom"))
end)

T.test("dispatch: failed Enter press returns err after the paste", function()
  local calls, exec = recorder(2)
  local ok, err = dispatch.send("block:9", "hi", { submit = true }, exec)
  T.eq(ok, false)
  T.ok(err:match("rex send%-key failed") and err:match("boom"))
  T.eq(#calls, 2)
end)

T.test("dispatch: the rex binary comes from REX_BIN", function()
  with_env("REX_BIN", "/opt/rex-nvim/bin/rex", function()
    local calls, exec = recorder()
    T.ok(dispatch.send("block:9", "hi", { submit = false }, exec))
    T.eq(calls[1][1], "/opt/rex-nvim/bin/rex")
  end)
end)
