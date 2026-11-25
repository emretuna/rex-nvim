-- Diff-aware annotations: non-file buffers (diff views, git panels, scratches)
-- resolve or degrade to a label, and their text is snapshotted at capture so it
-- survives the view being wiped.
local rn = require("rex-nvim")
local comments = require("rex-nvim.comments")
local prompt = require("rex-nvim.prompt")
local ui = require("rex-nvim.ui")
local dispatch = require("rex-nvim.dispatch")
local agents = require("rex-nvim.agents")
local diff = require("rex-nvim.diff")

local function drop(bufnr)
  if vim.api.nvim_buf_is_valid(bufnr) then
    vim.api.nvim_buf_delete(bufnr, { force = true })
  end
end

-- nvim expands a relative non-scheme name against the cwd when it is set.
local function expand_name(name)
  if name:match("^[%w+.-]+://") or name:sub(1, 1) == "/" then return name end
  return vim.fn.getcwd() .. "/" .. name
end

-- A failed test skips its cleanup; a leftover buffer would then make the next
-- test's nvim_buf_set_name fail on the duplicate. Reclaim the name first.
local function drop_named(name)
  for _, b in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_valid(b) and vim.api.nvim_buf_get_name(b) == expand_name(name) then
      vim.api.nvim_buf_delete(b, { force = true })
    end
  end
end

local function scratch(lines, name)
  local b = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_buf_set_lines(b, 0, -1, false, lines)
  if name then
    drop_named(name)
    vim.api.nvim_buf_set_name(b, name)
  end
  return b
end

-- Replace obj[key] for the duration of fn, then put the original back.
local function stub(obj, key, value, fn)
  local had = obj[key]
  obj[key] = value
  local ok, err = pcall(fn)
  obj[key] = had
  if not ok then error(err, 0) end
end

-- Same for a package.loaded entry (plugin module stubs / removal).
local function stub_loaded(name, value, fn)
  local had = package.loaded[name]
  package.loaded[name] = value
  local ok, err = pcall(fn)
  package.loaded[name] = had
  if not ok then error(err, 0) end
end

-- Capture a comment on `bufnr` through the public API with a stubbed prompt.
local function comment(bufnr, first, last, text)
  vim.api.nvim_set_current_buf(bufnr)
  stub(ui, "input_comment", function(cb) cb(text) end, function()
    rn.comment_range(first, last)
  end)
end

local function diffview_view(absolute_path, kind)
  return {
    get_current_view = function()
      return {
        infer_cur_file = function()
          return { absolute_path = absolute_path, kind = kind }
        end,
      }
    end,
  }
end

local function neogit_status(file)
  return {
    status = {
      instance = function()
        return {
          buffer = {
            ui = {
              get_hunk_or_filename_under_cursor = function()
                return { hunk = { file = file } }
              end,
            },
          },
        }
      end,
    },
  }
end

local DIFFVIEW_NAME = "diffview:///repo/.git/:0:/lua/foo.lua"

T.test("diff: a diffview buffer resolves its path and snapshots the full hunk", function()
  comments.clear()
  local b = scratch({
    "-line1",
    "+line1 changed",
    "@@ -1,3 +1,4 @@",
    " context",
    "+DIFF-SMOKE-MARKER",
  }, DIFFVIEW_NAME)
  stub_loaded("diffview.lib", diffview_view("/repo/lua/foo.lua", "working"), function()
    comment(b, 1, 5, "left side")
  end)
  local l = comments.list()
  T.eq(#l, 1)
  local c = l[1]
  T.eq({ c.diff, c.file, c.label }, { true, "/repo/lua/foo.lua", "diff, working" })
  T.eq(comments.snippet(c.id), {
    "-line1",
    "+line1 changed",
    "@@ -1,3 +1,4 @@",
    " context",
    "+DIFF-SMOKE-MARKER",
  })
  drop(b)
end)

T.test("diff: a comment survives its diff buffer being wiped", function()
  comments.clear()
  local b = scratch({ "-old", "+new", "+extra" }, "diffview:///repo/.git/:0:/lua/bar.lua")
  stub_loaded("diffview.lib", diffview_view("/repo/lua/bar.lua", "working"), function()
    comment(b, 1, 3, "snapshot me")
  end)
  local id = comments.list()[1].id
  local before = comments.snippet(id)
  vim.api.nvim_buf_delete(b, { force = true })
  T.eq(vim.api.nvim_buf_is_valid(b), false)
  local c = comments.get(id)
  T.ok(c, "a wiped diff buffer must still resolve its comment")
  T.eq({ c.file, c.label }, { "/repo/lua/bar.lua", "diff, working" })
  T.eq(comments.snippet(id), before, "the snapshot is the captured text")
  T.eq(#comments.list(), 1)
end)

T.test("prompt: diff items show path (diff, kind) with the full snippet", function()
  local s = prompt.format({
    { comment = { file = "/repo/lua/foo.lua", start_line = 1, end_line = 5, text = "diff note",
        diff = true, label = "diff, working" },
      snippet = { "-line1", "+line1 changed", "@@ -1,3 +1,4 @@", " context", "+DIFF-SMOKE-MARKER" } },
    { comment = { file = "/repo/a.rs", start_line = 1, end_line = 9, text = "file note" },
      snippet = { "l1", "l2", "l3", "l4" } },
  }, { cwd = "/repo" })
  T.ok(s:find("1. lua/foo.lua (diff, working)\n", 1, true), "diff location names the path and kind")
  for _, line in ipairs({ "-line1", "+line1 changed", "@@ -1,3 +1,4 @@", " context", "+DIFF-SMOKE-MARKER" }) do
    T.ok(s:find("   > " .. line .. "\n", 1, true), "diff snippet line kept: " .. line)
  end
  T.ok(s:find("2. a.rs:1-9\n", 1, true), "file comments keep their line location")
  T.ok(s:find("   > l3", 1, true))
  T.ok(not s:find("> l4", 1, true), "file comments stay capped at 3 lines")
end)

T.test("diff: a diffview buffer without a view degrades to a label, never a pseudo-path", function()
  comments.clear()
  local b = scratch({ "a", "b" }, "diffview://x")
  stub_loaded("diffview.lib", nil, function()
    comment(b, 1, 2, "label only")
  end)
  local c = comments.list()[1]
  T.eq({ c.diff, c.file, c.label }, { true, "", "diffview" })
  drop(b)
end)

T.test("diff: NeogitStatus resolves the hunk's file through the status buffer", function()
  comments.clear()
  local b = scratch({ "M file.txt", "" }, "NeogitStatus")
  vim.b[b].neogit_git_dir = "/repo/.git"
  stub_loaded("neogit", neogit_status("lua/foo.lua"), function()
    comment(b, 1, 1, "hunk")
  end)
  local c = comments.list()[1]
  T.eq({ c.diff, c.file, c.label }, { true, "/repo/lua/foo.lua", "diff, hunk" })
  drop(b)
end)

T.test("diff: NeogitStatus without the plugin keeps an honest label", function()
  comments.clear()
  local b = scratch({ "M file.txt" }, "NeogitStatus")
  vim.b[b].neogit_git_dir = "/repo/.git"
  stub_loaded("neogit", nil, function()
    comment(b, 1, 1, "hunk")
  end)
  local c = comments.list()[1]
  T.eq({ c.diff, c.file, c.label }, { true, "", "NeogitStatus" })
  drop(b)
end)

T.test("ref: a diff buffer is refused with guidance and nothing is dispatched", function()
  comments.clear()
  local b = scratch({ "-old", "+new" }, DIFFVIEW_NAME)
  local msgs, sends = {}, 0
  local o_notify, o_send, o_list = vim.notify, dispatch.send, agents.list
  vim.notify = function(msg, level) msgs[#msgs + 1] = { msg = msg, level = level } end
  dispatch.send = function() sends = sends + 1; return true end
  agents.list = function()
    return { { block_id = "block:z9", window_id = "window:w1", kind = "pi", title = "pi",
      status = "idle", cwd = "/repo" } }
  end
  vim.api.nvim_set_current_buf(b)
  local ok, err = pcall(rn.ref_range, 1, 2)
  vim.notify, dispatch.send, agents.list = o_notify, o_send, o_list
  if not ok then error(err, 0) end
  T.eq(sends, 0, "a view line is not a file line")
  T.eq(#msgs, 1)
  T.eq(msgs[1].level, vim.log.levels.WARN)
  T.ok(msgs[1].msg:find("no file lines", 1, true))
  drop(b)
end)

T.test("send: diff comments take the git cwd from their resolved path", function()
  comments.clear()
  local b = scratch({ "-old", "+new", "+extra" }, DIFFVIEW_NAME)
  stub_loaded("diffview.lib", diffview_view("/repo/lua/foo.lua", "working"), function()
    comment(b, 1, 3, "diff note")
  end)
  local recorded_cwd, sent_text
  local o_ctx, o_list, o_send = rn._git_context, agents.list, dispatch.send
  rn._git_context = function(cwd) recorded_cwd = cwd; return "MARKER" end
  agents.list = function()
    return { { block_id = "block:z9", window_id = "window:w1", kind = "pi", title = "pi",
      status = "idle", cwd = "/repo" } }
  end
  dispatch.send = function(_, text) sent_text = text; return true end
  local ok, err = pcall(rn.send_all, { submit = false })
  rn._git_context, agents.list, dispatch.send = o_ctx, o_list, o_send
  if not ok then error(err, 0) end
  T.eq(recorded_cwd, "/repo/lua", "the snapshot path's directory supplies the git cwd")
  T.ok(sent_text:find("MARKER", 1, true), "payload carries the git context header")
  T.ok(sent_text:find("lua/foo.lua (diff, working)", 1, true))
  drop(b)
end)

T.test("diff: real file buffers keep the live pipeline and carry no diff flag", function()
  comments.clear()
  local b = scratch({ "alpha", "beta" }, "/repo/lua/bar.lua")
  vim.bo[b].buftype = ""
  T.eq(diff.context(b), nil, "a named buftype-empty buffer is a file buffer")
  comment(b, 1, 2, "check")
  local c = comments.list()[1]
  T.eq(c.diff, nil)
  T.eq(c.file, "/repo/lua/bar.lua")
  vim.api.nvim_buf_set_lines(b, 1, 2, false, { "BETA-EDITED" })
  T.eq(comments.snippet(c.id), { "alpha", "BETA-EDITED" }, "file snippets stay live")
  drop(b)
end)

T.test("diff: fugitive URIs resolve to the working-tree path", function()
  comments.clear()
  local rev = "abcdefabcdefabcdefabcdefabcdefabcdefabcd"
  local b = scratch({ "blob line" }, "fugitive:///repo/.git//" .. rev .. "/lua/foo.lua")
  comment(b, 1, 1, "blob")
  local c = comments.list()[1]
  T.eq({ c.diff, c.file, c.label }, { true, "/repo/lua/foo.lua", "diff, abcdefa" })
  drop(b)

  comments.clear()
  local b2 = scratch({ "index line" }, "fugitive:///repo/.git//0/lua/foo.lua")
  comment(b2, 1, 1, "index")
  local c2 = comments.list()[1]
  T.eq({ c2.diff, c2.file, c2.label }, { true, "/repo/lua/foo.lua", "diff, 0" })
  drop(b2)
end)

T.test("diff: gitsigns revision buffers resolve, panels keep a label", function()
  comments.clear()
  local b = scratch({ "-old", "+new" }, "gitsigns:///repo/.git//:0:lua/foo.lua")
  comment(b, 1, 2, "revision")
  local c = comments.list()[1]
  T.eq({ c.diff, c.file, c.label }, { true, "/repo/lua/foo.lua", "diff, :0" })
  drop(b)

  comments.clear()
  local b2 = scratch({ "panel row" }, "gitsigns-diff:///repo/.git//1")
  comment(b2, 1, 1, "panel")
  local c2 = comments.list()[1]
  T.eq({ c2.diff, c2.file, c2.label }, { true, "", "gitsigns-diff" })
  drop(b2)
end)

T.test("diff: unknown schemes and unnamed scratches fall back to honest labels", function()
  comments.clear()
  local b = scratch({ "x" }, "custom://whatever")
  comment(b, 1, 1, "unknown")
  local c = comments.list()[1]
  T.eq({ c.diff, c.file, c.label }, { true, "", "custom" })
  drop(b)

  comments.clear()
  local b2 = scratch({ "y" })
  vim.bo[b2].filetype = "git"
  comment(b2, 1, 1, "ft only")
  local c2 = comments.list()[1]
  T.eq({ c2.diff, c2.file, c2.label }, { true, "", "git" })
  drop(b2)
end)

T.test("diff: a user resolver overrides the built-in handlers", function()
  comments.clear()
  local saved = vim.deepcopy(rn.config)
  rn.setup({ diff = { resolvers = {
    function() return { path = "/x/y.lua", label = "mine" } end,
  } } })
  local b = scratch({ "a" }, DIFFVIEW_NAME)
  stub_loaded("diffview.lib", diffview_view("/repo/lua/foo.lua", "working"), function()
    comment(b, 1, 1, "user resolver")
  end)
  rn.config = saved
  local c = comments.list()[1]
  T.eq({ c.diff, c.file, c.label }, { true, "/x/y.lua", "mine" })
  drop(b)
end)

T.test("diff: an unnamed scratch comment survives being wiped", function()
  comments.clear()
  local b = scratch({ "one", "two" })
  comment(b, 1, 2, "scratch note")
  local c = comments.list()[1]
  local id = c.id
  T.eq({ c.diff, c.file, c.label }, { true, "", "nofile" })
  vim.api.nvim_buf_delete(b, { force = true })
  T.eq(comments.snippet(id), { "one", "two" })
  T.eq(#comments.list(), 1)
end)

T.test("diff: linked-worktree gitdirs resolve to the worktree root", function()
  comments.clear()
  local base = vim.fn.tempname()
  local gitdir = base .. "/main/.git/worktrees/wt"
  vim.fn.mkdir(gitdir, "p")
  vim.fn.writefile({ base .. "/wt/.git" }, gitdir .. "/gitdir")
  local b = scratch({ "-old", "+new" }, "gitsigns://" .. gitdir .. "//:0:lua/foo.lua")
  comment(b, 1, 2, "worktree")
  local c = comments.list()[1]
  T.eq({ c.file, c.label }, { base .. "/wt/lua/foo.lua", "diff, :0" })
  drop(b)

  -- Without the gitdir file the root is unknowable: label only, never a guess.
  comments.clear()
  vim.fn.delete(gitdir .. "/gitdir")
  local b2 = scratch({ "x" }, "gitsigns://" .. gitdir .. "//:0:lua/foo.lua")
  comment(b2, 1, 1, "orphan")
  local c2 = comments.list()[1]
  T.eq({ c2.file, c2.label }, { "", "gitsigns" })
  drop(b2)
  vim.fn.delete(base, "rf")
end)