# XCL Launcher

A small, fast game library and launcher for [XCL](https://github.com/xcl-dev/xcl), the
Xbox Series X|S → Windows compatibility layer.

- **Square tiles.** Each game shows its `xbs_logo_480.png`, with its name from `MicrosoftGame.config` underneath.
- **Library folders.** Every game folder inside them is listed on its own. You can also add single games.
- **Settings for every game, with per-game overrides.** Window size and position, network on/off,
  intro movies, frame-rate limit, console model and more.
- **No console window.** XCL runs hidden, and XCL's own logs are written as before.
- **One-click folders** for saves, logs, cache, `xcl.toml`, the shader cache and more.
- **Cache cleaning** in a dialog: pick what to delete, see how big it is, then confirm.
- **Shader precompiling** per game, the same as `precompile-shaders-xcl-*.ps1`.
- Runs on **Windows** and **Linux** (there XCL runs under Wine or Proton).

> **Tested with XCL's `nvidia-dev` branch.** The progress bars read the lines that branch writes
> to `xcl.log` and prints from `xcl shaders --precompile`. Other XCL versions start games just
> the same, but their shader progress may not show.

## Getting started

1. **Build XCL** if you haven't yet (see XCL's README):

   ```powershell
   cargo build --release --workspace     # in the xcl repository
   ```

2. **Build the launcher** (Rust 1.88 or newer):

   ```powershell
   cargo build --release
   ```

   The program is `target\release\xcl-launcher.exe` (`target/release/xcl-launcher` on Linux).
   It's a single file with nothing to install, so you can copy it wherever you like.

3. **Start it, open Settings and set**
   - **xcl.exe**: usually `xcl\target\release\xcl.exe` in the XCL repository. If `xcl.exe` is
     next to the launcher, it's picked up automatically.
   - **Library folders**: a folder that holds your unpacked game folders, for example `D:\Games`.

4. **Double-click a tile** to play, or click it once to open the game's panel.

## The library

A *game folder* is a folder with a `MicrosoftGame.config`: the unpacked game package that you'd
give `xcl run`.

- **Library folders**: game folders directly inside a library folder, or one level deeper
  (`Publisher\Game`), are listed automatically. A library folder can also be a game folder itself.
  Click **Refresh** after adding or removing games on disk.
- **Add game…**: adds a single game folder from anywhere. Games added this way can be removed
  from the list again with right-click → *Remove from list*. Nothing on disk is deleted.

The tile image is the manifest's `Square480x480Logo`, falling back to `xbs_logo_480.png`, then
`Square150x150Logo`. The name is the manifest's `DefaultDisplayName`, or the folder name if it has
none. A game without a logo gets a tile in its manifest's background colour.

Logos are decoded in the background and scaled down to tile size, and only visible tiles are drawn.
The window only redraws when something changes, so the launcher stays light with a large library.

## Playing

| Action | How |
|---|---|
| Play | Double-click a tile, right-click → **Play**, or **Play** in the game's panel |
| Stop | **Stop** in the panel or the tile's right-click menu. It ends XCL and the game process it started |
| Game details | Click a tile. **Esc** closes the panel |
| Search | The search box filters by name |
| Copy a game's name or folder | Right-click the tile → **Copy name** / **Copy folder path** |

**Copy and paste** work everywhere: every text field takes Ctrl+V / Ctrl+C / Ctrl+X and has a
right-click menu with Cut, Copy and Paste. Any other text in the launcher (paths, error messages,
settings) can be selected with the mouse and copied with Ctrl+C.

XCL is started as `xcl run "<game folder>" [-- <game arguments>]` **without a console window**.
What XCL prints to its console is saved to a file instead (**Console output** in the game's
panel). XCL's own `xcl.log` and `report.json` are written exactly as before, in the game's XCL
data folder (**Logs**).

If XCL exits with an error, the bottom bar shows the reason (the `CRASH:` or `xcl:` line) with a
**Show output** button.

### Shader preparation before a game starts

Before the game's own code starts, XCL prepares its shaders in the game window. The launcher reads
the game's `xcl.log` as it grows and shows the same progress under **Stop** in the game's panel
and in the bottom bar. The tile shows **Preparing shaders** meanwhile. There are three steps:

1. **Translating shaders**: the console shaders are translated (records of the game's `shaders.rpf`).
2. **Compute pipelines**: worker processes check every record and build its compute pipelines.
3. **Graphics pipelines**: the graphics pipelines earlier runs used are built again from their
   saved state.

The bar shows the current step's count. XCL logs it every 5 seconds, so the bar moves in steps.
After the first run, XCL only prepares the shaders earlier runs used, which is much faster.
The count can stand still for minutes while the biggest pipelines are built: after 15 seconds
without progress the panel says so, and the spinner shows XCL is still at work. Closing the game
window cancels the launch.

XCL's switches for this can be set as [environment variables](#game-settings-and-overrides):
`XCL_PREPARE=0` leaves preparation out, `XCL_PREPARE_ALL=1` prepares every shader, not only the ones
used before, and `XCL_PREPARE_GRAPHICS=0` leaves out the graphics pipelines.

### Shader compiling while a game runs

XCL still builds some pipelines while a game runs. Once preparation is done, the panel and
bottom bar show how long it took and that work:

- **Warm-up bar**: at startup, XCL builds the pipelines an earlier run used ("at least 512 of 787").
  XCL only logs this count at 1, 2, 4, 8 …, so the bar is a lower bound and usually stops short
  of the end, even when the warm-up has finished.
- **Pipelines built** this run (compute pipelines, plus graphics pipelines large enough for XCL to log).
- **Large ones compiling now**: pipelines in the GPU driver's compiler. While there are any, the
  tile shows **Compiling shaders** instead of **Running**.

There's no overall percentage while playing: pipelines are built the moment the game first needs
them, so there's no total to measure against.

## Settings

**Settings** (top right) holds:

| Setting | Meaning |
|---|---|
| xcl.exe | The XCL program used to start games |
| XCL data folder | Passed to XCL as `XCL_DATA_ROOT`. Leave it empty for XCL's default, `%LOCALAPPDATA%\XCL` |
| Runner, runner arguments, Wine prefix | Linux only, see [Linux](#linux) |
| Library folders | See [The library](#the-library) |
| Tile size | 96–320 points |
| Default game settings | Used by every game unless the game overrides them (see below) |

### Game settings and overrides

Every setting has a tick box:

- In **Default game settings**, an unticked setting leaves XCL's own default in place. A ticked
  one is applied to every game.
- In a **game's panel**, an unticked setting uses the default. A ticked one overrides it for that
  game only. **Reset to defaults** removes all of a game's overrides.

| Setting | What XCL gets |
|---|---|
| Window size | `window_width` / `window_height` in the game's `xcl.toml` |
| Window position | `window_pos` in `xcl.toml`: chosen by Windows, centred, or at x, y |
| Gamertag | `gamertag` in `xcl.toml` |
| Keyboard and mouse | `keyboard_mouse` in `xcl.toml` |
| Mouse sensitivity | `mouse_sensitivity` in `xcl.toml` |
| Network | Off sets `XCL_NETWORK=0`: the game runs as on a console without a network |
| Movies | `XCL_SKIP_BINK`: play all, skip intro movies, or skip all movies |
| Frame rate limit | `XCL_FRAME_INTERVAL_US`: uncapped or a maximum fps |
| Console | `XCL_DEVICE_TYPE`: Xbox Series X or Series S |
| Ray tracing | `XCL_RAYTRACING`: the GPU's own tier, 1.1, 1.0 or off |
| GPU | `XCL_ADAPTER`: `auto`, the adapter's number, or part of its name (`radeon`) |
| Mute speech | `XCL_MUTE_TTS` |
| PlayStation controllers | Off sets `XCL_PLAYSTATION_PAD=0` |
| Game arguments | Passed to the game after `--`. Quote arguments with spaces: `-a "b c"` |
| Environment variables | Any other `XCL_*` switch from XCL's README (`XCL_ACTIVITY`, `XCL_RUN_SECONDS` …). A game's variables are added to the defaults, and replace defaults with the same name |

The `xcl.toml` settings are written into `<XCL data>\<title>\xcl.toml` just before the game
starts. Only ticked settings are written. The rest of the file, including your own edits and
comments, is left as it is, and the file is only rewritten when a value actually changes.
Unticking a setting doesn't put the old value back: the file keeps what was last written.

**Fullscreen:** XCL has no fullscreen mode yet. Set the window size to your screen's resolution
and the position to *centred* or `0, 0`.

The launcher's own settings are saved automatically to:

- Windows: `%APPDATA%\xcl-launcher\config\config.toml`
- Linux: `~/.config/xcl-launcher/config.toml`

To keep the launcher's settings somewhere else (for example a portable setup on a USB drive),
set `XCL_LAUNCHER_CONFIG` to a file path. Its logs then go to a `logs` folder next to that file.

## Folders

The game's panel has buttons for:

| Button | Opens |
|---|---|
| Game | The game folder (XCL only ever reads it) |
| Saves | The game's saves (`<XCL data>\<title>\gamesave`, or `saves`) |
| Local storage | The game's persistent local storage, its `S:` drive (`pls`) |
| XCL data | Everything XCL keeps for the game |
| Logs | `xcl.log`, `report.json` and the previous run's copies |
| Cache | XCL's runtime cache for the game |
| xcl.toml | The game's XCL configuration file |
| Console output | What XCL printed during the last launch from the launcher |

**Folders** in the top bar opens the general ones: XCL's data folder (all games), the shader
cache, XCL's program folder, the launcher's logs and settings, and the Wine prefix on Linux.

A folder that doesn't exist yet is created by XCL the first time the game runs.

## Cleaning caches

**Clean caches…** measures everything that can safely be deleted and lists it with its size:

| Group | Items |
|---|---|
| Shader cache (all games) | Compiled shaders (DXIL), translated shaders, driver pipeline cache, list of used pipelines |
| Per-game caches | For every game XCL has data for: runtime cache, dumped shaders, temporary files (`T:`), the previous run's log |
| Other | Leftover `xcl-multi-exe-*`, `xcl-no-exe-*`, `xcl-self-copy-*` folders from XCL's tests, and the launcher's console logs |

Tick what you want gone, click **Clean selected…**, check the list and confirm with **Delete**.
Deleting is refused while XCL is running anywhere on the system, including games not started
from the launcher.

**Never deleted:** saves, persistent local storage, the overlay (the game's own writes),
`xcl.toml`, and the last run's `xcl.log` / `report.json`.

After cleaning the shader cache, the next run of each game recompiles its shaders and starts
slower. Use **Precompile shaders** to get that work done ahead of time. The *list of used
pipelines* is what **Only pipelines used before** depends on, so it's unticked unless you choose it.

## Precompiling shaders

**Precompile shaders** in a game's panel compiles the game's compute shaders and pipelines into
XCL's cache without starting the game. XCL does this work in its [shader
preparation](#shader-preparation-before-a-game-starts) on every launch anyway, so this is for
getting it done ahead of time, for example right after updating XCL or cleaning the shader cache.
It runs

```
xcl shaders <pack> --precompile --only cs --pipelines [--used-only]
```

for every `shaders.rpf` in the game folder, one after another, without a console window. The output
is saved to `<title>.precompile.log` in the launcher's logs.

A **progress bar** in the game's panel and in the bottom bar shows how many of the pack's shaders
are done (pipelines are built as part of each shader), plus the number of pipelines built, the
time so far and an estimate of the time left. XCL reports the count every 200 shaders, and the
biggest programs can take minutes each. When the count hasn't moved for 30 seconds, the estimate
gives way to "compiling large shaders", and the spinner shows XCL is still at work. When it
finishes, the bottom bar shows XCL's summary (compiled, already cached, errors, pipelines made).
It's resumable: shaders already in the cache are skipped. **Only pipelines used before** adds
`--used-only`, which is much faster once you've played the game for a while.

## Linux

XCL is a Windows program. On Linux the launcher runs it under Wine (or Proton):

```
<runner> <runner arguments> xcl.exe run Z:\path\to\game
```

- **Runner**: `wine` by default. It can also be a Proton `proton` script (with runner arguments
  `run`), `umu-run`, or any wrapper you like.
- **Wine prefix**: passed as `WINEPREFIX`. If empty, `$WINEPREFIX` is used, or else `~/.wine`.
- Paths are passed to XCL as Wine paths (`/home/me/Games/X` becomes `Z:\home\me\Games\X`).
- XCL's data folder and shader cache are found inside the prefix
  (`drive_c/users/<user>/AppData/Local/XCL`).
- **Stop** ends the runner and, if the runner is Wine, runs `wineserver -k` for the prefix,
  because the game is a Windows process that killing the runner alone wouldn't end. Note that
  this ends **every** program running in that prefix.

Building on Linux needs the usual development packages for a windowed app, for example on
Debian/Ubuntu: `libxkbcommon-dev libwayland-dev libx11-dev libgl1-mesa-dev`. File dialogs use the
XDG desktop portal, which most desktops provide.

## Development

```powershell
cargo test                      # unit tests
cargo clippy --all-targets      # lints
cargo fmt                       # formatting
```

| Module | Purpose |
|---|---|
| `config.rs` | The launcher's settings, launch settings and how overrides merge |
| `game.rs` | Reading `MicrosoftGame.config` and scanning library folders |
| `paths.rs` | Where XCL's and the launcher's files are, and Wine path handling |
| `process.rs` | Starting XCL without a console, precompile jobs, stopping |
| `xcl_toml.rs` | Writing settings into a title's `xcl.toml` while keeping the rest of the file |
| `clean.rs` | Finding, measuring and deleting caches |
| `thumbs.rs` | Decoding logos on background threads |
| `app.rs`, `settings_ui.rs` | The user interface ([egui](https://github.com/emilk/egui)) |

The data folder name for each game is computed the way XCL computes it (the manifest's
`Identity Name`, made safe for a file name), so the launcher finds the same folders XCL uses.
