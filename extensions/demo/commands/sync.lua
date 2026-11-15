local flux = require("flux")

function main()
    local n = (flux.storage.get("runs") or 0) + 1
    flux.storage.set("runs", n)
    flux.logger.info("sync ran", n, "time(s)")
end
