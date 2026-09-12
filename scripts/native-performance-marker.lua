-- Offline diagnostic encoder, not an installed fixture or runtime observer.
-- Wire/pixel contract: docs/research/native-performance-marker-format.md.
local marker = {}

local function crc32(bytes)
    local crc = 0xffffffff
    for i = 1, #bytes do
        crc = crc ~ bytes:byte(i)
        for _ = 1, 8 do
            crc = (crc >> 1) ~ ((crc & 1) == 1 and 0xedb88320 or 0)
        end
    end
    return crc ~ 0xffffffff
end

local function validate_identity(identity)
    assert(type(identity) == "table" and getmetatable(identity) == nil, "expected plain identity table")
    for key in pairs(identity) do
        assert(key == "run_id" or key == "generation" or key == "frame_id" or key == "input_id",
               "unexpected identity field")
    end
    assert(math.type(identity.frame_id) == "integer" and identity.frame_id > 0,
           "frame_id must be a positive signed-64-bit integer")
    for _, field in ipairs({ "run_id", "generation", "input_id" }) do
        local value = identity[field]
        if field ~= "input_id" or value ~= nil then
            assert(type(value) == "string" and #value > 0 and #value <= 128
                   and value:match("^[A-Za-z0-9_.%-]+$"), "invalid " .. field)
        end
    end
end

function marker.draw(canvas, identity)
    validate_identity(identity)
    local run, generation, token = identity.run_id, identity.generation, identity.input_id or ""
    local header = string.pack(
        ">c8I8BBBBc128c128c128", "SLVMRK00", identity.frame_id,
        #run, #generation, #token, 0, run, generation, token
    )
    local packet = header .. string.pack(">I4", crc32(header))
    local rows = {}
    local black, white = string.char(0, 0, 0, 255):rep(2), string.char(255, 255, 255, 255):rep(2)
    for row = 0, 3 do
        local cells = {}
        for byte = row * 102 + 1, (row + 1) * 102 do
            local value = packet:byte(byte)
            for bit = 7, 0, -1 do
                cells[#cells + 1] = ((value >> bit) & 1) == 1 and white or black
            end
        end
        local scanline = table.concat(cells)
        rows[#rows + 1] = scanline
        rows[#rows + 1] = scanline
    end
    -- The fixture must have restored the identity transform and unrestricted
    -- clip before this final draw. No public canvas reset operation is added.
    canvas:save()
    canvas:alpha(1)
    canvas:operator("source")
    canvas:raw_pixels(
        table.concat(rows), "bgra8", 1632, 8, 6528,
        { x = 0, y = 0, width = 1632, height = 8 },
        { x = 188, y = 4, width = 1632, height = 8 }, "nearest"
    )
    canvas:restore()
end

return marker
