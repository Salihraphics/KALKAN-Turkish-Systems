# Phase 7 — DPI research and controlled integration proposal

*KALKAN — A Novera Labs Product. Engine: KOR — Kernel Operations Runtime.*

> **Status: research and planning only.** This document is a technical
> research note and an integration proposal for a *possible future*,
> independent module. **Nothing described here is implemented.** No code,
> no dependency, no driver and no packet-processing component has been
> added to KALKAN in Phase 7. Any implementation requires a separate,
> explicit decision after this document has been reviewed.

## 1. Purpose and scope

The original specification defines Phase 7 as "DPI research and controlled
integration planning", with these constraints: the architecture must allow
future network-processing modules without rewriting the GUI; DPI behaviour
and compatibility are researched separately; nothing may manipulate packets,
disable security software, bypass antivirus protections or modify system
networking without explicit user control; administrator requirements must be
clear; driver and network API failures must be handled safely.

This document covers:

1. how DPI affects users, and what a local application can and cannot change;
2. a proposed architecture for an independent, optional module;
3. user consent and explicit activation;
4. administrator privileges;
5. driver options, signing and their consequences;
6. failure handling and safe fallback;
7. compatibility with the existing KALKAN/KOR architecture;
8. testing strategy and its limits;
9. risks, open questions and decision gates.

It intentionally stays at the level of architecture and trade-offs. It does
not specify packet-level techniques.

## 2. DPI: behaviour and compatibility considerations

### 2.1 What DPI is

Deep Packet Inspection (DPI) is network equipment, usually operated by an
ISP, an organisation or a regulator, that looks beyond IP addresses and
ports into connection contents or metadata to classify traffic. Depending
on configuration it can block, reset, redirect or slow down connections.

Commonly documented mechanisms, from the least to the most specific:

| Mechanism | What it looks at | Can a local app influence it? |
|---|---|---|
| IP address / route blocking | Destination address | **No.** Traffic to the address is dropped regardless of what the client does locally. |
| DNS-based filtering | DNS queries and answers | Partly: by using a different, encrypted resolver (DNS over HTTPS / TLS), which Windows 11 can configure natively. |
| Hostname inspection in cleartext metadata | e.g. the server name sent at the start of a TLS connection, HTTP Host headers | Sometimes. Publicly documented tools change how this metadata appears on the wire. Effectiveness depends entirely on the DPI implementation. |
| Throttling / traffic shaping | Traffic patterns and classification | Rarely, and not reliably. |
| Protocol-level classification | Statistical or protocol fingerprints | Generally no. |

### 2.2 Compatibility considerations

* **Effectiveness is not portable.** It differs by ISP, region, time and
  DPI vendor, and can change without notice. No result measured on one
  connection says anything about another.
* **Protocol evolution reduces the cleartext surface.** Encrypted DNS and
  Encrypted Client Hello (ECH, where both browser and server support it)
  remove metadata that DPI relies on. A future module must expect its
  relevance to change.
* **Local changes can break things.** Altering how connections look can
  break captive portals, enterprise proxies, VPN clients, parental-control
  or security software, and sites behind certain CDNs.
* **Gaming impact.** Every packet that passes through an extra processing
  step costs latency and can add jitter. Kernel-level network filters are
  also a known source of conflicts with anti-cheat systems, which may
  refuse to start a game or flag the machine.
* **Security software.** Products that intercept or modify traffic are
  frequently flagged as potentially unwanted or malicious by antivirus
  engines, independent of intent.

### 2.3 Legal and policy considerations

Access restrictions are often based on court orders or regulator
decisions. Whether, and under which conditions, circumventing them is
lawful depends on jurisdiction, including Türkiye, and on the specific
case. This document does not give legal advice. **A legal review is a
precondition (gate G1) before any implementation work.** Distribution
channels (code-signing providers, software stores, antivirus vendors) also
have their own policies.

## 3. What KALKAN does today (baseline for this proposal)

* KOR is a **user-mode** engine (threads in `kalkan.exe`). "Kernel" is part
  of its name only.
* Network monitoring is **read-only**: `NetworkMonitor` has a single
  `sample(&mut Snapshot)` method over `GetIfTable2`; there is no API to
  capture, send, block or modify traffic.
* KALKAN needs no administrator rights for monitoring, installs no driver,
  changes no system setting and never touches security software.
* The GUI talks to the engine only through `Command` / `Event`
  (`kalkan-core::protocol`), and the engine has a strict lifecycle state
  machine with bounded shutdown and no orphaned threads.

Any future module must preserve all of these properties for everything that
is not the module itself.

## 4. Proposed integration architecture

### 4.1 Principles

1. **Independent and optional.** A separate component, not part of KOR's
   monitoring path. KALKAN without the module behaves exactly as today.
2. **Off by default, never automatic.** It does nothing until the user
   explicitly enables it, and it is never enabled silently, by an update or
   by a configuration file alone.
3. **Least privilege.** `kalkan.exe` stays unelevated. Only a small,
   separate helper runs with administrator rights, and only while the module
   is active.
4. **Fail open.** If anything goes wrong, the module gets out of the way and
   the user's network keeps working normally.
5. **No persistent system changes** without a separate, explicit and
   reversible user action.

### 4.2 Components (proposal)

```
 kalkan.exe (unelevated, as today)
 ├── kalkan-gui ── Command/Event ──► KOR (kalkan-core)
 │                                   ├── read-only monitor (unchanged)
 │                                   └── module client  ── IPC (named pipe, ACL: current user only)
 │                                                            │
 └────────────────────────────────────────────────────────────┼──────────────
                                                              ▼
 kalkan-module-helper.exe (elevated via UAC, only while active)
 └── backend (one of the options in section 6)
```

* **New crate for the contract** (working name `kalkan-module`): a
  `TrafficModule` trait *separate from* `NetworkMonitor` (which stays
  read-only), module states, capability description and error types.
  Platform independent and unit-testable, like `kalkan-net`.
* **Module client inside KOR:** starts/stops the helper, forwards commands,
  turns helper state into events. It owns no privileges.
* **Elevated helper process:** the only privileged part. Started on demand
  through a UAC prompt, exits when the module is disabled or KALKAN closes.
  Process isolation means a crash in the helper cannot take down the GUI or
  the monitor, and the privileged surface stays small and auditable.
* **IPC:** a local named pipe restricted to the current user's SID, a
  versioned message format, and a version handshake so a mismatched helper
  is refused.

### 4.3 Protocol extensions (proposal)

Additive variants, so existing front ends and tests are unaffected:

* `Command::Module(ModuleCommand)` with `Enable { consent_version }`,
  `Disable`, `QueryModule`.
* `Event::ModuleState { state, message }`, with states such as
  `Unavailable`, `Disabled`, `Starting`, `Active`, `Stopping`, `Failed`.
* `EngineStatus` gains an optional module summary (never measured values
  that do not exist, following the existing honesty rules for displayed
  data).

The module would have **its own state machine**, separate from the engine
lifecycle: the monitor can run with the module disabled, and a module
failure must never move the engine to `Failed`.

### 4.4 Configuration (proposal)

* A new `[module]` section, default `enabled = false`, plus the version of
  the consent text the user accepted.
* **Compatibility note:** every existing section uses
  `deny_unknown_fields`. New keys load fine in new builds (serde defaults
  keep Phase 1–6 files valid), but a file written by a new build would be
  rejected by an older build. Options: bump `schema_version` and document
  it, or read the new section with a more tolerant rule. Decision needed
  before implementation.
* `start_engine_on_launch` must **not** imply enabling the module. A
  separate opt-in would be needed, and is not recommended initially.

## 5. User consent and explicit activation

Proposed flow:

1. The module is listed in Settings as unavailable/disabled, with a short
   neutral description and a link to documentation.
2. **Enable** opens a consent screen in plain language: what the module
   does and does not do; that it requires administrator rights; possible
   effects on games with anti-cheat, on VPNs, on security software and on
   connection reliability; that effectiveness is not guaranteed; that legal
   conditions depend on the user's jurisdiction; how to turn it off.
3. Explicit confirmation (no pre-ticked boxes), followed by the Windows UAC
   prompt for the helper.
4. While active: a persistent, unmissable indicator in the GUI (sidebar and
   Dashboard), and a one-click **Disable** that works even if the helper is
   unresponsive.
5. The accepted consent text version is stored; if the text changes
   materially, consent is asked again.
6. All activations, deactivations and failures are logged locally (the
   existing log system); nothing is sent anywhere.

## 6. Administrator privileges and driver options

### 6.1 Privileges

* Monitoring: unchanged, no administrator rights.
* Module: any form of packet interception or modification on Windows
  requires administrator rights (to load or open a kernel component, or to
  register filtering callouts). Hence the separate elevated helper and the
  UAC prompt on every activation, unless the user later chooses a
  persistent service (not recommended initially).

### 6.2 Backend options

| Option | What it means | Main consequences |
|---|---|---|
| **A. No driver** (Windows Filtering Platform management API from user mode) | Can permit or block traffic by rules | **Cannot inspect or change payloads.** Useful only for allow/block features; not sufficient for DPI-related work. |
| **B. Existing third-party signed driver** (for example WinDivert, used by GoodbyeDPI) | A user-mode library talks to a separately signed kernel driver | No own kernel code; but depends on the upstream project's signing and maintenance; its licence (LGPL/GPL terms) must be checked against KALKAN's MIT licence and distribution model; such drivers are frequently flagged by antivirus and anti-cheat software. |
| **C. Own WFP callout driver** | KALKAN ships its own kernel-mode driver | Full control, but: kernel code (bugs can crash the whole system); an EV code-signing certificate and Microsoft signing (attestation signing via the Partner Center, or HLK/WHQL) are required for normal Windows 10/11 machines; compatibility with Memory Integrity (HVCI) and the vulnerable-driver blocklist; a security review; long-term maintenance cost. It would also contradict the simple message that KALKAN has no kernel component, and would need very clear separation from KOR. |

Notes on signing: since Windows 10 version 1607, kernel drivers on systems
with Secure Boot must be signed through Microsoft. Test signing mode exists
for development only and is not acceptable for users.

**Recommendation for a future decision:** if implementation is ever
approved, evaluate option B first in a lab (gate G2), because it avoids
shipping KALKAN-owned kernel code; treat option C as a separate project with
its own budget and review. Option A alone does not meet the goal.

## 7. Failure handling and safe fallback

| Failure | Required behaviour |
|---|---|
| Helper fails to start, UAC declined, driver missing or blocked | Module stays `Disabled`/`Unavailable` with the real reason shown; the monitor is unaffected. |
| Helper crashes or hangs while active | KOR detects it (pipe closed, missed heartbeat), reports `Failed`, and the network returns to normal. With WFP, filters can be created in a *dynamic session* that Windows removes automatically when the owning process exits, so a crash cannot leave rules behind. Any other backend must provide an equivalent guarantee. |
| KALKAN closes or crashes | The helper exits and its state is removed (same mechanism); on the next start KALKAN checks for and cleans up leftovers before offering activation. |
| Version mismatch between app and helper | Refused at the handshake; clear message. |
| Antivirus quarantines the helper or driver | Detected at start; explained to the user; never worked around. |
| Unexpected connectivity problems | One-click Disable; documented manual recovery steps. |
| Persistent system settings (DNS, proxy) | Not changed by the module. If ever offered, only as a separate explicit action with an automatic undo. |

General rules: **fail open** (never leave the user without connectivity),
bounded timeouts for every helper operation, no automatic retries that
could loop, and every failure logged with its real cause.

## 8. Compatibility with the existing KALKAN/KOR architecture

* **No GUI rewrite:** the GUI already talks only through `Command` /
  `Event`; the module adds variants and a Settings section, and one status
  card. The Phase 2–6 GUI structure (pages, layout, update paths) is
  unchanged.
* **No change to monitoring:** `NetworkMonitor` stays read-only and
  separate; the totals and their counting rules are unaffected, although an
  active module may change the traffic the monitor observes, which the GUI
  should state.
* **Lifecycle:** the module's state machine sits beside the engine
  lifecycle; the existing guarantees (bounded shutdown, no orphaned
  threads) extend to the helper process.
* **Performance:** the Phase 6 tools (`--benchmark`, `--soak`, the
  simulated long-run test) measure the app's own cost; a module would
  additionally need latency and jitter measurements for the traffic it
  touches, which do not exist yet.
* **Dependencies:** the contract crate needs none beyond the workspace's
  existing ones; any backend dependency is a decision for gate G2.
* **Licence and branding:** KALKAN is MIT-licensed; third-party components
  keep their own licences and notices, which must be checked before use.

## 9. Testing strategy and limitations

| Level | What | Where |
|---|---|---|
| Unit tests | Module state machine, consent versioning, configuration compatibility, IPC message format and version handshake | Any OS, in CI (like the existing 171 tests) |
| Engine integration | A **fake module** implementing the trait (like the scripted monitor in `soak_sim.rs`): activation, failure, crash and recovery paths; no leaks, no orphaned threads | Any OS, in CI |
| Helper and IPC | Elevated helper start/stop, pipe ACLs, crash detection, cleanup on next start | Windows, in CI (the runners are administrators) |
| Backend / driver | Loading, filtering, dynamic-session cleanup, HVCI compatibility | **Windows VMs only**, with test-signed or properly signed components; GitHub-hosted runners are not suitable for loading test-signed kernel drivers (test signing needs a boot setting and a reboot) |
| Compatibility matrix | Windows 10/11, Memory Integrity on/off, common antivirus products, VPN clients, enterprise proxies | Lab VMs and volunteer machines |
| Performance | Added latency and jitter per connection, CPU cost of the helper | Lab, with repeatable measurements |

**Limitations to state honestly:**

* Real-world effectiveness depends on ISPs and cannot be tested in CI, and
  results from one network do not generalise.
* Testing against real DPI equipment or real access restrictions may itself
  be subject to legal constraints; this belongs to the legal review.
* Anti-cheat compatibility cannot be tested without running the games
  concerned, which may violate their terms; at most, documented vendor
  policies can be referenced.

## 10. Risks

| Risk | Impact | Mitigation in this proposal |
|---|---|---|
| Legal exposure for the publisher or users | High | Gate G1 legal review before any implementation; neutral wording; no claims of access to specific services |
| Antivirus flagging and reputation damage | High | Separate, clearly documented helper; signing; no obfuscation; explain rather than evade |
| Anti-cheat conflicts (gaming is a core audience) | High | Off by default; clear warning; possible scoping to selected traffic; no persistent service |
| Kernel instability (option C) | High | Prefer option B; option C only as a separate project with review |
| Breaking users' connectivity | Medium | Fail open, dynamic cleanup, one-click disable |
| Licence incompatibility (option B) | Medium | Licence review at gate G2 |
| Maintenance burden as DPI and protocols change | Medium | Treat the module as an optional add-on with its own release cadence |

## 11. Decision gates (proposal)

| Gate | Question | Output |
|---|---|---|
| **G0** | This document reviewed and accepted | Approved research and proposal |
| **G1** | Legal review: is the intended use lawful in the target jurisdictions, and under which conditions? | Written legal assessment; go / no-go |
| **G2** | Backend choice (A/B/C), licence check, signing path, costs | Decision record |
| **G3** | Contract only: `kalkan-module` crate, protocol variants, fake module and tests; no packet processing | Reviewed, tested code |
| **G4** | Lab prototype of the chosen backend in VMs only; latency/jitter measurements | Lab report |
| **G5** | Consent UX, failure handling and compatibility matrix verified | Test report |
| **G6** | Decision on any public release | Release decision |

Every gate requires an explicit approval; none is implied by the previous
one.

## 12. Explicitly out of scope

* Disabling, bypassing or evading antivirus or other security software.
* Hidden or automatic activation, background installation of drivers or
  services, or activation through configuration files alone.
* Persistent changes to system networking without a separate, explicit and
  reversible user action.
* Claims that KALKAN provides access to any particular service or website.
* Any implementation before the gates above are passed.

## 13. Phase 7 result

* Research of DPI behaviour and compatibility, and the integration proposal
  above: **done (this document)**.
* Code, dependencies, drivers, packet processing: **none added**.
* Next step: review of this document (gate G0). No implementation starts
  without a separate approval.
