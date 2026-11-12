-- The lights and plugs you pick, from Home Assistant through the `home` widget (`rb.home`): a
-- bulb in the bar with how many are on, and a popup with a switch for each. It asks nothing of
-- its own: the `home` widget follows the entities (list them in its `watch` if they aren't tiles)
-- and says when one changes, with the event `home_assistant`, so this widget has no timer.
--
--   [layout] right = ["lights", ...]
--   [widgets.lights]
--   entities = ["light.living_room", "switch.bedroom_lamp"]
--   [widgets.home]
--   watch = ["switch.bedroom_lamp"]     # if it isn't a tile already
local rb = require("rustybar")

local function entities(self)
  return self.options.entities or {}
end

local function on_count(self)
  local n = 0
  for _, id in ipairs(entities(self)) do
    local e = rb.home.state(id)
    if e and e.state == "on" then n = n + 1 end
  end
  return n
end

rb.widget {
  name = "lights",
  on = { "home_assistant" },

  -- Something of Home Assistant changed: the bar draws again after the call
  event = function(self, name, args) end,

  bar = function(self, ctx)
    local on = on_count(self)
    local color = on > 0 and "yellow" or "subtext"
    if not rb.home.online() then color = "overlay0" end
    return rb.ui.pill { rb.ui.icon { "EA80", color = color }, rb.ui.text { tostring(on) }, on_click = "popup" }
  end,

  popup = function(self, ctx)
    local rows = { rb.ui.section { rb.t("Luci", "Lights"), right = rb.home.online() and "" or rb.t("Non raggiungibile", "Unreachable") } }
    for _, id in ipairs(entities(self)) do
      local e = rb.home.state(id)
      local domain = id:match("^([^.]+)")
      rows[#rows + 1] = rb.ui.toggle {
        e and e.name or id,
        on = e ~= nil and e.state == "on",
        on_change = function(on)
          rb.home.call(domain, on and "turn_on" or "turn_off", { entity_id = id })
        end,
      }
    end
    rows[#rows + 1] = rb.ui.disclosure {
      rb.t("Dettagli", "Details"),
      open = self.details,
      on_click = function() self.details = not self.details end,
      rb.ui.detail { rb.t("Accese", "On"), tostring(on_count(self)) },
      rb.ui.detail { rb.t("In tutto", "In all"), tostring(#entities(self)) },
    }
    return rb.ui.column(rows)
  end,
}
