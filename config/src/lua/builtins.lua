--- Check whether a string starts with a given substring.
--- @param s string
--- @param start string
function string.starts_with(s, start)
    return s:sub(1, #start) == start
end

--- Check whether a string ends with a given substring.
--- @param s string
--- @param ending string
function string.ends_with(s, ending)
    return ending == "" or s:sub(- #ending) == ending
end

--- Split a string by separator.
--- @param str string
--- @param separator string
--- @return string[]
function string.split(str, separator)
    separator = separator or "%s"

    local t = {}
    for str in string.gmatch(str, "([^" .. separator .. "]+)") do
        table.insert(t, str)
    end

    return t
end

--- Merge two tables. On collision table2 has priority.
--- @param table1 table
--- @param table2 table
--- @return table
function table.merge(table1, table2)
    local merged = {}
    for k, v in pairs(table1) do
        merged[k] = v
    end
    for k, v in pairs(table2) do
        merged[k] = v
    end
    return merged
end

--- Print table keys and values.
--- @param table table
function table.print(table)
    for k, v in pairs(table) do
        print(k .. "=" .. v)
    end
end

--- Concat paths together.
--- @param path string
--- @param ... string
function Path(path, ...)
    for _, v in ipairs({ ... }) do
        path = chariot.concat_paths(path, v)
    end
    return path
end
