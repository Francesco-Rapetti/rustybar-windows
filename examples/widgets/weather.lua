-- The weather from Open-Meteo (free, no key): the temperature in the bar, every 15 minutes; the
-- popup has the sky, the wind, the humidity and the next hours. One request every 15 minutes,
-- on a worker thread of the bar (rb.http): no process, nothing while no bar shows or a listed
-- game is in front.
--
--   [layout] right = ["weather", ...]
--   [widgets.weather]
--   latitude = 41.89
--   longitude = 12.49
--   place = "Roma"
local rb = require("rustybar")

-- The weather codes of WMO, as Open-Meteo gives them
local skies = {
  [0] = { "Sereno", "Clear" }, [1] = { "Poco nuvoloso", "Mostly clear" }, [2] = { "Nuvoloso", "Partly cloudy" },
  [3] = { "Coperto", "Overcast" }, [45] = { "Nebbia", "Fog" }, [48] = { "Nebbia", "Fog" },
  [51] = { "Pioviggine", "Drizzle" }, [53] = { "Pioviggine", "Drizzle" }, [55] = { "Pioviggine", "Drizzle" },
  [61] = { "Pioggia", "Rain" }, [63] = { "Pioggia", "Rain" }, [65] = { "Pioggia forte", "Heavy rain" },
  [71] = { "Neve", "Snow" }, [73] = { "Neve", "Snow" }, [75] = { "Neve forte", "Heavy snow" },
  [80] = { "Rovesci", "Showers" }, [81] = { "Rovesci", "Showers" }, [82] = { "Rovesci forti", "Heavy showers" },
  [95] = { "Temporale", "Thunderstorm" }, [96] = { "Temporale", "Thunderstorm" }, [99] = { "Temporale", "Thunderstorm" },
}

local function sky(code)
  local s = skies[code]
  return s and rb.t(s[1], s[2]) or ""
end

local function degrees(t)
  return string.format("%d°", math.floor(t + 0.5))
end

rb.widget {
  name = "weather",
  every = 900,

  update = function(self)
    local o = self.options
    local url = string.format(
      "https://api.open-meteo.com/v1/forecast?latitude=%s&longitude=%s"
        .. "&current=temperature_2m,weather_code,wind_speed_10m,relative_humidity_2m"
        .. "&hourly=temperature_2m,weather_code&forecast_hours=6&timezone=auto",
      o.latitude or 41.89, o.longitude or 12.49)
    rb.http(url, function(body, status, error)
      local data = status == 200 and rb.json(body)
      if not data then
        self.failed = error or ("HTTP " .. status)
        return
      end
      self.failed = nil
      self.now = data.current
      self.hours = data.hourly
    end)
  end,

  bar = function(self, ctx)
    local now = self.now
    if not now then
      return rb.ui.pill { rb.ui.icon { "E9CA", color = "subtext" }, rb.ui.text { "–", color = "subtext" } }
    end
    local color = self.failed and "subtext" or "text"
    return rb.ui.pill { rb.ui.icon { "E9CA", color = color }, rb.ui.text { degrees(now.temperature_2m), color = color } }
  end,

  popup = function(self)
    local now, hours = self.now, self.hours
    local place = self.options.place or rb.t("Meteo", "Weather")
    if not now then
      return rb.ui.column { rb.ui.section { place }, rb.ui.text { self.failed or rb.t("Lettura…", "Reading…"), small = true, wrap = true } }
    end
    local next_hours = {}
    for i = 1, math.min(#hours.time, 6) do
      local hour = hours.time[i]:sub(12, 16)
      next_hours[#next_hours + 1] = rb.ui.detail { hour .. "  " .. sky(hours.weather_code[i]), degrees(hours.temperature_2m[i]) }
    end
    local column = {
      rb.ui.section { place, right = self.failed and rb.t("non aggiornato", "not up to date") },
      rb.ui.tiles {
        rb.ui.tile { rb.ui.text { rb.t("Ora", "Now"), small = true }, rb.ui.text { degrees(now.temperature_2m), size = 24, weight = "semibold" },
                     rb.ui.text { sky(now.weather_code), small = true } },
        rb.ui.tile { rb.ui.detail { rb.t("Vento", "Wind"), string.format("%d km/h", math.floor(now.wind_speed_10m + 0.5)) },
                     rb.ui.detail { rb.t("Umidità", "Humidity"), now.relative_humidity_2m .. "%" } },
      },
      rb.ui.section { rb.t("Prossime ore", "Next hours") },
    }
    for _, row in ipairs(next_hours) do column[#column + 1] = row end
    column[#column + 1] = rb.ui.button { "Open-Meteo", on_click = "https://open-meteo.com" }
    return rb.ui.column(column)
  end,
}
