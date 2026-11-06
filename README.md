# Set Creator

An offline Windows app for building GameShow question sets. It uses the same editor as the
GameShow web app, but stores everything on this computer. There are no accounts, no server and
no game mode.

## For the person making a set

1. Run `SetCreator.exe`. There's nothing to install. It needs Windows 10 or 11, which already
   include WebView2.
2. **Utwórz zestaw** (Create set), then add games and questions. Pictures, sounds and videos up to
   50 MB each are supported. Changes save automatically.
3. Click **Eksportuj .zip** (Export .zip) on the set and send the `.gameshow.zip` file to the host.

Your sets live in `%APPDATA%\com.gameshow.setcreator\` (`sets\` and `media\`). **Importuj plik**
(Import file) opens a `.gameshow.zip`, or a `.json` exported from GameShow, as a new set.

## For the host (GameShow running)

On **Twoje zestawy pytań** (Your question sets) click **Importuj plik** (Import file) and pick the
`.gameshow.zip`. The browser unpacks it with hard limits:

- at most 300 entries
- set.json at most 2 MB
- each file at most 50 MB
- at most 1 GB in total

It uploads each media file through the normal `/api/media` upload, which checks the format and
size on the server. Then it creates the set through `/api/sets/import`. The server never handles
the zip itself.

## Zip format

```
set.json       GameShow set file: { "format": "gameshow-set", "version": 2, title, mode, ... }
manifest.json  { "media": [{ id, kind, name, contentType, size }] }
media/<id>     the file bytes (id = UUID)
```

## Building (on your PC only)

You need [Rust](https://rustup.rs) (MSVC), [Bun](https://bun.sh) and the GameShow repo at
`D:\GameShow`. If the repo is somewhere else, set `GAMESHOW_DIR`. Then run:

```powershell
.\build.ps1
```

The script does three things:

1. Builds the editor UI from GameShow (`bun run build:desktop`, entry `frontend/src/desktop`)
   into `ui/`.
2. Compiles the Tauri app.
3. Copies the result to `SetCreator.exe`, a single portable file of about 5 MB.

To run the tests, use `cargo test`. They cover the zip round trip, path traversal, zip bombs,
oversized and fake media files, and entry limits.

### Layout

- `src/main.rs`: commands (`list_sets`, `get_set`, `save_set`, `delete_set`, `add_media`,
  `export_zip`, `import_file`) and the `media:` protocol that serves the editor's previews.
- `tauri.conf.json`: the window, which loads `ui/desktop.html`, plus the CSP.
- The UI code lives in GameShow:
  - `frontend/src/desktop/` holds the local set list and the Tauri-backed `SetStore`.
  - `frontend/src/editor/SetStore.ts` is the storage switch that lets the web editor run on
    local files.
