# API versioning

`api_version` in the manifest is the **major** version of the extension API the extension was written for (`1` or `"1"`).
Flux lists the majors it can run (`SUPPORTED_API_VERSIONS`, currently `[1]`). A manifest that omits it is assumed to be 1 and gets a warning.

* **Incompatible:** `this extension targets API 2, but Flux 0.1.0 supports API 1. Update Flux, or use an older version of the extension.`
  (or "Update the extension to a supported API" for retired majors). `min_flux_version = "x.y.z"` expresses a minimum Flux.
* **Within a major:** only additive changes (new functions, new optional fields). Behaviour is never changed silently.
* **Deprecation:** a name is marked deprecated (table in `deprecations.rs`) with its replacement and the first API version that may drop it.
  Using it logs one warning per extension per run and `flux validate` reports it. A deprecated name keeps working for at least the whole
  remaining life of its major version.
* **Retiring a major:** it stays in `SUPPORTED_API_VERSIONS` for at least one Flux release after its successor ships.
* **Breaking changes** are listed in [api-changelog.md](api-changelog.md) with the migration.
