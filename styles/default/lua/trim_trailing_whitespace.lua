function apply(text, path)
    local lines = mj.split_lines(text)
    local changed = false
    for i = 1, #lines do
        local line = lines[i]
        local body = line:gsub("[\r\n]+", "")
        local ending = line:match("(\r?\n)$") or ""
        local trimmed = body:gsub("[ \t]+$", "")
        if trimmed ~= body then
            lines[i] = trimmed .. ending
            changed = true
        end
    end
    if not changed then
        return text
    end
    return mj.join_lines(lines)
end
