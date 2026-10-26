# Fleet flicker probe

This opt-in script records changes to selected Workspaces DOM text and node identities. It does not record pixels, backend events, or application store state. No probe code runs until you load it in Web Inspector.

1. Run CLAI from this checkout in development mode with `npm run tauri:dev`. In the CLAI window, press **Ctrl+Shift+I** (Linux/Windows), or right-click and choose **Inspect Element**, then select **Console**. The current release build does not enable Web Inspector.
2. Paste this single command into the Console before reproducing the flicker:

   ```js
   document.head.append(Object.assign(document.createElement('script'), { src: '/diagnostics/fleet-flicker-probe.js?run=' + Date.now() }))
   ```

   A `CLAI flicker` row confirms it loaded. The timestamp makes the command repeatable; a new run stops the previous probe.
3. Start a screen recording, answer a pending `ask_user` question, and navigate between workspaces inside the app while the flicker is visible. Do not reload the window before exporting: a reload clears the probe and its rows. Keep the recording and Console capture from the same run.
4. Immediately run `__claiFlicker.stop()`, then run `JSON.stringify(__claiFlicker.rows)` and copy the returned JSON value from Console. Send it with the recording and the approximate time of the visible flicker.

A changed text value or node ID during the flicker supports a DOM/state path. Stable sampled fields during visible flicker motivate a paint/compositing investigation; they do not establish a WebKitGTK defect. The probe samples every 100 ms and observes DOM mutations, so it can miss brief changes outside its selected fields.
