# KALKAN GUI

The GUI is native Win32, painted with GDI. It uses no Electron, WebView2, Tauri or UI framework. The only Windows dependency is the `windows` crate.

## Module structure (`crates/kalkan-gui`)

| Module | Platform | Responsibility | Tests |
|---|---|---|---|
| `model` | any | Pages, `UiState`, the `Value`/`Source` honesty types, what each page shows | unit tests |
| `layout` | any | Pure function: client size + DPI + page content → rectangles. Responsive columns | unit tests |
| `theme` | any | Palette, DIP metrics, type scale, DPI scaling | unit tests |
| `win32` | Windows | Window class, message loop, GDI painting, owner-drawn controls, self-test | `kalkan --self-test` on Windows CI |

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

* The GUI depends only on `kalkan-core`. It never sees `kalkan-net` or `kalkan-config`. The app turns config values into display strings before passing them in.
* The app passes the real engine, `kalkan_core::EngineHandle` (GoodBoyDPI, see [ARCHITECTURE.md](ARCHITECTURE.md)). In `WM_CREATE` the GUI installs a waker. The engine calls it from its own thread, and it posts `WM_APP_ENGINE`. An `AtomicBool` makes a burst of events produce at most one posted message. The flag is cleared *before* draining, so an event that arrives mid-drain triggers a new post.
* The GUI never polls the engine and never waits for it. The displayed engine state is always the last `StateChanged` or `Status` the engine sent. The GUI never sets it on its own.
* Periodic `Status` events (one per sample interval while Running) repaint only when the Dashboard is visible.

## Honesty rules for displayed data

Every value on screen is a `model::Value`:

| Variant | Badge | Meaning |
|---|---|---|
| `Real { source: Engine }` | **LIVE** (green) | Measured by the running engine. Today that is KALKAN's own CPU and working set |
| `Real { source: System }` | **SYSTEM** (green) | Queried from Windows (e.g. privilege level) |
| `Real { source: Config }` | **CONFIG** (green) | Read from `kalkan.toml` |
| `Unavailable { reason }` | **NOT AVAILABLE** (amber) | Shown as "Not available" plus the reason, e.g. "Requires the network monitor (Phase 4)" |

CPU, memory and network cards are `Real` only when an `EngineStatus` contains that number. The unit test `no_engine_data_means_no_real_metrics_anywhere` checks every page. When the engine leaves `Running`, the metrics are cleared and the cards show "Measured while the engine is running". Network cards stay NOT AVAILABLE until the Phase 4 monitor exists.

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
* **Icons** are 16-DIP line icons drawn with GDI pens (dashboard, network, statistics, logs, settings, download, upload, CPU, memory, shield, interface, clock, peak, average, file). There are no image resources. The window and taskbar icon is the KALKAN shield, rendered with `CreateIconIndirect` at start-up.
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
* The engine panel always shows the identity **GoodBoyDPI — Rust Core Engine** and the scope line "Lifecycle and process metrics only. No DPI circumvention or packet processing." The scope line spans the full panel width below the buttons, so it stays readable at the minimum window size. A layout test checks this.
* Settings are read-only. Editing arrives in Phase 5.

## Resource use

* `GetMessageW` blocks, and the GUI has no periodic timers, so the UI uses no CPU while idle. While the engine is Running, the UI wakes once per sample interval (default 1 s) to show new metrics. While Stopped, nothing wakes it. This follows from the design. It has not been measured yet; that is Phase 6.
* The only timer is a one-shot timer (7 s) armed when the window is closing. It bounds the wait for the engine's shutdown confirmation.
* The back buffer is created per `WM_PAINT` and freed immediately. Nothing is cached while idle.
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
* The window icon is generated at runtime. There is no `.ico` resource embedded in the executable, so Explorer shows the default program icon for `kalkan.exe`.
* Dark theme only. The app does not follow the Windows light/dark setting.
* There is no system tray.
* Logs are not shown in the app yet. GUI runs write to `%LOCALAPPDATA%\Kalkan\logs\kalkan.log`, and the Logs page shows that path. The in-app viewer is Phase 5.
* CPU and memory show KALKAN's own process, not the whole system.
* The graphs auto-scale with a 1 KB/s floor and have no axis labels; they show the trend, not exact values.
