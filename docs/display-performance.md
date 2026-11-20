# Display performance

Open the VM viewer and expand its scaling menu, labeled **Fit image** by
default. Enable **Performance overlay**. The setting applies to the current
viewer window. The same item turns it off.

| Metric | What Hydroxl measures |
| --- | --- |
| Live FPS | Guest image updates drawn in the last second |
| Average FPS | Guest image updates per second over up to the last 10 seconds |
| 1% low | FPS equivalent of the mean duration of the slowest 1% of frame intervals in that history |

The readings update four times a second. Hydroxl counts changing guest
textures drawn by the viewer. Repainting the same texture, moving the host
pointer, and refreshing the overlay contribute no guest frames. A static
desktop can read 0 FPS. A dash means there is insufficient sample data.
Pause, reconnect, and guest resolution changes restart the sample.

## Run a repeatable 60 FPS check

1. Set the host display and guest to at least 60 Hz. Use the same guest
   resolution, viewer scaling, graphics backend, and transport for each
   comparison. Start with 1920 × 1080 or the resolution you expect to use.
2. Run a continuously moving workload inside the guest. Use the same file,
   browser, and zoom each time. For a cross-platform baseline, save this
   as `motion.html` and open it in the guest browser in full screen.

   ```html
   <!doctype html>
   <meta charset="utf-8">
   <title>Display motion check</title>
   <style>
     body { margin: 0; overflow: hidden; background: #152032; }
     main { width: 100vw; height: 100vh; display: grid; place-items: center; }
     div {
       width: 60vw;
       height: 55vh;
       background: repeating-linear-gradient(90deg, #eaf2ff 0 4px, #152032 4px 24px);
       animation: sweep 2s linear infinite alternate;
     }
     @keyframes sweep { from { transform: translateX(-18vw); } to { transform: translateX(18vw); } }
   </style>
   <main><div></div></main>
   ```

3. Keep the viewer visible and avoid resizing it during the run. Enable the
   overlay, wait 10 seconds for the rolling history to fill, then watch the
   readings for 30 seconds. Record the average and the lowest sustained
   1% low you see. Repeat three times.
4. Aim for live and average FPS around 60. A 60 FPS cadence gives each frame
   about 16.7 ms. A 1% low close to 60 indicates even delivery. A much lower
   1% low, such as 30, indicates recurring frame gaps even when the average
   stays near 60.
5. Repeat with the applications or animations you actually run in the guest.
   Record the VM's graphics model, acceleration setting, display transport,
   and resolution with each result.

The overlay samples the viewer's paint path. Guest rendering, QEMU capture,
transport, host rendering, and compositor refresh can all affect what you
see. The readings depend on the workload and host and do not guarantee 60 FPS
for every VM. [SPICE display pipeline notes](spice-2d-tearing.md)
