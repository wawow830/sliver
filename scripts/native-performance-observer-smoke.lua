-- Uninstalled bounded software-seam fixture. NOT the P1 native workload:
-- no timer, epoch, warmup, physical source authentication, or acceptance claim.
-- Tests copy the existing marker encoder alongside this as native_marker.lua.
local sliver = require("sliver.v1")
local marker = require("native_marker")
local frame = 0
local token = 0
return {
    api_version = 1,
    touch = function(event)
        if event.phase == "down" then
            assert(token < 16, "smoke fixture input bound exhausted")
            token = token + 1
            sliver.redraw()
        end
    end,
    render = function(canvas)
        assert(frame < 32, "smoke fixture render bound exhausted")
        frame = frame + 1
        canvas:rectangle(0, 0, 2008, 60, token == 0 and "#112233" or "#335577")
        marker.draw(canvas, {
            run_id = "software-smoke", generation = "g", frame_id = frame,
            input_id = token > 0 and tostring(token) or nil,
        })
    end,
}
