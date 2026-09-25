# Using the GUI

## Toolbar

**Connect**, then **Fetch**, tick **Auto**, or **Sequence** (arm one acquisition,
wait for it to complete, then pull the curve). **CSV** / **JSON** / **PNG**
save the traces already on the plot (JSON includes measurements and a settings
snapshot; **Wide** writes `t,CH1,CH2,…`). **Cursors**: left-click sets A, right-click
or Shift-click sets B; the bar under the plot shows Δt and 1/Δt.

## View controls

The **View** row controls the plot:

- **Autoscale** refits both axes on every capture. Any manual zoom, scroll, or drag
  switches it off so the view stops jumping; **Fit now** is a one-shot refit.
- **Stack** gives each channel its own band, scaled to that channel's own min and
  max, so a 50 mV ripple is as tall as a 5 V square wave instead of a flat line
  next to it. The y axis then labels bands by channel rather than volts; hover a
  trace to read the real value, and the cursor and measurement readouts under the
  plot stay in volts either way.
- **Zoom axes X / Y** choose which axes zoom. Untick **X** to zoom vertically only —
  a Mac trackpad pinch is uniform, so this is how you get vertical-only zoom.
- **−** / **+** zoom the enabled axes; **−Y** / **+Y** always zoom vertically.
- **Scroll zooms** (default on) makes two-finger scroll zoom the enabled axes.
  Turn it off to pan with scroll instead.
- Drag pans, right-drag is a box zoom, and double-click resets.

## Channel / instrument panel

The left panel reads the current front-panel state and controls. Its choices are
provided by the detected instrument backend, so unsupported settings are hidden
or read-only:

- CH1–CH4 enable, volts/div, position, offset, coupling, 1 MΩ/50 Ω input,
  passive-probe attenuation, and bandwidth
- time/div, horizontal position, and record length
- edge-trigger mode, source, slope, coupling, and level
- acquisition mode, continuous/single-sequence behavior, and run/stop
- Autoset and an advanced raw SCPI query/write console

For example, DHO900 input termination is shown as fixed 1 MΩ and its bandwidth
choices are 20 MHz or the model's full bandwidth. Its memory-depth selector
includes 1k–50M with a reminder that the maximum is 50M for one active channel,
25M for two, and 10M for all four.

Each Apply operation is read back from the instrument so values coerced by the
scope are reflected in the GUI. Physical front-panel changes can be imported with
**Refresh**. **Demo** plots a synthetic sine with no instrument attached.

> **50 Ω caution:** only select 50 Ω input termination when the source voltage is
> safe for the scope's internal terminator. Unlike a passive probe setting, this
> physically changes the input load.

## Rendering notes

Traces are min/max reduced to about two points per pixel column before plotting,
and the reduction relaxes as you zoom in. `egui_plot` transforms and tessellates
every point it is given on every frame with no culling, so handing it two raw
10k-point records made a maximized window redraw too slowly to respond. Peaks
survive the reduction, and measurements and exports always use full resolution.
The bar under the plot shows plotted-versus-captured point counts.

## Preferences & behavior

Host, port, Auto interval, Wide CSV, and Scroll-zooms are remembered in
`~/.config/instrument-viewer/prefs`. `--host` / `--port` on the command line override the
saved address.

Auto captures every 2 seconds by default; the spinner next to it sets the interval
(0.5–30 s). The slow default is deliberate — see [Protocol & quirks](protocol.md).
Auto switches itself off on any error rather than retrying into a struggling
instrument.

While connected and otherwise idle, the toolbar polls acquisition state every
two seconds. Tektronix reports `Acq: RUN`/`STOP`; Rigol also exposes trigger
states such as `TD` and `WAIT`. Polls pause during fetches and settings changes,
never reconnect, and silently back off if the instrument does not answer.
