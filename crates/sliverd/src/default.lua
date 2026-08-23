local sliver = require("sliver.v1")

local WIDTH = 2008
local HEIGHT = 60
local WHITE = "#ffffff"
local BLACK = "#000000"
local PRESSED = "#383838"
local GREEN = "#34c759"
local AMBER = "#ffbf00"
local RED = "#ff3b30"
local BATTERY_ROOT = "/sys/class/power_supply/macsmc-battery"

local clock_control = { name = "clock", label = "--:--" }
local battery_control = { name = "battery", label = "--%" }

local normal = {
    { name = "escape", label = "Esc", key = sliver.input.keys.keyboard.escape },
    { name = "brightness_down", label = "", key = sliver.input.keys.consumer.brightness_down },
    { name = "brightness_up", label = "", key = sliver.input.keys.consumer.brightness_up },
    { name = "previous", label = "", key = sliver.input.keys.consumer.previous },
    { name = "play_pause", label = "", key = sliver.input.keys.consumer.play_pause },
    { name = "next", label = "", key = sliver.input.keys.consumer.next },
    clock_control,
    battery_control,
    { name = "mute", label = "", key = sliver.input.keys.consumer.mute },
    { name = "volume_down", label = "", key = sliver.input.keys.consumer.volume_down },
    { name = "volume_up", label = "", key = sliver.input.keys.consumer.volume_up },
}

local function_key_names = {
    "f1", "f2", "f3", "f4", "f5", "f6",
    "f7", "f8", "f9", "f10", "f11", "f12",
}
local function_layer = {}
for index, name in ipairs(function_key_names) do
    function_layer[index] = {
        name = name,
        label = "F" .. tostring(index),
        key = sliver.input.keys.keyboard[name],
    }
end

local contacts = {}
local pressed = {}
local clock_text = os.date("%H:%M") or "--:--"
local battery_capacity
local battery_charging = false

local function read_file(path)
    local file = io.open(path, "r")
    if not file then
        return nil
    end
    local value = file:read("*a")
    file:close()
    return value
end

local function parse_capacity(value)
    local digits = value and value:match("^(%d+)\n$")
    if not digits then
        return nil
    end
    local capacity = tonumber(digits)
    if not capacity or capacity < 0 or capacity > 100 then
        return nil
    end
    return capacity
end

local function parse_status(value)
    local status = value and value:match("^(.-)\n$")
    if status == "Unknown" or status == "Charging" or status == "Discharging"
        or status == "Not charging" or status == "Full" then
        return status
    end
    return nil
end

local function refresh_battery()
    battery_capacity = parse_capacity(read_file(BATTERY_ROOT .. "/capacity"))
    local status = parse_status(read_file(BATTERY_ROOT .. "/status"))
    battery_charging = status == "Charging"
    battery_control.label = battery_capacity and (tostring(battery_capacity) .. "%") or "--%"
end

local function battery_color()
    if not battery_capacity then
        return WHITE
    end
    if battery_charging then
        return GREEN
    end
    if battery_capacity <= 15 then
        return RED
    end
    if battery_capacity <= 30 then
        return AMBER
    end
    return WHITE
end

local function set_pressed(index, active)
    local count = pressed[index] or 0
    if active then
        count = count + 1
    else
        count = count - 1
    end
    if count > 0 then
        pressed[index] = count
    else
        pressed[index] = nil
    end
end

local function current_row()
    return sliver.input.state().fn and function_layer or normal
end

local function key_for(row, index)
    local control = row[index]
    return control and control.key
end

local function hit(row, x, y)
    if y < 0 or y >= HEIGHT or x < 0 or x >= WIDTH then
        return nil
    end
    local index = math.floor(x / (WIDTH / #row)) + 1
    return row[index] and index or nil
end

local function activate(contact)
    local key = key_for(contact.row, contact.control)
    if key then
        sliver.input.key.tap(key)
    end
end

local function touch(event)
    if event.phase == "down" then
        local row = current_row()
        local control = hit(row, event.x, event.y)
        contacts[event.id] = {
            row = row,
            control = control,
            inside = control ~= nil,
        }
        if control then
            set_pressed(control, true)
            sliver.redraw()
        end
        return
    end

    local contact = contacts[event.id]
    if not contact then
        return
    end

    if event.phase == "move" then
        if contact.inside and hit(contact.row, event.x, event.y) ~= contact.control then
            contact.inside = false
            set_pressed(contact.control, false)
            sliver.redraw()
        end
        return
    end

    contacts[event.id] = nil
    if contact.control then
        local activate_on_up = event.phase == "up"
            and contact.inside
            and hit(contact.row, event.x, event.y) == contact.control
        if contact.inside then
            set_pressed(contact.control, false)
        end
        if activate_on_up then
            activate(contact)
        end
        sliver.redraw()
    end
end

local function draw_path(canvas, commands, color, width)
    local path = sliver.path(commands)
    if width then
        canvas:stroke(path, width, color)
    else
        canvas:fill(path, color)
    end
end

local function draw_sun(canvas, center_x, center_y, sign)
    local rays = {
        { "move_to", center_x - 11, center_y, }, { "line_to", center_x - 6, center_y, },
        { "move_to", center_x + 6, center_y, }, { "line_to", center_x + 11, center_y, },
        { "move_to", center_x, center_y - 11, }, { "line_to", center_x, center_y - 6, },
        { "move_to", center_x, center_y + 6, }, { "line_to", center_x, center_y + 11, },
    }
    draw_path(canvas, rays, WHITE, 2)
    canvas:rectangle(center_x - 5, center_y - 5, 10, 10, WHITE)
    if sign < 0 then
        canvas:rectangle(center_x - 16, center_y + 13, 8, 2, WHITE)
    else
        canvas:rectangle(center_x + 8, center_y + 13, 8, 2, WHITE)
    end
end

local function draw_previous(canvas, center_x, center_y)
    draw_path(canvas, {
        { "move_to", center_x + 9, center_y - 10 },
        { "line_to", center_x - 3, center_y },
        { "line_to", center_x + 9, center_y + 10 },
        { "close" },
    }, WHITE)
    canvas:rectangle(center_x - 11, center_y - 10, 3, 20, WHITE)
end

local function draw_next(canvas, center_x, center_y)
    draw_path(canvas, {
        { "move_to", center_x - 9, center_y - 10 },
        { "line_to", center_x + 3, center_y },
        { "line_to", center_x - 9, center_y + 10 },
        { "close" },
    }, WHITE)
    canvas:rectangle(center_x + 8, center_y - 10, 3, 20, WHITE)
end

local function draw_play_pause(canvas, center_x, center_y)
    draw_path(canvas, {
        { "move_to", center_x - 10, center_y - 11 },
        { "line_to", center_x + 3, center_y },
        { "line_to", center_x - 10, center_y + 11 },
        { "close" },
    }, WHITE)
    canvas:rectangle(center_x + 6, center_y - 10, 3, 20, WHITE)
    canvas:rectangle(center_x + 12, center_y - 10, 3, 20, WHITE)
end

local function draw_speaker(canvas, center_x, center_y, direction)
    draw_path(canvas, {
        { "move_to", center_x - 14, center_y - 5 },
        { "line_to", center_x - 7, center_y - 5 },
        { "line_to", center_x + 2, center_y - 12 },
        { "line_to", center_x + 2, center_y + 12 },
        { "line_to", center_x - 7, center_y + 5 },
        { "line_to", center_x - 14, center_y + 5 },
        { "close" },
    }, WHITE)
    if direction < 0 then
        draw_path(canvas, {
            { "move_to", center_x + 8, center_y - 7 },
            { "line_to", center_x + 15, center_y + 7 },
        }, WHITE, 2)
        draw_path(canvas, {
            { "move_to", center_x + 15, center_y - 10 },
            { "line_to", center_x + 23, center_y + 10 },
        }, WHITE, 2)
    else
        draw_path(canvas, {
            { "move_to", center_x + 8, center_y - 9 },
            { "line_to", center_x + 15, center_y + 9 },
        }, WHITE, 2)
    end
end

local function draw_mute(canvas, center_x, center_y)
    draw_speaker(canvas, center_x - 3, center_y, 0)
    draw_path(canvas, {
        { "move_to", center_x + 10, center_y - 11 },
        { "line_to", center_x + 25, center_y + 11 },
        { "move_to", center_x + 25, center_y - 11 },
        { "line_to", center_x + 10, center_y + 11 },
    }, WHITE, 2)
end

local function draw_battery(canvas, center_x, center_y)
    draw_path(canvas, {
        { "move_to", center_x - 14, center_y - 9 },
        { "line_to", center_x + 13, center_y - 9 },
        { "line_to", center_x + 13, center_y + 9 },
        { "line_to", center_x - 14, center_y + 9 },
        { "close" },
    }, WHITE, 2)
    canvas:rectangle(center_x + 14, center_y - 4, 3, 8, WHITE)
end

local function draw_control(canvas, control, index, left, width)
    if pressed[index] then
        canvas:rectangle(left + 3, 3, width - 6, HEIGHT - 6, PRESSED)
    end

    local center_x = left + width / 2
    local center_y = 24
    if control.name == "brightness_down" then
        draw_sun(canvas, center_x, center_y, -1)
    elseif control.name == "brightness_up" then
        draw_sun(canvas, center_x, center_y, 1)
    elseif control.name == "previous" then
        draw_previous(canvas, center_x, center_y)
    elseif control.name == "play_pause" then
        draw_play_pause(canvas, center_x, center_y)
    elseif control.name == "next" then
        draw_next(canvas, center_x, center_y)
    elseif control.name == "battery" then
        draw_battery(canvas, center_x - 35, center_y - 5)
    elseif control.name == "mute" then
        draw_mute(canvas, center_x - 15, center_y - 4)
    elseif control.name == "volume_down" then
        draw_speaker(canvas, center_x - 5, center_y - 4, -1)
    elseif control.name == "volume_up" then
        draw_speaker(canvas, center_x - 5, center_y - 4, 1)
    end

    local label = control.label
    if control.name == "battery" then
        canvas:text(left + 30, 42, label, 14, battery_color())
    elseif label ~= "" then
        local text_width = canvas:measure_text(label, 18)
        canvas:text(center_x - text_width / 2, 18, label, 18, WHITE)
    end
end

local function render(canvas)
    canvas:rectangle(0, 0, WIDTH, HEIGHT, BLACK)
    local row = current_row()
    local width = WIDTH / #row
    for index, control in ipairs(row) do
        local left = (index - 1) * width
        draw_control(canvas, control, index, left, width)
    end
end

refresh_battery()
sliver.timer.every(30, function()
    refresh_battery()
    sliver.redraw()
end)
local function schedule_clock()
    local seconds = tonumber(os.date("%S")) or 0
    sliver.timer.after(math.max(0.01, 60 - seconds), function()
        clock_text = os.date("%H:%M") or "--:--"
        clock_control.label = clock_text
        sliver.redraw()
        schedule_clock()
    end)
end
schedule_clock()
clock_control.label = clock_text

return {
    api_version = 1,
    start = function()
        sliver.backlight.set(0.75)
    end,
    key = function(event)
        if event.key == "fn" then
            sliver.redraw()
        end
    end,
    touch = touch,
    render = render,
}
