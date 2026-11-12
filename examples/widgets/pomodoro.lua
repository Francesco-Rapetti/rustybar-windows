-- A pomodoro timer: the hourglass in the bar, with the time left while it runs; the popup starts,
-- pauses and resets it, and sets how long it lasts. Its timer ticks only while it runs
-- (`self.every = false` otherwise), so at rest it costs nothing. When the time is up it sends the
-- trigger `pomodoro_done`, for another widget (or `on = { "pomodoro_done" }`) to hear.
--
--   [layout] right = ["pomodoro", ...]
--   [widgets.pomodoro]
--   minutes = 25
local rb = require("rustybar")

local function clock(seconds)
  return string.format("%d:%02d", seconds // 60, seconds % 60)
end

local function stop(self)
  self.running = false
  self.every = false
end

rb.widget {
  name = "pomodoro",
  every = false,

  init = function(self)
    self.minutes = math.floor(self.options.minutes or 25)
    self.left = self.minutes * 60
    stop(self)
  end,

  update = function(self)
    if not self.running then return end
    self.left = self.left - 1
    if self.left <= 0 then
      stop(self)
      self.left = self.minutes * 60
      self.done = true
      rb.trigger("pomodoro_done", { minutes = self.minutes })
    end
  end,

  bar = function(self, ctx)
    local color = self.running and "red" or (self.done and "green" or "text")
    local hourglass = rb.ui.icon { "E916", color = color }
    if not self.running or ctx.compact then
      return rb.ui.pill { hourglass }
    end
    return rb.ui.pill { hourglass, rb.ui.text { clock(self.left), fixed = "00:00" } }
  end,

  popup = function(self)
    local total = self.minutes * 60
    return rb.ui.column {
      rb.ui.section { "Pomodoro", right = self.running and rb.t("in corso", "running") or rb.t("fermo", "stopped") },
      rb.ui.tile {
        rb.ui.text { clock(self.left), size = 28, weight = "semibold" },
        rb.ui.progress { value = 1 - self.left / total, color = "red" },
      },
      rb.ui.detail { rb.t("Durata", "Length"), self.minutes .. " min" },
      rb.ui.slider {
        value = (self.minutes - 5) / 55,
        on_change = function(v)
          self.minutes = math.floor(5 + v * 55 + 0.5)
          if not self.running then self.left = self.minutes * 60 end
        end,
      },
      rb.ui.divider(),
      rb.ui.button {
        self.running and rb.t("Pausa", "Pause") or rb.t("Avvia", "Start"),
        icon = self.running and "E769" or "E768",
        on_click = function()
          self.running = not self.running
          self.every = self.running and 1 or false
          self.done = false
        end,
      },
      rb.ui.button {
        rb.t("Azzera", "Reset"),
        icon = "E72C",
        on_click = function()
          stop(self)
          self.left = self.minutes * 60
          self.done = false
        end,
      },
    }
  end,
}
