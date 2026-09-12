-- Uninstalled, reviewed private P1 scene. Requires an explicit worker capability.
-- This file alone cannot arm collection or establish native evidence.
local capture = ...
assert(capture ~= nil, "private P1 capability required")
local sliver = require("sliver.v1")
local periodic = sliver.timer.every(1 / capture.rate, function()
    local phase = capture:status()
    if phase == "warmup" or phase == "measured" then sliver.redraw() end
end)

return {
    stop = function() periodic:cancel() end,
    key = function() error("unexpected key transition in private P1 fixture") end,
    visibility = function(event)
        assert(event.visible, "private P1 fixture lost visibility")
    end,
    touch = function(event)
        if event.phase == "down" then
            capture:advance_token()
            sliver.redraw()
        end
    end,
    api_version = 1,
    render = function(canvas)
        local frame = capture:frame()
        -- Identical full-frame raw animation in A/B/C; intended Lua time is not
        -- the phase clock. The token changes visible pixels, not only a label.
        local pixel = string.char(frame.frame_id % 251, (frame.token * 37) % 251, 113, 255)
        canvas:raw_pixels(pixel:rep(2008 * 60), "rgba8", 2008, 60, 8032,
            { x = 0, y = 0, width = 2008, height = 60 },
            { x = 0, y = 0, width = 2008, height = 60 }, "nearest")
        if capture.marker_enabled then capture:draw_marker(canvas) end
    end,
}
