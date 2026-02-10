local function normalize_mapping(map)
    local out = {}
    if type(map) ~= "table" then
        return out
    end
    for k, v in pairs(map) do
        if k ~= nil and v ~= nil then
            out[string.lower(tostring(k))] = tostring(v)
        end
    end
    return out
end

function apply(text, path)
    local mapping = normalize_mapping(mj_config["mapping"])
    if next(mapping) == nil then
        return text
    end

    local lines = mj.split_lines(text)
    local changed = false

    for i = 1, #lines do
        local line = lines[i]
        local body = line:gsub("[\r\n]+", "")
        local indent, comment = body:match("^(%s*)//%s*(.-)%s*$")
        if indent ~= nil and comment ~= nil then
            if comment ~= "" and not comment:match("^%-+$") then
                local key = string.lower(comment)
                local target = mapping[key]
                if target ~= nil then
                    local normalized = indent .. "// " .. target
                    if normalized ~= body then
                        lines[i] = normalized .. (line:match("(\r?\n)$") or "")
                        changed = true
                    end
                end
            end
        end
    end

    if not changed then
        return text
    end
    return mj.join_lines(lines)
end
