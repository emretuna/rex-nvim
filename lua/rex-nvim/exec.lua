local M = {}

-- The rex-nvim binary: the Rust half picks files, lists daemons, and prints
-- config for this plugin. The sidebar daemon inherits the path from the rex
-- side in REX_NVIM_BIN.
function M.self()
  return vim.env.REX_NVIM_BIN or "rex-nvim"
end

function M.default_exec(argv)
  local r = vim.system(argv, { text = true }):wait()
  return { code = r.code, stdout = r.stdout or "", stderr = r.stderr or "" }
end

return M
