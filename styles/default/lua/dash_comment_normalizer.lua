local function get_number(name, default)
    local v = mj_config[name]
    if v == nil then
        return default
    end
    return tonumber(v) or default
end

local function get_string(name, default)
    local v = mj_config[name]
    if v == nil then
        return default
    end
    return tostring(v)
end

local function is_dash_line(line)
    return line:match("^%s*//%-+%s*$") ~= nil
end

local function adjacent_title_length(lines, idx)
    local prev = idx - 1
    local next = idx + 1
    for _, neighbor in ipairs({prev, next}) do
        if neighbor >= 1 and neighbor <= #lines then
            local body = lines[neighbor]:gsub("[\r\n]+", "")
            local trimmed = body:match("^%s*(.-)%s*$")
            if trimmed:sub(1, 2) == "//" then
                local content = trimmed:sub(3):match("^%s*(.-)%s*$")
                if content ~= "" and not content:match("^%-+$") then
                    return #trimmed
                end
            end
        end
    end
    return nil
end

function apply(text, path)
    local long_length = get_number("long_length", 64)
    local short_length = get_number("short_length", 28)
    local long_threshold = get_number("long_threshold", 50)
    local mode = get_string("mode", "threshold"):lower()
    local min_length = get_number("min_length", short_length)

    local lines = mj.split_lines(text)
    local changed = false

    for i = 1, #lines do
        local line = lines[i]
        local body = line:gsub("[\r\n]+", "")
        if is_dash_line(body) then
            local current_len = #body:gsub("^%s*", ""):gsub("%s*$", "")
            local target_len = (current_len >= long_threshold) and long_length or short_length

            if mode == "auto" then
                local title_len = adjacent_title_length(lines, i)
                if title_len ~= nil then
                    target_len = math.max(current_len, title_len, min_length)
                end
            end
            if target_len >= 2 then
                local indent = body:match("^(%s*)//") or ""
                local normalized = indent .. "//" .. string.rep("-", target_len - 2)
                if normalized ~= body then
                    lines[i] = normalized .. (line:match("(\r?\n)$") or "")
                    changed = true
                end
            end
        end
    end

    if not changed then
        return text
    end
    return mj.join_lines(lines)
end
