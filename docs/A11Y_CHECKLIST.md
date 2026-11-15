# Accessibility manual-test checklist

Status: **unexecuted**. The AccessKit bridge exists (see the accessibility-tree row in
[COMPATIBILITY.md](COMPATIBILITY.md)), but **no manual assistive-technology pass has been
performed or recorded**. Do not claim support for any screen reader. When a pass is run,
record the date, build, OS/AT versions and results in the table at the bottom; until then
this page is a procedure, not a result.

Test app: any `examples/` app with labelled controls (buttons with text, a text input
with `<label>`, a checkbox). Build it with `rendrix package` (or run with `rendrix run`)
so the native accessibility bridge is active.

## Narrator (Windows)

1. Update Windows; note the build number.
2. Start the app, then start Narrator (`Win+Ctrl+Enter`).
3. `CapsLock+Right/Left`: every button, input and checkbox is announced with its
   visible label and role (button / edit / check box), in visual order.
4. `Enter`/`Space` on a focused button activates it (the app responds; Narrator
   announces the state change if the label changes).
5. Type into the text input: characters and the final value are announced;
   the caret follows typing.
6. Toggle the checkbox: the checked/unchecked state is announced.
7. Record: Narrator version (Settings → Accessibility → Narrator shows the Windows
   build), pass/fail per step, and anything announced as "unknown" or skipped.

## NVDA (Windows)

1. Install the current NVDA release; note the version (`NVDA+N` → Help → About).
2. Start the app with NVDA running.
3. `Tab`/`Shift+Tab`: focus moves through interactive elements in visual order;
   each announces role + name; `NVDA+Tab` re-announces the focused control.
4. Browse mode (`NVDA+Space` toggles): arrow keys walk static text and headings
   in reading order.
5. Activate buttons, edit text, toggle the checkbox as under Narrator.
6. `NVDA+F1` (log viewer) on any mis-announced control; record the object
   properties shown.
7. Record: NVDA version, pass/fail per step.

## VoiceOver (macOS)

1. Note the macOS version; enable VoiceOver (`Cmd+F5`).
2. Start the `.app` bundle produced by `rendrix package` on macOS.
3. `VO+Right/Left` (`Ctrl+Option+Right/Left`): walk the interface; every control
   announces role + label, in visual order.
4. `VO+Space` activates buttons; type in the text input and confirm the value
   and caret are announced; toggle the checkbox and confirm the state change.
5. `VO+F1` details on any control that announces incorrectly.
6. Record: macOS + VoiceOver versions (VoiceOver version follows the OS),
   pass/fail per step. A macOS run has never been performed (see
   [RELEASE_CHECKLIST.md](RELEASE_CHECKLIST.md)).

## Results log

| Date | Build (`rendrix --version` + git sha) | OS / version | AT / version | Steps pass/total | Notes |
|---|---|---|---|---|---|
| — | — | — | — | — | No pass recorded yet |
