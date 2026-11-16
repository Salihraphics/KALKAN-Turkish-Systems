# KALKAN GUI

*KALKAN — A Novera Labs Product. Engine: KOR — Kernel Operations Runtime (legacy name GoodBoyDPI; see [ARCHITECTURE.md](ARCHITECTURE.md) for the naming status).*

The GUI is native Win32, painted with GDI. It uses no Electron, WebView2, Tauri or UI framework. The only Windows dependency is the `windows` crate.

## Module structure (`crates/kalkan-gui`)

| Module | Platform | Responsibility | Tests |
|---|---|---|---|
| `model` | any | Pages, `UiState`, the `Value`/`Source` honesty types, what each page shows | unit tests |
| `layout` | any | Pure function: client size + DPI + page content → rectangles. Responsive columns | unit tests |
| `theme` | any | Palette, DIP metrics, type scale, DPI scaling | unit tests |
| `logview` | any | Log viewer state: level chips, search, pause, selection, export text | unit tests |
| `settings` | any | Settings form: fields, parsing, validation through `Config::validate`, dirty/revert/defaults, save messages | unit tests |
| `brand` | any | Embedded `kalkan.ico`: directory parser and size picker | unit tests |
| `win32` (+ `win32_paint`, `win32_pages`) | Windows | Window class, message loop, GDI painting, owner-drawn controls, Logs/Settings controls, I/O thread, self-test | `kalkan --self-test` on Windows CI |

`#![deny(unsafe_code)]` applies to the whole crate. Only `win32` is allowed `unsafe`, and every `unsafe` block carries a `SAFETY:` comment (enforced by clippy).

## Data flow

```
             AppInfo (real start-up facts: version, privileges, config)
                │
kalkan-app ─────┼──► kalkan_gui::run(info, Box<dyn EngineLink>, opts)
                │
                ▼
   UI thread:  WM_COMMAND    ─► EngineLink::send(Command)            (never blocks)
               WM_APP_ENGINE ─► EngineLink::try_recv() loop ─► UiState::apply(Event)
                                ─► relayout + invalidate
               WM_PAINT      ─► page_view(page, ui, info) ─► Painter (back buffer → BitBlt)

   engine threads ──waker()──► PostMessageW(hwnd, WM_APP_ENGINE)   (coalesced by an atomic flag)
```

* The GUI depends on `kalkan-core`, and since Phase 5 on `kalkan-config` (Settings validation and saving) and `kalkan-logging` (viewer buffer, runtime level and rotation). It never sees `kalkan-net` or `kalkan-win`. See [ARCHITECTURE.md](ARCHITECTURE.md) for the reason.
* Phase 5 adds two more posted messages: `WM_APP_LOG` (new log entries; coalesced like `WM_APP_ENGINE`) and `WM_APP_IO` (a save/export finished on the `kalkan-io` thread). If one arrives while the window state is borrowed (for example during a modal dialog), it is re-posted rather than dropped.
* The app passes the real engine, `kalkan_core::EngineHandle` (KOR, see [ARCHITECTURE.md](ARCHITECTURE.md)). In `WM_CREATE` the GUI installs a waker. The engine calls it from its own thread, and it posts `WM_APP_ENGINE`. An `AtomicBool` makes a burst of events produce at most one posted message. The flag is cleared *before* draining, so an event that arrives mid-drain triggers a new post.
* The GUI never polls the engine and never waits for it. The displayed engine state is always the last `StateChanged` or `Status` the engine sent. The GUI never sets it on its own.
* Periodic `Status` events (one per sample interval while Running) repaint only when a live page (Dashboard, Network Monitoring, Statistics) is visible.

## Honesty rules for displayed data

Every value on screen is a `model::Value`:

| Variant | Badge | Meaning |
|---|---|---|
| `Real { source: Engine }` | **LIVE** (green) | Measured by the running engine. Today that is KALKAN's own CPU and working set |
| `Real { source: System }` | **SYSTEM** (green) | Queried from Windows (e.g. privilege level) |
| `Real { source: Config }` | **CONFIG** (green) | Read from `kalkan.toml` |
| `Unavailable { reason }` | **NOT AVAILABLE** (amber) | Shown as "Not available" plus the reason, e.g. "Requires the network monitor (Phase 4)" |

CPU, memory and network cards are `Real` only when an `EngineStatus` contains that number. The unit test `no_engine_data_means_no_real_metrics_anywhere` checks every page. When the engine leaves `Running`, the metrics are cleared and the cards show "Measured while the engine is running". Network cards are NOT AVAILABLE until the engine's network monitor reports data.

## Design system

All colours and sizes come from `theme.rs`. There are no gradients, shadows or animations.

| Token | Value | Use |
|---|---|---|
| `PRIMARY` / `_HOVER` / `_PRESSED` / `_SOFT` | `#14B8A6` / `#2DD4BF` / `#0F9488` / `#102B2A` | Brand teal: Start, active navigation, download series, info notices |
| `ON_PRIMARY` | `#041F1C` | Label on teal (dark text, 6.9:1; white on teal would fail AA) |
| `BG` / `SIDEBAR` / `CARD` / `CARD_HOVER` | `#0F1216` / `#0A0C0F` / `#161A20` / `#1D222A` | Surfaces |
| `BORDER` / `BORDER_STRONG` | `#262C35` / `#3A4350` | 1 px outlines |
| `TEXT` / `TEXT_MUTED` / `TEXT_DISABLED` | `#E8EBEF` / `#8B95A3` / `#4E5662` | Text hierarchy; "Not available" uses the disabled colour |
| `SUCCESS` / `WARNING` / `ERROR` (+ `_SOFT`) | `#34D399` / `#FBBF24` / `#F87171` | Status pills, badges, notices |
| `UPLOAD` | `#60A5FA` | Upload series, kept distinct from the teal download series |

* **Contrast.** `theme::tests::text_contrast_meets_wcag` checks WCAG AA (4.5:1) for every text/surface pair the GUI uses. The lowest pair is muted text on the teal-tinted surface, at 4.94.
* **Icons** are 16-DIP line icons drawn with GDI pens (dashboard, network, statistics, logs, settings, download, upload, CPU, memory, shield, interface, clock, peak, average, file, search).
* **Logo.** `assets/kalkan.ico` (16–256 px PNG entries, made from the supplied logo with its outer background made transparent) is compiled in with `include_bytes!`. The window and taskbar icons, the sidebar mark (32 DIP) and the engine panel mark (16 DIP) are created from it with `CreateIconFromResourceEx`, choosing the smallest entry at least as large as needed (32/48/64 px are exact at 100/150/200 %), and drawn with `DrawIconEx`. They are recreated on `WM_DPICHANGED` and destroyed with the window. If loading fails, the drawn shield is used and a warning is logged.
* **Components:**
  - **Card**: icon, title, source badge, value and detail.
  - **Tile**: a large value with a 60-sample area graph. It is drawn from a fixed-size ring buffer into a stack `POINT` array, with no per-frame allocation.
  - **Strip**: labelled values separated by dividers.
  - **Table**: columns with minimum widths, weights and priorities. Low-priority columns are hidden when space is short. Rows that don't fit become "+N more".
  - **State pill**: a soft background and a haloed dot.
  - **Buttons** have hover, pressed, disabled and focus states, and show play/stop glyphs.
* **Hover feedback.** The owner-drawn buttons are subclassed in the classic way (`GWLP_WNDPROC`, with the original procedure kept in `GWLP_USERDATA`). `TrackMouseEvent` provides leave notifications, and the cursor becomes a hand. No comctl32 v6 manifest is needed.

## Layout

```
┌──────────────┬──────────────────────────────────────────────────┐
│ ◆ KALKAN     │  Page title / subtitle                           │
│   v0.1.0     │ ┌─ notice (Dashboard) ─────────────────────────┐ │
│ ▣ Dashboard  │ ┌─ ◆ GoodBoyDPI — Rust Core Engine  [▶Start][■Stop]┐
│ ⇅ Network    │ │  (● Running)  Running for 01:23              │ │
│ ▥ Statistics │ │  Lifecycle and process metrics only. No DPI…  │ │
│ ☰ Logs       │ ├──────────────────────────────────────────────┤ │
│ ≡ Settings   │ │ tiles (Network)   ┌ Download ┐ ┌ Upload ┐    │ │
│              │ │ cards (2–4 cols)  ┌ card ┐┌ card ┐┌ card ┐   │ │
│ ● Engine: …  │ │ strip             Active interface │ … │ …   │ │
│ Administrator│ │ table             interface rows / settings   │ │
└──────────────┴──────────────────────────────────────────────────┘
```

* The sidebar is 208 DIP wide. Tiles and cards reflow into a balanced grid (4 cards → 2×2, 6 → 3×2) with a minimum width of 200 DIP each.
* The minimum client size is **760 × 600 DIP**. Layout tests check that every page fits at the minimum size at 96, 120, 144, 168 and 192 DPI, that no blocks overlap, and that the sidebar border stays visible.
* Per-monitor DPI awareness v2: fonts and layout are rebuilt on `WM_DPICHANGED`.

## Controls and input

* Sidebar entries and Start/Stop are real `BUTTON` controls with `BS_OWNERDRAW`. They support Tab/Shift+Tab, Space/Enter (through `IsDialogMessageW`), focus rectangles, and accessible names taken from the button text.
* Start and Stop are enabled from `EngineState::can_start()` and `can_stop()`, the same table the engine uses:

  | Engine state | Start | Stop |
  |---|---|---|
  | Stopped, Failed | enabled | disabled |
  | Starting | disabled | enabled (cancels the start) |
  | Running | disabled | enabled |
  | Stopping | disabled | disabled |
  | App closing (any state) | disabled | disabled |

  A duplicate command that gets past the buttons (for example, sent directly) is rejected by the engine. It shows an amber warning notice and the state does not change.
* Errors from the engine (`EngineError`) appear as a red notice with the engine's real message. A successful Start or Stop clears old notices.
* The engine panel always shows the engine identity, currently still the legacy title **GoodBoyDPI — Rust Core Engine** (`kalkan_core::ENGINE_TITLE`; renaming it to KOR is planned, not done), and the scope line "Lifecycle and process metrics only. No DPI circumvention or packet processing." (`ENGINE_SCOPE`; the wording predates read-only network monitoring and understates what the engine reads; updating it belongs with the planned rename) The scope line spans the full panel width below the buttons, so it stays readable at the minimum window size. A layout test checks this.
* Keyboard: Tab/Shift+Tab reach every Logs and Settings control (search box, chips, list, Pause/Copy/Export, level segments, number boxes, switches, Save/Revert/Reset). Ctrl+C in the log list copies the selected entry.

## Logs page (Phase 5)

* **Real data only.** Rows come from the `tracing` events KALKAN actually emitted (`LogBuffer`), plus the previous run's file tail marked as history (muted). Nothing is generated for display.
* **Toolbar:** a search box (case-insensitive; matches level, thread, target and message) and four level chips with live counts: ERROR, WARN, INFO, DEBUG (DEBUG includes TRACE). Header actions: **Pause/Resume**, **Copy** (selected entry), **Export**.
* **List:** a virtual owner-drawn `LISTBOX` (`LBS_NODATA`), so 5 000 entries cost one count, not 5 000 strings. Each row: UTC time, level pill (colour per level), target (hidden when narrow), message with ellipsis. Error rows have a red bar. While scrolled to the end, the list follows new entries; scrolled up, it stays where it is. The selection is kept across refreshes.
* **Refresh** is event-driven: the buffer's waker posts `WM_APP_LOG`, and the page re-filters only if the buffer version changed and the view is not paused. There is no timer. While paused, new entries are kept and appear on Resume.
* **Status line:** the real log file path, size, archive count and policy ("rotates at 5 MB, keeps 7"), the last file error if any, entries shown of total, evicted count, and damaged history lines skipped. A console run says "No log file".
* **Export** opens the standard Save As dialog (`GetSaveFileNameW`), then writes the visible entries on the `kalkan-io` thread and reports the result (path and count, or the error) in the notice bar.
* **Empty and error states:** "No entries yet", "No entries match the search or level filter", history load errors and file errors are shown in the page, not hidden.

## Settings page (Phase 5)

* **Status card:** the config path and state: LOADED, DEFAULTS (no file yet; Save creates it) or INVALID (the loader's error; defaults are in use and the file is untouched until Save).
* **Fields** (each has a label, a short help line and, when invalid, the validation message in red in place of the help):

  | Setting | Control | Key | Applies |
  |---|---|---|---|
  | Log level | 5 segments | `logging.level` | immediately |
  | Maximum log file size (MB, 1–100) | number box | `logging.max_file_size_mb` | immediately |
  | Log archives to keep (1–90) | number box | `logging.max_log_files` | immediately (extra archives pruned) |
  | Write logs to a file | switch | `logging.file_logging` | next launch |
  | Sample interval (ms, 250–60 000) | number box | `monitor.sample_interval_ms` | next engine start |
  | Start engine on launch | switch | `general.start_engine_on_launch` | next launch |
  | Start minimized | switch | `ui.start_minimized` | next launch |

  Every field maps to an existing, implemented config key; no setting was added without a real effect.
* **Save** validates the whole form with `Config::validate` (the loader's rules). Errors stop the save, are shown next to each field, and summarised in a red notice; nothing is written. A valid form is saved atomically on the I/O thread; when the previous file was invalid, it is first copied to `kalkan.toml.bak`. The notice confirms the save and names settings that apply later.
* **Revert** restores the last saved values. **Reset defaults** asks for confirmation (`MessageBoxW`, Yes/No), then saves the defaults and applies them.
* Save is enabled when the form differs from the saved configuration (or the file is invalid), Revert when the form differs, Reset defaults when the saved or edited values are not already the defaults. All three are disabled while a save is running. Copy needs a selected row; Export needs at least one visible entry.

## Resource use

* `GetMessageW` blocks, and the GUI has no periodic timers, so the UI uses no CPU while idle. While the engine is Running, the UI wakes once per sample interval (default 1 s) to show new metrics. While Stopped, nothing wakes it. Measured in Phase 6: see [PERFORMANCE.md](PERFORMANCE.md).
* **Update paths (Phase 6).** A *structural* change (engine state, button state, notice, page, size, DPI, a layout change) runs the full `relayout`: view, layout, every control moved/shown/enabled, the whole window and its children repainted. A *metrics-only* status (numbers and graphs) takes `refresh_metrics`: the view and layout are rebuilt; if the layout is unchanged, no control is moved, enabled or repainted and the sidebar is not redrawn. *Validated on Windows CI (run 36, commit `06e2997`):* owner-drawn control repaints 5.05 → 0.03 per second and relayouts 1.00 → 0 per second during monitoring.
  * *Local, not yet run on Windows:* the fast path invalidates only the blocks that show live data (engine panel, tiles, cards, strip, table), and `paint` also skips the page header, notice and explanatory note when they are outside the update region. It will be kept only if the next Windows run shows a measurable gain (the soak's same-process A/B of the old and new update path measures it).
* The Logs page repaints only when the viewer changed (or the page area, while paused, so the file status stays live).
* The periodic status is logged at Trace, not Debug (about 2 KB of text per second with 6 adapters).
* The only timer is a one-shot timer (7 s) armed when the window is closing. It bounds the wait for the engine's shutdown confirmation.
* The back buffer is created per `WM_PAINT` and freed immediately. Nothing is cached while idle. Caching it was considered and rejected in Phase 6: it is 0.04 ms of a 2.3 ms frame (1.6 %).
* Brushes are created per draw call and released at once. Fonts are created once per DPI.
* The self-test prints the working set, peak working set and private bytes after the scripted run. These numbers are a point-in-time observation, not a benchmark.

## Robustness

* Window state lives in a `RefCell`. A handler returns an `Outcome`, and the borrow is released **before** `DefWindowProcW` or `SetWindowPos` runs. Both re-enter the window procedure: `WM_WINDOWPOSCHANGING` triggers `WM_GETMINMAXINFO`, and `WM_WINDOWPOSCHANGED` triggers `WM_SIZE`. The first version held the borrow there and lost those messages. The CI screenshots exposed that.
* **Closing is a two-step process.**
  1. The first `WM_CLOSE` sends `Shutdown`, disables both buttons and shows "Shutting down…". It does not block.
  2. When `ShutdownComplete` arrives, the GUI posts another `WM_CLOSE`, which destroys the window.
  3. If no confirmation arrives within 7 s, the close goes ahead and an error is logged.
  4. Destroying the window drops the `EngineHandle`, which joins the controller thread. The engine has already exited by then.
* `WM_CLOSE` releases the borrow before `DestroyWindow`, because `WM_NCDESTROY` frees the state.
* Panics are caught at the window-procedure boundary. The window closes, and `run` returns `GuiError::Panicked` instead of unwinding into Windows.
* If the config fails to load during a normal GUI start, a message box shows the problem. It is not printed to an invisible stderr.

## Self-test (`kalkan --self-test --screenshots <dir>`)

In Phase 5 the self-test has **63 checks**, plus a 64th (the `.bak` check) when it starts from an invalid file. CI run 20: 63/63, 63/63 and 64/64 passed. Phase 5 adds these checks, using the real window, engine and logger:

* **Logs:** the page shows real engine entries ("engine started"); the list box row count equals the filtered entries; an upper-case search finds entries case-insensitively; turning INFO and DEBUG chips off hides them; a paused view does not change while a marker is logged, and Resume shows it; Copy puts exactly the selected entry on the clipboard; Export writes exactly the text that was visible when it ran, off the UI thread.
* **Settings:** the configuration state is recorded; an out-of-range value ("0" MB) is rejected with an explanation and the file is byte-for-byte unchanged; a valid save (12 MB, debug level) is on disk and the log level changes live; Reset to defaults writes the defaults and re-applies the Info level. When the run starts from an invalid file, the `.bak` copy must exist (CI run 3 starts from `sample_interval_ms = 5`).
* **Logo:** the embedded logo loaded for the sidebar, the panel and both window icons.

The modal dialogs themselves (Save As, the Reset confirmation) are not clicked by the self-test; it calls the code paths behind them.

In Phase 4 the self-test has **47 checks**, and CI runs it 3 times per build. On top of the Phase 3 steps it now:

* waits for the **real** network monitor and requires at least one counted interface, then requires rates computed from two real samples;
* checks that the Dashboard shows live download, upload and active interface, that the Network table lists every interface, and that Statistics shows live session totals;
* measures **process CPU over 5 s** with the live Network page repainting, inside a real blocking message loop. The bound is < 15 % of one core, to catch busy loops. Measured: 0.31 %;
* **paints every page in every engine state, with every notice kind, at 96 and 144 DPI, synchronously**, plus hover in/out on every button (214 frames). This step was added after a paint-path crash that occurred only when a `WM_PAINT` landed in the few milliseconds of the `Stopping` state. The cause was an empty string handed to `DrawTextW` as a dangling zero-length slice;
* simulates moving to a 150 % monitor and back (`WM_DPICHANGED`), checks that the layout matches the real client area, and saves a screenshot.

The Phase 3 steps are listed below.

The self-test drives the real window **and the real engine** on Windows CI. It makes 37 checks, and CI runs it 3 times per build:

1. Initial state: Dashboard is shown, the engine reports Stopped, Start is enabled and Stop is disabled.
2. `BM_CLICK` on each sidebar button. It checks that the page changed and the content fits, and saves a screenshot.
3. **Start** (real button). It waits for the engine to report Running, checks that `EngineStarted` arrived once and the buttons flipped, then waits for live CPU and memory samples. It also checks that no network values are reported.
4. **Duplicate Start**, sent directly to the engine. It checks for the amber "already running" warning and that the state is unchanged.
5. **Stop** (real button). It waits for `EngineStopped`, then checks that the worker thread terminated (only the controller is alive), the metrics were cleared and the buttons flipped back.
6. **Duplicate Stop.** It checks for the "not running" warning.
7. **10 Start/Stop cycles** through the real buttons. It checks the event counts (11 started, 11 stopped, 0 errors) and that no worker thread is left.
8. Resize checks: the minimum size is enforced, and after the minimum, maximise and restore steps the layout matches the actual client rectangle.
9. **Close while running.** It starts the engine and posts `WM_CLOSE`. After the message loop ends it checks that the engine confirmed shutdown before the window closed, the window state was released, and `live_engine_threads() == 0`.

Screenshots are uploaded as the `gui-screenshots` CI artifact.

## Not done yet

* No scrolling. Long interface lists end in "+N more"; enlarge the window to see everything.
* No interface selection. The Network page shows an overview of all interfaces, and the totals follow the counting rules.
* Hover feedback is limited to the buttons (sidebar, Start, Stop). Cards and table rows are not interactive.
* The logo is used for the window and taskbar, but the executable has no icon *resource* (no `.rc` build step), so Explorer still shows the default program icon for `kalkan.exe` itself.
* Dark theme only. The app does not follow the Windows light/dark setting.
* There is no system tray.
* Log timestamps are UTC (as in the file), not local time.
* The log list uses the classic Win32 scroll bar, which is not themed dark.
* Switches and level segments are owner-drawn buttons: screen readers get their names but not their on/off state.
* CPU and memory show KALKAN's own process, not the whole system.
* The graphs auto-scale with a 1 KB/s floor and have no axis labels; they show the trend, not exact values.
