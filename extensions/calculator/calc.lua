-- A small expression evaluator (recursive descent). It never uses `load`, so an expression can only
-- compute numbers: there is no way to reach the rest of the Lua environment from user input.
--
--   expr    = term { ("+" | "-") term }
--   term    = unary { ("*" | "/" | "%" | "mod") unary }
--   unary   = ("-" | "+") unary | power
--   power   = postfix [ "^" unary ]          (right associative: 2^3^2 = 2^(3^2))
--   postfix = primary { "!" }                (factorial)
--   primary = number | name | name "(" [ expr { "," expr } ] ")" | "(" expr ")"

local M = {}

local CONSTANTS = { pi = math.pi, e = math.exp(1), tau = 2 * math.pi, inf = math.huge }

local function factorial(n)
    if n < 0 or n ~= math.floor(n) then error("factorial needs a non-negative integer", 0) end
    if n > 170 then return math.huge end
    local r = 1.0
    for i = 2, n do r = r * i end
    return r
end

local FUNCTIONS = {
    sqrt = function(x) if x < 0 then error("sqrt of a negative number", 0) end return math.sqrt(x) end,
    abs = math.abs, floor = math.floor, ceil = math.ceil,
    sin = math.sin, cos = math.cos, tan = math.tan,
    asin = math.asin, acos = math.acos, atan = math.atan,
    exp = math.exp,
    ln = function(x) if x <= 0 then error("ln of a non-positive number", 0) end return math.log(x) end,
    log = function(x) if x <= 0 then error("log of a non-positive number", 0) end return math.log(x, 10) end,
    log2 = function(x) if x <= 0 then error("log2 of a non-positive number", 0) end return math.log(x, 2) end,
    round = function(x, digits)
        local m = 10 ^ (digits or 0)
        return math.floor(x * m + 0.5) / m
    end,
    min = function(...) return math.min(...) end,
    max = function(...) return math.max(...) end,
}

local function tokenize(src)
    local tokens, i, n = {}, 1, #src
    while i <= n do
        local c = src:sub(i, i)
        if c:match("%s") then
            i = i + 1
        elseif c:match("[%d.]") then
            local num = src:match("^%d[%d_]*%.?%d*[eE][+-]?%d+", i) or src:match("^%.%d+[eE][+-]?%d+", i)
                or src:match("^%d[%d_]*%.?%d*", i) or src:match("^%.%d+", i)
            if not num then error("unexpected '" .. c .. "' at position " .. i, 0) end
            local value = tonumber((num:gsub("_", "")))
            if value then value = value + 0.0 end -- floats only: Lua integers would silently wrap around on overflow
            if not value then error("bad number '" .. num .. "'", 0) end
            tokens[#tokens + 1] = { t = "num", v = value, pos = i }
            i = i + #num
        elseif c:match("[%a_]") then
            local name = src:match("^[%a_][%w_]*", i)
            tokens[#tokens + 1] = { t = "name", v = name:lower(), pos = i }
            i = i + #name
        elseif c == "*" and src:sub(i + 1, i + 1) == "*" then
            tokens[#tokens + 1] = { t = "op", v = "^", pos = i }
            i = i + 2
        elseif c:match("[%+%-%*/%%%^%(%),!]") then
            tokens[#tokens + 1] = { t = "op", v = c, pos = i }
            i = i + 1
        else
            error("unexpected '" .. c .. "' at position " .. i, 0)
        end
    end
    return tokens
end

--- Evaluates `src`. Returns the number, or `nil, message`.
function M.eval(src)
    if type(src) ~= "string" then return nil, "expected text" end
    if #src > 500 then return nil, "expression too long" end
    local ok, result = pcall(function()
        local tokens = tokenize(src)
        local pos, depth = 1, 0
        local function peek() return tokens[pos] end
        local function take() pos = pos + 1; return tokens[pos - 1] end
        local function is_op(v) local t = peek(); return t and t.t == "op" and t.v == v end
        local function is_name(v) local t = peek(); return t and t.t == "name" and t.v == v end

        local expr

        local function primary()
            local t = take()
            if not t then error("unexpected end of expression", 0) end
            if t.t == "num" then return t.v end
            if t.t == "name" then
                if is_op("(") then
                    local fn = FUNCTIONS[t.v]
                    if not fn then error("unknown function '" .. t.v .. "'", 0) end
                    take()
                    local args = {}
                    if not is_op(")") then
                        repeat args[#args + 1] = expr() until not (is_op(",") and take())
                    end
                    if not is_op(")") then error("missing ')' after the arguments of " .. t.v, 0) end
                    take()
                    return fn(table.unpack(args))
                end
                local c = CONSTANTS[t.v]
                if c == nil then error("unknown name '" .. t.v .. "'", 0) end
                return c
            end
            if t.t == "op" and t.v == "(" then
                depth = depth + 1
                if depth > 50 then error("too many nested parentheses", 0) end
                local v = expr()
                if not is_op(")") then error("missing ')'", 0) end
                take()
                depth = depth - 1
                return v
            end
            error("unexpected '" .. tostring(t.v) .. "' at position " .. t.pos, 0)
        end

        local unary

        local function postfix()
            local v = primary()
            while is_op("!") do take(); v = factorial(v) end
            return v
        end

        local function power()
            local base = postfix()
            if is_op("^") then
                take()
                return base ^ unary()
            end
            return base
        end

        function unary()
            if is_op("-") then take(); return -unary() end
            if is_op("+") then take(); return unary() end
            return power()
        end

        local function term()
            local v = unary()
            while true do
                if is_op("*") then take(); v = v * unary()
                elseif is_op("/") then
                    take()
                    local d = unary()
                    if d == 0 then error("division by zero", 0) end
                    v = v / d
                elseif is_op("%") or is_name("mod") then
                    take()
                    local d = unary()
                    if d == 0 then error("division by zero", 0) end
                    v = v % d
                elseif peek() and (peek().t == "num" or peek().t == "name" or is_op("(")) then
                    v = v * unary() -- implicit multiplication: 2pi, 3(4+1)
                else
                    return v
                end
            end
        end

        function expr()
            local v = term()
            while is_op("+") or is_op("-") do
                local op = take().v
                local r = term()
                if op == "+" then v = v + r else v = v - r end
            end
            return v
        end

        if #tokens == 0 then error("empty expression", 0) end
        local v = expr()
        if peek() then error("unexpected '" .. tostring(peek().v) .. "' at position " .. peek().pos, 0) end
        return v
    end)
    if not ok then return nil, tostring(result) end
    if result ~= result then return nil, "not a number" end
    return result
end

--- Compact display: integers without a decimal point, otherwise up to 12 significant digits.
function M.format(n)
    if n == math.huge then return "∞" elseif n == -math.huge then return "-∞" end
    if n == math.floor(n) and math.abs(n) < 1e15 then return string.format("%d", n) end
    local s = string.format("%.12g", n)
    return s
end

return M
