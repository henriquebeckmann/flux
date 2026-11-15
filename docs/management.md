# Managing extensions

* **Unique id**: the manifest `name`. If two folders declare the same id the first wins and the second is reported as invalid.
* **Status** per extension: `Running`, `Disabled`, `Invalid` (with every error) or `Restarted` (crashed or unresponsive and replaced).
  An invalid extension never prevents the others from loading.
* **Where**: *development* folders (`./extensions`, `~/.config/flux/extensions`, `flux dev` attachments) and *installed* ones
  (`~/.local/share/flux/installed/<id>`). Logs: `~/.local/state/flux/logs/<id>.log` (1 MB rotation); the last 1000 lines and the last errors are
  kept in memory.
* **Manage Extensions** (launcher command): version, source and status per extension; actions *Show Details* (version, author, licence,
  permissions, allowed hosts, commands), *Enable/Disable*, *Reload*, *Show Logs*, *Show Errors*, *Remove* (confirmed; installed extensions only),
  *Open Folder*, *Copy Path*. The list updates in place as the runtime reports changes.
* **Reload** is per extension (its `on_unload` hooks run, a new thread and Lua state start); editing a file in `--dev` reloads only that extension.
* **Install** validates first, stages in a temporary folder and renames, so a failure never leaves a half-installed extension.
  Archives are checked against zip-slip, symlinks, more than 5 000 entries / 10 MB per file / 50 MB total (measured on what is *read*, not declared).
* Limitations: secrets stored in the keyring by an extension are not enumerable, so `--purge` cannot delete them. A view open while its
  extension reloads is not closed automatically (press Esc).
