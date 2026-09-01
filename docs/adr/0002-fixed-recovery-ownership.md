# Fixed recovery ownership

Status: accepted

Issues #1 and #10 establish an accepted, hard-to-reverse boundary: fixed recovery input and rendering live outside Lua. Recovery retains the selected-path identity, uses an independent physical Fn takeover after exactly two continuous seconds for healthy workers, keeps the healthy worker hidden and alive, and does not restart failed workers automatically. The physical-hold deadline is separate from the two-second Lua callback watchdog and the 500 ms graceful-stop deadline. The safety path cannot depend on Lua because the selected worker may be the thing that failed to load, start, render, or stay alive, so recovery must still own input and display when Lua is unavailable. See issues #1 and #10 for the requirements behind this decision.
