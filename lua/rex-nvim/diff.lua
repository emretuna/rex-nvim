local M = {}

-- Comment context for non-file buffers: diff views, git panels, scratches.
-- Real file buffers return nil -- the live pipeline already handles them,
-- including diff plugins' working-side buffers (diffview LOCAL, gitsigns
-- inline signs on the real buffer). Resolver order: user resolvers from
-- setup({ diff = { resolvers = { ... } } }), built-in handlers, generic label.
-- A resolver is fn(bufnr, name) -> { path?, label } | nil (label required);
-- pcall-guarded, first non-nil result wins.

local function url_decode(s)
  return (s:gsub("%%(%x%x)", function(h) return string.char(tonumber(h, 16)) end))
end

-- Linked worktree: gitdir is <main>/.git/worktrees/<name>, so its parent is
-- NOT the working tree. The `gitdir` file inside points at the worktree's
-- own .git file, whose parent is the real root.
local function worktree_root(gitdir)
  local f = io.open(gitdir .. "/gitdir", "r")
  if not f then return nil end
  local dotgit = f:read("*l")
  f:close()
  if not dotgit or dotgit == "" then return nil end
  return vim.fs.dirname(vim.trim(dotgit))
end

-- Shared shape: fugitive://<gitdir>//<rev>/<path> and
-- gitsigns://<gitdir>//<base>:<relpath> (sources in Context).
local function gitdir_uri(name)
  local gitdir, rest = name:match("^[^:]+://(.-)//(.+)$")
  if not gitdir or gitdir == "" or not rest then return nil end
  local decoded = url_decode(gitdir)
  local toplevel
  if decoded:match("/%.git/worktrees/[^/]+$") then
    toplevel = worktree_root(decoded)
  else
    toplevel = vim.fs.dirname(decoded)
  end
  if not toplevel then return nil end
  local base, relpath = rest:match("^(:%d):(.+)$")
  if not base then base, relpath = rest:match("^(.-):(.+)$") end
  if base and relpath then
    return { path = toplevel .. "/" .. relpath, label = "diff, " .. base }
  end
  local rev, path = rest:match("^([^/]+)/(.+)$")
  if rev and path then
    return { path = toplevel .. "/" .. path, label = "diff, " .. rev:sub(1, 7) }
  end
  return nil
end

local function builtin(bufnr, name)
  if name:match("^diffview://") then
    if name == "diffview://null" then return { label = "diffview" } end
    local ok, ctx = pcall(function()
      local view = require("diffview.lib").get_current_view()
      local entry = view and view:infer_cur_file()
      if entry and entry.absolute_path then
        return { path = entry.absolute_path, label = "diff, " .. (entry.kind or "working") }
      end
      return nil
    end)
    if ok and ctx then return ctx end
    return { label = "diffview" }
  end
  if name:match("^fugitive://") or name:match("^gitsigns://") then
    return gitdir_uri(name) or { label = name:match("^([^:]+)") }
  end
  if name:match("^gitsigns%-") then
    -- gitsigns-diff:// (panel), gitsigns-commit:// (message), gitsigns-blame:
    return { label = (name:match("^(gitsigns%-[^:/]+)") or "gitsigns") }
  end
  local tail = vim.fn.fnamemodify(name, ":t")
  if tail:match("^Neogit") then
    if tail == "NeogitStatus" then
      local ok, ctx = pcall(function()
        local status = require("neogit").status.instance()
        local found = status and status.buffer.ui:get_hunk_or_filename_under_cursor()
        local hunk = found and found.hunk
        local git_dir = vim.b[bufnr].neogit_git_dir
        if hunk and hunk.file and git_dir then
          return { path = vim.fs.dirname(git_dir) .. "/" .. hunk.file, label = "diff, hunk" }
        end
        return nil
      end)
      if ok and ctx then return ctx end
    end
    return { label = tail }
  end
  return nil
end

function M.context(bufnr)
  local name = vim.api.nvim_buf_get_name(bufnr)
  if vim.bo[bufnr].buftype == "" and name ~= "" and not name:match("^[%w+.-]+://") then
    return nil
  end
  local ok, cfg = pcall(function() return require("rex-nvim").config end)
  for _, resolver in ipairs(ok and cfg.diff and cfg.diff.resolvers or {}) do
    local rok, res = pcall(resolver, bufnr, name)
    if rok and type(res) == "table" and res.label then return res end
  end
  local built = builtin(bufnr, name)
  if built then return built end
  local scheme = name:match("^([%w+.-]+)://")
  local ft = vim.bo[bufnr].filetype
  return { label = scheme or (ft ~= "" and ft) or vim.bo[bufnr].buftype or "scratch" }
end

return M