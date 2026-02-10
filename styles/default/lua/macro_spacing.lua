local function is_macro_header(lines, idx)
    if idx < 2 then
        return false
    end
    local title = lines[idx] or ""
    if not title:find("user defined macros") then
        return false
    end
    local above = lines[idx - 1] or ""
    local below = lines[idx + 1] or ""
    return above:match("^//%-+") ~= nil and below:match("^//%-+") ~= nil
end

local function ensure_blank_line_after_macro_header(lines)
    local i = 1
    while i <= #lines do
        if is_macro_header(lines, i) then
            local next_line = lines[i + 2] or ""
            if next_line:match("^#define") and next_line:match("^%s*$") == nil then
                table.insert(lines, i + 2, "\n")
            end
        end
        i = i + 1
    end
end

function apply(text, path)
    if not path:match("%.c$") and not path:match("%.cc$") and not path:match("%.cpp$") and not path:match("%.cxx$") then
        return text
    end
    local lines = mj.split_lines(text)
    ensure_blank_line_after_macro_header(lines)
    return mj.join_lines(lines)
end

