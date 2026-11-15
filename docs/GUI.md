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
   UI thread:  WM_COMMAND ─► EngineLink::send(Command)
               EngineLink::try_recv() ─► UiState::apply(Event) ─► relayout + invalidate
               WM_PAINT  ─► page_view(page, ui, info) ─► Painter (back buffer → BitBlt)
```

* The GUI depends only on `kalkan-core`. It never sees `kalkan-net` or `kalkan-config`. The app turns config values into display strings before passing them in.
* Phase 2 passes `UnimplementedEngine`. It answers Start and Stop with an `Event::Info` saying nothing happened, and it always reports `Stopped`. In Phase 3 a link to the engine thread will post `WM_APP_ENGINE` to wake the UI. The handler for that message already exists.

## Honesty rules for displayed data

Every value on screen is a `model::Value`:

| Variant | Badge | Meaning |
|---|---|---|
| `Real { source: Engine }` | **LIVE** (green) | Measured by the running engine |
| `Real { source: System }` | **SYSTEM** (green) | Queried from Windows (e.g. privilege level) |
| `Real { source: Config }` | **CONFIG** (green) | Read from `kalkan.toml` |
| `Unavailable { reason }` | **NOT AVAILABLE** (amber) | Shown as "Not available" plus the reason, e.g. "Requires the network monitor (Phase 4)" |

CPU, memory and network cards are `Real` only when an `EngineStatus` contains that number. The unit test `no_engine_data_means_no_real_metrics_anywhere` checks every page.

## Layout

```
┌──────────────┬──────────────────────────────────────────────────┐
│ ◆ KALKAN     │  Page title                                      │
│   v0.1.0     │  subtitle                                        │
│              │ ┌─ notice (Dashboard, after an action) ────────┐ │
│ ▌Dashboard   │ └──────────────────────────────────────────────┘ │
│  Network Mon │ ┌─ ● Stopped ───────────────── [Start] [Stop] ─┐ │
│  Statistics  │ └──────────────────────────────────────────────┘ │
│  Logs        │ ┌ card ─────┐┌ card ─────┐┌ card ─────┐┌ card ───┐ │
│  Settings    │ └───────────┘└───────────┘└───────────┘└─────────┘ │
│              │ ┌ key/value table (Settings) ──────────────────┐ │
│ ● Engine: …  │ └──────────────────────────────────────────────┘ │
│ Standard user│  explanatory note                                │
└──────────────┴──────────────────────────────────────────────────┘
```

* The sidebar is a fixed 208 DIP wide. Content cards reflow to between 1 and 4 columns (minimum card width 200 DIP).
* Minimum client size is 760 × 560 DIP. The layout tests check that every page fits at the minimum size at 100 %, 150 % and 200 % scaling.
* Per-monitor DPI awareness v2: fonts and layout are rebuilt on `WM_DPICHANGED`.

## Controls and input

* Sidebar entries and Start/Stop are real `BUTTON` controls with `BS_OWNERDRAW`. They support Tab/Shift+Tab, Space/Enter (through `IsDialogMessageW`), focus rectangles, and accessible names taken from the button text.
* Start and Stop are enabled from `EngineState::can_start()` and `can_stop()`, the same table the engine uses.
* Settings are read-only in Phase 2. Editing arrives in Phase 5.

## Resource use

* `GetMessageW` blocks, and there are no timers or polling, so the app uses no CPU while idle. The design ensures this, but it has not been measured yet; that is Phase 6.
* The back buffer is created per `WM_PAINT` and freed immediately. Nothing is cached while idle.
* Brushes are created per draw call and released at once. Fonts are created once per DPI.
* The self-test prints the working set, peak working set and private bytes after the scripted run. These numbers are a point-in-time observation, not a benchmark.

## Robustness

* Window state lives in a `RefCell`. A handler returns an `Outcome`, and the borrow is released **before** `DefWindowProcW` or `SetWindowPos` runs. Both re-enter the window procedure: `WM_WINDOWPOSCHANGING` triggers `WM_GETMINMAXINFO`, and `WM_WINDOWPOSCHANGED` triggers `WM_SIZE`. The first version held the borrow there and lost those messages. The CI screenshots exposed that.
* `WM_CLOSE` releases the borrow before `DestroyWindow`, because `WM_NCDESTROY` frees the state.
* Panics are caught at the window-procedure boundary. The window closes, and `run` returns `GuiError::Panicked` instead of unwinding into Windows.
* If the config fails to load during a normal GUI start, a message box shows the problem. It is not printed to an invisible stderr.

## Self-test (`kalkan --self-test --screenshots <dir>`)

The self-test runs on Windows CI against the real window:

1. Checks the initial state: Dashboard is shown, Start is enabled, Stop is disabled.
2. Sends `BM_CLICK` to each sidebar button, checks the page changed and the content fits, and saves a screenshot.
3. Clicks Start and checks for the "Nothing was started" notice, that the state is still Stopped and that Stop is still disabled.
4. Requests a 1×1 window and checks that the minimum size is enforced. Then maximises and restores. After each resize it checks that the layout matches the **actual** client rectangle, so a missed `WM_SIZE` cannot pass, and that the minimum-size grid has 2 columns.
5. Closes the window and checks that the message loop exits with 0 and the window state was released.

Screenshots are uploaded as the `gui-screenshots` CI artifact.

## Not done yet

* No hover effects, because owner-drawn buttons don't get hover state without subclassing.
* No application icon resource yet. The shield is drawn in the sidebar only.
* No scrolling. Content is sized to fit at the minimum window size.
* Dark theme only. The app does not follow the Windows light/dark setting.
* There is no system tray.
* Logs are not shown in the app yet. In GUI mode, log output is not captured anywhere until Phase 5.
