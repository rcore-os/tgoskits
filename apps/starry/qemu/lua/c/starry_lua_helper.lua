local M = {}

function M.answer()
    return 21 * 2
end

function M.join(values, separator)
    return table.concat(values, separator)
end

function M.write_lines(path, lines)
    local file = assert(io.open(path, "w"))
    for _, line in ipairs(lines) do
        assert(file:write(line, "\n"))
    end
    assert(file:close())
end

function M.read_all(path)
    local file = assert(io.open(path, "r"))
    local data = assert(file:read("*a"))
    assert(file:close())
    return data
end

return M
